use crate::app::session::SessionEvent;
use crate::cluster::NamespaceListError;
use crate::store::events::EventRow;
use crate::store::watch::StoreId;
use crossterm::event::Event as CtEvent;
use indexmap::IndexMap;
use kube::api::GroupVersionKind;
use std::collections::HashSet;

/// Health of a single kind's watch. Shown in the status bar so stale data is
/// never presented as live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStatus {
    Initialising,
    Synced,
    Reconnecting,
    Failed,
}

/// Everything that can wake the event loop.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Input(CtEvent),
    StoreChanged {
        gvk: GroupVersionKind,
    },
    WatchStatus {
        gvk: GroupVersionKind,
        status: WatchStatus,
    },
    /// Progress of a cluster switch. Carries no data the store owns — the
    /// registry is the source of truth; this only says something changed.
    Session(SessionEvent),
    /// The result of fetching namespaces from the API, in answer to the
    /// namespace picker opening. Fetching is I/O, so it runs on a spawned
    /// task (see `cluster::namespaces::list_namespaces`) and arrives back
    /// through this channel rather than blocking the draw that opens the
    /// picker.
    ///
    /// `store` identifies the `SharedStore` that was current when the fetch
    /// was spawned. A cluster switch or a namespace change replaces the
    /// store wholesale (`switch_cluster`/`restart_watch`) without cancelling
    /// an in-flight listing fetch, so without this a stale prod listing can
    /// reinstall itself into `Session::namespaces_from_api` after the very
    /// switch that cleared it. The caller compares this against the
    /// CURRENTLY active store before applying, inside the same lock
    /// acquisition it would write under anyway — the same idiom
    /// `spawn_discovery_and_watches` already uses via `Arc::ptr_eq`.
    NamespacesListed {
        store: StoreId,
        result: Result<Vec<String>, NamespaceListError>,
    },
    /// The answer to one `store::events::fetch_events` call, for the detail
    /// pane's Events tab. See `FetchedEvents` for why it carries the identity
    /// it was fetched for rather than just the rows.
    EventsFetched(FetchedEvents),
    /// Discovery finished for the cluster on screen and has written its kinds
    /// to the session. Carries nothing on purpose: the session is the source
    /// of truth for what was found, exactly as `Session(..)` carries nothing
    /// the registry owns — this only says "something changed, redraw".
    KindsDiscovered,
    /// A debounce armed for a Table refetch has elapsed; re-evaluate whether
    /// one is due (`store::table::refetch_is_due`).
    ///
    /// Deliberately NOT a `StoreChanged`: a wake that marked the store dirty
    /// would arm another debounce, and the two would trade wakes forever at
    /// the debounce interval — a timer loop, which is exactly what the
    /// watch-triggered design exists to avoid. A `Wake` arms no further
    /// debounce, so a quiet cluster settles back to zero events and zero CPU.
    Wake,
    Error(String),
    Quit,
}

/// One completed events fetch, tagged with the object it was fetched FOR.
///
/// Carrying kind, namespace and name — not just the rows — is what makes a
/// same-cluster stale reply harmless: the detail pane can be re-pointed at
/// another object, or closed, while a fetch is in flight, and a reply that
/// carried only rows would be applied to whatever happens to be open when it
/// lands. That identity alone does NOT make misattribution unrepresentable
/// ACROSS a cluster switch or namespace change, though: a StatefulSet in one
/// cluster and a Pod in another routinely share a namespace and a name, and
/// `switch_cluster`/`restart_watch` do not cancel an in-flight fetch. `store`
/// closes that gap — it identifies the `SharedStore` the fetch was spawned
/// against, and the caller compares it against `OpenDetail`'s own (which a
/// store mismatch closes outright; see `main.rs`) before applying anything,
/// so a reply from a superseded store cannot land under a same-named object
/// in the one that replaced it.
#[derive(Debug, Clone)]
pub struct FetchedEvents {
    pub gvk: GroupVersionKind,
    pub namespace: Option<String>,
    pub name: String,
    pub store: StoreId,
    /// `String`, not `anyhow::Error`: `AppEvent` is `Clone` and `Debug` and
    /// `anyhow::Error` is neither.
    pub result: Result<Vec<EventRow>, String>,
}

/// The result of collapsing a batch of events into a single render's worth of work.
#[derive(Debug, Default)]
pub struct Coalesced {
    pub inputs: Vec<CtEvent>,
    /// Every kind a `StoreChanged` named in this batch, deduplicated.
    ///
    /// A flat `store_dirty: bool` (what this replaced) cannot distinguish a
    /// delta on the kind the table is showing from one on `coordination.k8s.
    /// io/Lease` or `core/Event` — both just set the same flag, so ANY of
    /// the (now up to 40) eagerly-watched kinds forced a full frame repaint.
    /// Deciding whether that matters needs the gvk, which only the caller
    /// (via `active_kind`, not known here) can compare against — see
    /// `main.rs`'s `table_body_is_dirty`.
    pub changed_kinds: HashSet<GroupVersionKind>,
    pub status_changes: Vec<(GroupVersionKind, WatchStatus)>,
    pub errors: Vec<String>,
    /// Kept in order and never dropped: a `Connecting` collapsed into the
    /// `Connected` behind it would lose the only frame that says the app is
    /// waiting on a cluster that takes tens of seconds to answer.
    pub session_events: Vec<SessionEvent>,
    /// The most recent namespace-listing result seen in this batch, if any,
    /// alongside the store it was fetched against (see
    /// `AppEvent::NamespacesListed`'s doc comment). Only ever one fetch is in
    /// flight at a time in practice, but if the picker were opened, closed
    /// and reopened fast enough to queue two results in one batch, the newer
    /// one is what the picker should show — same reasoning as
    /// `status_changes` keeping only the latest per kind.
    pub namespace_list: Option<(StoreId, Result<Vec<String>, NamespaceListError>)>,
    /// Every events fetch that completed in this batch, in arrival order and
    /// never dropped. Unlike `namespace_list`, keeping only the newest would
    /// be wrong: each reply is scoped to a DIFFERENT object, and the caller —
    /// not this function — is the only thing that knows which object the pane
    /// is currently open on. Collapsing here would discard the one reply that
    /// matches in favour of one that does not.
    pub events_fetched: Vec<FetchedEvents>,
    /// True if discovery reported new kinds in this batch. A flag, not a
    /// list: the kinds themselves live on the session.
    pub kinds_discovered: bool,
    /// True if a debounce wake arrived. Collapses like `changed_kinds` — ten
    /// wakes and one wake both mean "re-evaluate once".
    pub wake: bool,
    pub quit: bool,
}

/// Collapse a drained batch into one render's work.
///
/// Store changes coalesce to a set of the kinds that changed: 10,000 deltas
/// on one kind cost one entry, and the caller decides what that set means for
/// a redraw (see `changed_kinds`'s doc comment). Input is never coalesced —
/// dropping keystrokes is always wrong. Errors are never dropped. Only the
/// newest status per kind is kept.
pub fn coalesce(events: Vec<AppEvent>) -> Coalesced {
    let mut out = Coalesced::default();
    let mut statuses: IndexMap<GroupVersionKind, WatchStatus> = IndexMap::new();

    for event in events {
        match event {
            AppEvent::Input(e) => out.inputs.push(e),
            AppEvent::StoreChanged { gvk } => {
                out.changed_kinds.insert(gvk);
            }
            AppEvent::WatchStatus { gvk, status } => {
                statuses.insert(gvk, status);
            }
            AppEvent::Session(s) => out.session_events.push(s),
            AppEvent::NamespacesListed { store, result } => {
                out.namespace_list = Some((store, result))
            }
            AppEvent::EventsFetched(f) => out.events_fetched.push(f),
            AppEvent::KindsDiscovered => out.kinds_discovered = true,
            AppEvent::Wake => out.wake = true,
            AppEvent::Error(e) => out.errors.push(e),
            AppEvent::Quit => out.quit = true,
        }
    }

    out.status_changes = statuses.into_iter().collect();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::watch::ResourceStore;
    use crossterm::event::{
        Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
    };
    use kube::api::GroupVersionKind;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    /// A distinct `SharedStore` for identity purposes only — `StoreId::of`
    /// compares allocations, not contents, so an empty store is as good as a
    /// populated one for telling two "sessions" apart in a test.
    fn a_store() -> crate::store::watch::SharedStore {
        Arc::new(RwLock::new(ResourceStore::new()))
    }

    fn key(c: char) -> CtEvent {
        CtEvent::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn many_store_changes_collapse_to_one_changed_kind() {
        let events: Vec<AppEvent> = (0..10_000)
            .map(|_| AppEvent::StoreChanged { gvk: pod_gvk() })
            .collect();
        let out = coalesce(events);
        assert_eq!(
            out.changed_kinds,
            [pod_gvk()].into_iter().collect(),
            "10,000 deltas on one kind must collapse to one entry"
        );
        assert!(out.inputs.is_empty());
        assert!(!out.quit);
    }

    #[test]
    fn input_events_are_preserved_in_order_and_never_dropped() {
        let events = vec![
            AppEvent::Input(key('a')),
            AppEvent::StoreChanged { gvk: pod_gvk() },
            AppEvent::Input(key('b')),
        ];
        let out = coalesce(events);
        assert_eq!(out.inputs.len(), 2, "input must never be coalesced away");
        assert_eq!(out.inputs[0], key('a'));
        assert_eq!(out.inputs[1], key('b'));
        assert!(out.changed_kinds.contains(&pod_gvk()));
    }

    #[test]
    fn quit_is_sticky() {
        let out = coalesce(vec![
            AppEvent::Quit,
            AppEvent::StoreChanged { gvk: pod_gvk() },
        ]);
        assert!(out.quit);
    }

    #[test]
    fn latest_status_per_gvk_wins() {
        let out = coalesce(vec![
            AppEvent::WatchStatus {
                gvk: pod_gvk(),
                status: WatchStatus::Initialising,
            },
            AppEvent::WatchStatus {
                gvk: pod_gvk(),
                status: WatchStatus::Synced,
            },
        ]);
        assert_eq!(
            out.status_changes.len(),
            1,
            "only the newest status per kind matters"
        );
        assert_eq!(out.status_changes[0].1, WatchStatus::Synced);
    }

    #[test]
    fn session_progress_is_kept_in_order_and_never_coalesced() {
        // Connecting and Connected are the same "kind" of event; collapsing to
        // the newest would erase the only frame that reports the wait.
        use crate::cluster::ClusterId;
        let id = ClusterId("prod".into());
        let out = coalesce(vec![
            AppEvent::Session(SessionEvent::Connecting(id.clone())),
            AppEvent::StoreChanged { gvk: pod_gvk() },
            AppEvent::Session(SessionEvent::Connected(id.clone())),
        ]);
        assert_eq!(
            out.session_events,
            vec![
                SessionEvent::Connecting(id.clone()),
                SessionEvent::Connected(id),
            ]
        );
    }

    #[test]
    fn the_latest_namespace_listing_result_wins() {
        // Mirrors `latest_status_per_gvk_wins`: at most one fetch is
        // normally in flight, but if the picker is reopened fast enough to
        // queue two results in the same batch, the newer one is what the
        // picker should actually show.
        use crate::cluster::NamespaceListError;
        let store = StoreId::of(&a_store());
        let out = coalesce(vec![
            AppEvent::NamespacesListed {
                store: store.clone(),
                result: Err(NamespaceListError::Forbidden("stale".to_string())),
            },
            AppEvent::NamespacesListed {
                store: store.clone(),
                result: Ok(vec!["alpha".to_string(), "beta".to_string()]),
            },
        ]);
        assert_eq!(
            out.namespace_list,
            Some((store, Ok(vec!["alpha".to_string(), "beta".to_string()]))),
            "the newer fetch result must win, not the stale forbidden one"
        );
    }

    #[test]
    fn a_batch_with_no_namespace_listing_event_reports_none() {
        let out = coalesce(vec![AppEvent::StoreChanged { gvk: pod_gvk() }]);
        assert_eq!(
            out.namespace_list, None,
            "unrelated events must not manufacture a listing result"
        );
    }

    // --- Task 10: wiring ---

    fn fetched(name: &str, reason: &str) -> FetchedEvents {
        FetchedEvents {
            gvk: pod_gvk(),
            namespace: Some("demo".to_string()),
            name: name.to_string(),
            store: StoreId::of(&a_store()),
            result: Ok(vec![EventRow {
                kind: "Warning".to_string(),
                reason: reason.to_string(),
                message: String::new(),
                age: "1m".to_string(),
                count: 1,
            }]),
        }
    }

    #[test]
    fn events_fetches_for_different_objects_are_all_kept() {
        // The one place "keep only the newest" would be actively wrong.
        // Each reply is scoped to a DIFFERENT object, and only the caller
        // knows which object the pane is open on — so a batch carrying a
        // reply for `web-a` followed by one for `web-b` must still contain
        // `web-a`'s, or opening the pane on `web-a` and receiving both in one
        // batch would show it as having no events at all.
        let out = coalesce(vec![
            AppEvent::EventsFetched(fetched("web-a", "Scheduled")),
            AppEvent::EventsFetched(fetched("web-b", "FailedScheduling")),
        ]);
        let names: Vec<&str> = out.events_fetched.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["web-a", "web-b"],
            "an events reply for one object must not be collapsed away by a \
             reply for a different one"
        );
    }

    #[test]
    fn a_batch_with_no_events_fetch_carries_none() {
        let out = coalesce(vec![AppEvent::StoreChanged { gvk: pod_gvk() }]);
        assert!(out.events_fetched.is_empty());
        assert!(!out.kinds_discovered);
        assert!(!out.wake);
    }

    #[test]
    fn discovery_and_debounce_wakes_collapse_to_flags() {
        let out = coalesce(vec![
            AppEvent::KindsDiscovered,
            AppEvent::Wake,
            AppEvent::Wake,
            AppEvent::KindsDiscovered,
        ]);
        assert!(out.kinds_discovered);
        assert!(out.wake);
        assert!(
            out.changed_kinds.is_empty(),
            "a debounce wake must not mark any kind changed: that would arm \
             another debounce and the two would trade wakes forever"
        );
    }

    #[test]
    fn errors_are_all_retained() {
        let out = coalesce(vec![
            AppEvent::Error("first".into()),
            AppEvent::Error("second".into()),
        ]);
        assert_eq!(
            out.errors,
            vec!["first".to_string(), "second".to_string()],
            "errors must never be silently swallowed"
        );
    }
}
