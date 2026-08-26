use crate::app::session::SessionEvent;
use crossterm::event::Event as CtEvent;
use indexmap::IndexMap;
use kube::api::GroupVersionKind;

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
    Error(String),
    Quit,
}

/// The result of collapsing a batch of events into a single render's worth of work.
#[derive(Debug, Default)]
pub struct Coalesced {
    pub inputs: Vec<CtEvent>,
    pub store_dirty: bool,
    pub status_changes: Vec<(GroupVersionKind, WatchStatus)>,
    pub errors: Vec<String>,
    /// Kept in order and never dropped: a `Connecting` collapsed into the
    /// `Connected` behind it would lose the only frame that says the app is
    /// waiting on a cluster that takes tens of seconds to answer.
    pub session_events: Vec<SessionEvent>,
    pub quit: bool,
}

/// Collapse a drained batch into one render's work.
///
/// Store changes coalesce to a single dirty flag: 10,000 deltas cost one repaint.
/// Input is never coalesced — dropping keystrokes is always wrong. Errors are
/// never dropped. Only the newest status per kind is kept.
pub fn coalesce(events: Vec<AppEvent>) -> Coalesced {
    let mut out = Coalesced::default();
    let mut statuses: IndexMap<GroupVersionKind, WatchStatus> = IndexMap::new();

    for event in events {
        match event {
            AppEvent::Input(e) => out.inputs.push(e),
            AppEvent::StoreChanged { .. } => out.store_dirty = true,
            AppEvent::WatchStatus { gvk, status } => {
                statuses.insert(gvk, status);
            }
            AppEvent::Session(s) => out.session_events.push(s),
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
    use crossterm::event::{
        Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
    };
    use kube::api::GroupVersionKind;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
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
    fn many_store_changes_collapse_to_one_dirty_flag() {
        let events: Vec<AppEvent> = (0..10_000)
            .map(|_| AppEvent::StoreChanged { gvk: pod_gvk() })
            .collect();
        let out = coalesce(events);
        assert!(out.store_dirty, "store changes must mark the view dirty");
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
        assert!(out.store_dirty);
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
