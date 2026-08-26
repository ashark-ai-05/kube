use crate::app::event::{AppEvent, WatchStatus};
use crate::store::cache::KindCache;
use crate::store::multi::{KindAvailability, availability_of};
use crate::store::rbac::{WatchFailure, classify};
use crate::store::table::TableData;
use futures::{Stream, StreamExt};
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube::runtime::watcher;
use kube::{Api, Client};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// All cached kinds plus their watch health.
pub struct ResourceStore {
    kinds: HashMap<GroupVersionKind, KindCache>,
    statuses: HashMap<GroupVersionKind, WatchStatus>,
    /// Per-kind sidebar availability, alongside `statuses` rather than behind
    /// a second channel: the sidebar reads counts and availability from one
    /// store snapshot under one lock, so the two can never disagree about a
    /// kind the way a status string and a separately-derived guess could.
    availability: HashMap<GroupVersionKind, KindAvailability>,
    /// Server-rendered columns and rows for a kind, once a `fetch_table`
    /// (`store::table::fetch_table`) has completed for it.
    ///
    /// Lives here, beside `statuses`/`availability`, under the SAME lock and
    /// keyed by the SAME `GroupVersionKind`, for the reason documented on
    /// `availability` above: the render path reads objects, status,
    /// availability AND now table columns from one store snapshot under one
    /// lock acquisition, so none of them can ever be read for a DIFFERENT
    /// kind than the others. A side cell holding only "the current table",
    /// keyed by nothing but "whatever is active right now", would go stale
    /// the instant the user switches kinds faster than an in-flight fetch
    /// returns — the returning fetch would land tagged as belonging to
    /// whichever kind is active BY THEN, not the one it was actually
    /// requested for. Keying by `GroupVersionKind`, the same key every other
    /// per-kind fact in this store uses, makes that misattribution
    /// unrepresentable: a stale fetch for a kind the user has since left
    /// simply updates that kind's (unread) entry.
    tables: HashMap<GroupVersionKind, TableData>,
    /// When this kind's watch last changed anything, and when a Table fetch
    /// was last issued for it — the two inputs `store::table::refetch_is_due`
    /// needs.
    ///
    /// Here rather than in the event loop for a reason the loop could not
    /// enforce itself: both are only meaningful relative to the data in THIS
    /// store, and a cluster switch or a namespace change replaces the store
    /// wholesale (`switch_cluster` step 5, `restart_watch`). Kept in the loop
    /// they would have to be cleared by hand at both of those points, and a
    /// carried-over `last_fetch` would suppress the very first refetch on the
    /// new cluster — a table showing the old cluster's columns until
    /// something happens to change. Living in the store makes them start
    /// empty by construction, the same reasoning `tables` itself documents.
    last_change: HashMap<GroupVersionKind, Instant>,
    last_table_fetch: HashMap<GroupVersionKind, Instant>,
}

impl Default for ResourceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceStore {
    pub fn new() -> Self {
        Self {
            kinds: HashMap::new(),
            statuses: HashMap::new(),
            availability: HashMap::new(),
            tables: HashMap::new(),
            last_change: HashMap::new(),
            last_table_fetch: HashMap::new(),
        }
    }

    pub fn apply(
        &mut self,
        gvk: &GroupVersionKind,
        resource: &ApiResource,
        event: watcher::Event<DynamicObject>,
    ) {
        self.kinds
            .entry(gvk.clone())
            .or_insert_with(|| KindCache::new(resource.clone()))
            .apply(event);
        // Recorded here, at the single point every delta for every kind
        // passes through, rather than at the watch loop's call sites: a
        // second place to remember to stamp this is a second place to forget
        // to, and a missed stamp shows up as a Table that quietly stops
        // refreshing for one kind.
        self.last_change.insert(gvk.clone(), Instant::now());
    }

    pub fn objects(&self, gvk: &GroupVersionKind) -> Vec<Arc<DynamicObject>> {
        self.kinds.get(gvk).map(|c| c.objects()).unwrap_or_default()
    }

    /// How many objects of this kind are cached.
    ///
    /// Not `objects(gvk).len()`: the sidebar needs a count for EVERY
    /// discovered kind on every frame, and `objects` clones a `Vec` of `Arc`s
    /// to produce one. Forty kinds over a few hundred objects each would be
    /// tens of thousands of refcount bumps per repaint, for a number the
    /// cache already knows — and would break the O(viewport) render budget on
    /// exactly the clusters where it matters.
    pub fn count(&self, gvk: &GroupVersionKind) -> usize {
        self.kinds.get(gvk).map(|c| c.len()).unwrap_or(0)
    }

    /// When this kind's watch last delivered anything into the store, or
    /// `None` if it never has. One of the two inputs to
    /// `store::table::refetch_is_due`.
    pub fn last_change(&self, gvk: &GroupVersionKind) -> Option<Instant> {
        self.last_change.get(gvk).copied()
    }

    /// Record that a Table fetch was ISSUED for this kind.
    ///
    /// Issue time, not completion time: recorded on completion, every wake
    /// arriving while a fetch is in flight would see a stale `last_fetch` and
    /// issue another, so one slow request on a busy namespace becomes a pile
    /// of concurrent identical GETs. Recording at issue is conservative in
    /// the safe direction — anything that changes while the request is in
    /// flight updates `last_change` past this value and re-arms the refetch
    /// normally.
    pub fn note_table_fetch(&mut self, gvk: GroupVersionKind, at: Instant) {
        self.last_table_fetch.insert(gvk, at);
    }

    pub fn last_table_fetch(&self, gvk: &GroupVersionKind) -> Option<Instant> {
        self.last_table_fetch.get(gvk).copied()
    }

    pub fn set_status(&mut self, gvk: GroupVersionKind, status: WatchStatus) {
        self.statuses.insert(gvk, status);
    }

    pub fn status(&self, gvk: &GroupVersionKind) -> WatchStatus {
        self.statuses
            .get(gvk)
            .copied()
            .unwrap_or(WatchStatus::Initialising)
    }

    pub fn set_availability(&mut self, gvk: GroupVersionKind, availability: KindAvailability) {
        self.availability.insert(gvk, availability);
    }

    /// Defaults to `Watching` for a kind with no recorded entry, matching how
    /// `status` defaults to `Initialising`: absence means "nothing permanent
    /// has been observed yet", not "broken".
    pub fn availability(&self, gvk: &GroupVersionKind) -> KindAvailability {
        self.availability
            .get(gvk)
            .cloned()
            .unwrap_or(KindAvailability::Watching)
    }

    pub fn set_table_data(&mut self, gvk: GroupVersionKind, table: TableData) {
        self.tables.insert(gvk, table);
    }

    /// `None` until a fetch for this kind has completed. The render path
    /// must treat that exactly like any other kind with no data yet —
    /// falling back to the builtin column registry (`store::columns::
    /// column_source`) rather than blocking or showing nothing.
    pub fn table_data(&self, gvk: &GroupVersionKind) -> Option<TableData> {
        self.tables.get(gvk).cloned()
    }
}

pub type SharedStore = Arc<RwLock<ResourceStore>>;

/// Threshold: a watch that fails this many times in a row is likely RBAC denial,
/// not a transient network blip. Escalate to Failed so the UI can show "unavailable"
/// instead of permanent "reconnecting".
const FAILURE_ESCALATION_THRESHOLD: u32 = 3;

/// Status for a watch that has failed `consecutive_errors` times in a row.
pub fn status_for_failure_count(consecutive_errors: u32) -> WatchStatus {
    if consecutive_errors >= FAILURE_ESCALATION_THRESHOLD {
        WatchStatus::Failed
    } else {
        WatchStatus::Reconnecting
    }
}

/// Build the status-bar message for a 403 that will never resolve on its own.
///
/// Leads with the remedy, not the apiserver's own text: this string passes
/// through `truncate_error`'s ~200-char budget (`main.rs`) before it ever
/// reaches the screen, and truncation cuts the *tail* of the string. Whatever
/// is useful has to be at the front, or the one truncated-away detail is the
/// action the user actually needed.
///
/// The remedy differs by scope: cluster-scope watches can retry narrower
/// (`-n <namespace>` or the `n` picker); a watch that was already
/// namespace-scoped and still got denied has no such escape hatch — the
/// user lacks access to that namespace, full stop.
///
/// "press n to pick one" was, before `cluster::namespaces` existed, a claim
/// the picker could not always back up: it was built only from namespaces
/// already seen in loaded objects, so on exactly the cluster this message
/// fires for (0 pods loaded, forbidden at cluster scope) it opened empty.
/// It is checked true again now for both of the picker's own failure modes —
/// when listing namespaces is itself forbidden the picker says so and still
/// accepts a typed name (`main.rs`'s `resolve_confirm`), which needs no
/// listing permission at all — so the wording did not need to change, only
/// the picker underneath it.
pub fn forbidden_message(kind_plural: &str, namespace: Option<&str>, detail: &str) -> String {
    match namespace {
        None => format!(
            "{kind_plural} forbidden at cluster scope — try -n <namespace>, or press n to pick one: {detail}"
        ),
        Some(ns) => format!(
            "{kind_plural} forbidden in namespace {ns} — you don't have access to this namespace either: {detail}"
        ),
    }
}

/// Build the status-bar message for a kind that no longer exists on the
/// cluster (most commonly: its CRD was uninstalled mid-session).
pub fn not_found_message(kind_plural: &str, detail: &str) -> String {
    format!("{kind_plural} not found — it may have been removed from the cluster: {detail}")
}

/// Drive one watch stream into the store, emitting an event after each delta.
///
/// `watcher` already handles relist-on-410-Gone internally, so most of this
/// loop only has to translate errors into visible status rather than
/// reconnect by hand. The one case it must handle itself: a 403/404 wrapped
/// inside a `watcher::Error` is not something retrying will ever fix (see
/// `crate::store::rbac`), so those stop the loop instead of looping forever.
///
/// Generic over the stream type (rather than taking `Api`/`Client` directly)
/// so tests can drive it with a canned sequence of events/errors without a
/// cluster.
async fn drive_watch<S>(
    mut stream: S,
    gvk: GroupVersionKind,
    ar: ApiResource,
    namespace: Option<String>,
    store: SharedStore,
    tx: UnboundedSender<AppEvent>,
) where
    S: Stream<Item = watcher::Result<watcher::Event<DynamicObject>>> + Unpin,
{
    // A watch that keeps failing is usually RBAC denial, not a network
    // blip. Escalating after a few consecutive errors lets the UI say
    // "unavailable" instead of lying with a permanent "reconnecting".
    let mut consecutive_errors: u32 = 0;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                consecutive_errors = 0;
                let synced = matches!(event, watcher::Event::InitDone | watcher::Event::Apply(_));
                store.write().await.apply(&gvk, &ar, event);
                if synced {
                    store
                        .write()
                        .await
                        .set_status(gvk.clone(), WatchStatus::Synced);
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status: WatchStatus::Synced,
                    });
                }
                let _ = tx.send(AppEvent::StoreChanged { gvk: gvk.clone() });
            }
            Err(e) => match classify(&e) {
                ref failure @ WatchFailure::Forbidden { ref detail } => {
                    let msg = forbidden_message(&ar.plural, namespace.as_deref(), detail);
                    {
                        // One critical section for both: the sidebar reads
                        // status and availability from the same store
                        // snapshot, so they must never be set as two
                        // separately-locked writes that a reader could
                        // observe half-applied.
                        let mut s = store.write().await;
                        s.set_status(gvk.clone(), WatchStatus::Failed);
                        s.set_availability(gvk.clone(), availability_of(failure));
                    }
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status: WatchStatus::Failed,
                    });
                    let _ = tx.send(AppEvent::Error(msg));
                    // Permanent: no retry will ever succeed for this
                    // identity. Return rather than `break`, so we skip the
                    // "stream ended unexpectedly" epilogue below — that
                    // message is for a watch that died, not one we stopped
                    // on purpose, and it would bury the actionable one.
                    return;
                }
                ref failure @ WatchFailure::NotFound { ref detail } => {
                    let msg = not_found_message(&ar.plural, detail);
                    {
                        let mut s = store.write().await;
                        s.set_status(gvk.clone(), WatchStatus::Failed);
                        s.set_availability(gvk.clone(), availability_of(failure));
                    }
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status: WatchStatus::Failed,
                    });
                    let _ = tx.send(AppEvent::Error(msg));
                    return;
                }
                WatchFailure::Retryable => {
                    consecutive_errors += 1;
                    let status = status_for_failure_count(consecutive_errors);
                    store.write().await.set_status(gvk.clone(), status);
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status,
                    });
                    // `safe_source_text`, not `{e}`. A retryable watch failure
                    // is the one error path that fires repeatedly, and exec
                    // credentials refresh lazily per request — so a plugin
                    // that prints an `ExecCredential` and then exits non-zero
                    // surfaces here as `watcher::Error::WatchStartFailed(
                    // kube::Error::Service(Box<AuthError>))`, whose `Display`
                    // is the plugin's stdout in plaintext. See
                    // `cluster::redact`.
                    let _ = tx.send(AppEvent::Error(format!(
                        "watch {}: {}",
                        ar.kind,
                        crate::cluster::safe_source_text(&e)
                    )));
                }
            },
        }
    }

    // The watcher is designed to be infinite; if the stream ends, the watch
    // is dead and the view is now stale. Say so rather than leaving the last
    // status showing as if it were live.
    store
        .write()
        .await
        .set_status(gvk.clone(), WatchStatus::Failed);
    let _ = tx.send(AppEvent::WatchStatus {
        gvk: gvk.clone(),
        status: WatchStatus::Failed,
    });
    let _ = tx.send(AppEvent::Error(format!(
        "watch {} ended unexpectedly",
        ar.kind
    )));
}

pub fn spawn_watch(
    client: Client,
    ar: ApiResource,
    namespace: Option<String>,
    store: SharedStore,
    tx: UnboundedSender<AppEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let gvk = GroupVersionKind::gvk(&ar.group, &ar.version, &ar.kind);
        let api: Api<DynamicObject> = match namespace.as_deref() {
            Some(ns) => Api::namespaced_with(client, ns, &ar),
            None => Api::all_with(client, &ar),
        };

        let stream = watcher::watcher(api, watcher::Config::default());
        futures::pin_mut!(stream);
        drive_watch(stream, gvk, ar, namespace, store, tx).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::core::Status;
    use kube::runtime::watcher;
    use tokio::sync::mpsc;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("default")
    }

    fn forbidden_error(message: &str) -> watcher::Error {
        watcher::Error::InitialListFailed(kube::Error::Api(Box::new(Status {
            code: 403,
            reason: "Forbidden".to_string(),
            message: message.to_string(),
            ..Default::default()
        })))
    }

    /// A retryable watch failure whose cause is a credential plugin that
    /// printed an `ExecCredential` and then exited non-zero — the shape a
    /// lazy per-request exec refresh produces. Classified `Retryable` (it is
    /// not a 403/404 `Status`), so it takes the `format!` path that used to
    /// print the error verbatim.
    fn credential_leak_error(token: &str) -> watcher::Error {
        use std::os::unix::process::ExitStatusExt;
        let out = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: format!(r#"{{"kind":"ExecCredential","status":{{"token":"{token}"}}}}"#)
                .into_bytes(),
            stderr: Vec::new(),
        };
        watcher::Error::WatchStartFailed(kube::Error::Service(Box::new(
            kube::client::AuthError::AuthExecRun {
                cmd: "\"kubelogin\" \"get-token\"".to_string(),
                status: out.status,
                out,
            },
        )))
    }

    fn not_found_error(message: &str) -> watcher::Error {
        watcher::Error::InitialListFailed(kube::Error::Api(Box::new(Status {
            code: 404,
            reason: "NotFound".to_string(),
            message: message.to_string(),
            ..Default::default()
        })))
    }

    #[test]
    fn store_routes_events_to_the_right_kind() {
        let mut store = ResourceStore::new();
        let ar = ApiResource::erase::<Pod>(&());
        store.apply(&pod_gvk(), &ar, watcher::Event::Apply(pod("a")));
        assert_eq!(store.objects(&pod_gvk()).len(), 1);
    }

    #[test]
    fn unknown_kind_returns_empty_not_panic() {
        let store = ResourceStore::new();
        let unknown = GroupVersionKind::gvk("apps", "v1", "Deployment");
        assert!(store.objects(&unknown).is_empty());
    }

    // --- Task 10: counts and refetch bookkeeping ---

    #[test]
    fn counting_a_kind_agrees_with_listing_it_without_cloning_the_list() {
        // The sidebar needs a count per kind per frame; `objects().len()`
        // would clone a Vec of Arcs for every one of them.
        let mut store = ResourceStore::new();
        let ar = ApiResource::erase::<Pod>(&());
        for n in ["a", "b", "c"] {
            store.apply(&pod_gvk(), &ar, watcher::Event::Apply(pod(n)));
        }
        store.apply(&pod_gvk(), &ar, watcher::Event::Delete(pod("b")));
        assert_eq!(
            store.count(&pod_gvk()),
            store.objects(&pod_gvk()).len(),
            "the cheap count must agree with the expensive one"
        );
        assert_eq!(store.count(&pod_gvk()), 2);
        assert_eq!(
            store.count(&GroupVersionKind::gvk("apps", "v1", "Deployment")),
            0,
            "a kind nothing has been applied for counts zero, not a panic"
        );
    }

    #[test]
    fn a_watch_delta_records_when_this_kind_last_changed() {
        // `refetch_is_due` needs this instant; without it, a Table fetch has
        // nothing to debounce against and either never fires or fires on
        // every delta.
        let mut store = ResourceStore::new();
        let ar = ApiResource::erase::<Pod>(&());
        assert_eq!(
            store.last_change(&pod_gvk()),
            None,
            "nothing has happened for this kind yet"
        );

        let before = Instant::now();
        store.apply(&pod_gvk(), &ar, watcher::Event::Apply(pod("a")));
        let after = Instant::now();

        let at = store
            .last_change(&pod_gvk())
            .expect("a delta must record when it landed");
        assert!(
            at >= before && at <= after,
            "the recorded instant must be the one the delta actually landed at"
        );
        assert_eq!(
            store.last_change(&GroupVersionKind::gvk("apps", "v1", "Deployment")),
            None,
            "one kind's delta says nothing about another's"
        );
    }

    #[test]
    fn a_table_fetch_is_recorded_per_kind() {
        let mut store = ResourceStore::new();
        let deploy = GroupVersionKind::gvk("apps", "v1", "Deployment");
        let at = Instant::now();
        assert_eq!(store.last_table_fetch(&pod_gvk()), None);
        store.note_table_fetch(pod_gvk(), at);
        assert_eq!(store.last_table_fetch(&pod_gvk()), Some(at));
        assert_eq!(
            store.last_table_fetch(&deploy),
            None,
            "fetching one kind's table must not suppress another's first fetch"
        );
    }

    #[test]
    fn a_fresh_store_carries_no_fetch_or_change_history() {
        // What makes a cluster switch safe: `switch_cluster` replaces the
        // store, so the new cluster starts with nothing suppressing its very
        // first Table fetch.
        let mut old = ResourceStore::new();
        old.apply(
            &pod_gvk(),
            &ApiResource::erase::<Pod>(&()),
            watcher::Event::Apply(pod("a")),
        );
        old.note_table_fetch(pod_gvk(), Instant::now());
        assert!(old.last_table_fetch(&pod_gvk()).is_some());

        let fresh = ResourceStore::new();
        assert_eq!(fresh.last_table_fetch(&pod_gvk()), None);
        assert_eq!(fresh.last_change(&pod_gvk()), None);
    }

    #[test]
    fn status_defaults_to_initialising() {
        let store = ResourceStore::new();
        assert_eq!(store.status(&pod_gvk()), WatchStatus::Initialising);
    }

    #[test]
    fn status_is_recorded_per_kind() {
        let mut store = ResourceStore::new();
        store.set_status(pod_gvk(), WatchStatus::Synced);
        assert_eq!(store.status(&pod_gvk()), WatchStatus::Synced);
        let other = GroupVersionKind::gvk("apps", "v1", "Deployment");
        assert_eq!(
            store.status(&other),
            WatchStatus::Initialising,
            "one kind's health must not mask another's"
        );
    }

    #[test]
    fn objects_are_isolated_per_kind() {
        let mut store = ResourceStore::new();
        let pod_ar = ApiResource::erase::<Pod>(&());
        let deploy_gvk = GroupVersionKind::gvk("apps", "v1", "Deployment");
        let deploy_ar = ApiResource::from_gvk(&deploy_gvk);

        store.apply(&pod_gvk(), &pod_ar, watcher::Event::Apply(pod("p1")));
        store.apply(&pod_gvk(), &pod_ar, watcher::Event::Apply(pod("p2")));
        store.apply(
            &deploy_gvk,
            &deploy_ar,
            watcher::Event::Apply(DynamicObject::new("d1", &deploy_ar).within("default")),
        );

        assert_eq!(
            store.objects(&pod_gvk()).len(),
            2,
            "pods must not include deployments"
        );
        assert_eq!(
            store.objects(&deploy_gvk).len(),
            1,
            "deployments must not include pods"
        );
    }

    #[test]
    fn transient_errors_report_reconnecting_but_repeated_ones_report_failed() {
        assert_eq!(status_for_failure_count(1), WatchStatus::Reconnecting);
        assert_eq!(status_for_failure_count(2), WatchStatus::Reconnecting);
        assert_eq!(
            status_for_failure_count(3),
            WatchStatus::Failed,
            "a persistently failing watch is usually RBAC denial, not a blip"
        );
        assert_eq!(status_for_failure_count(99), WatchStatus::Failed);
    }

    // --- forbidden_message / not_found_message content ---

    #[test]
    fn cluster_scope_forbidden_message_leads_with_the_namespace_remedy() {
        let msg = forbidden_message("pods", None, "pods is forbidden: access denied");
        assert!(
            msg.starts_with("pods forbidden at cluster scope"),
            "the remedy must lead, not trail, so truncation can't eat it; got {msg}"
        );
        assert!(msg.contains("-n <namespace>"), "got {msg}");
        assert!(
            msg.contains("access denied"),
            "the apiserver's own reason should still be included; got {msg}"
        );
    }

    #[test]
    fn the_cluster_scope_remedy_actually_offers_pressing_n() {
        // This message tells the user to press `n`. That claim is only
        // honest if doing so offers something — before `cluster::namespaces`
        // existed, the picker was built solely from namespaces already seen
        // in loaded objects, so on exactly this cluster (0 pods loaded,
        // forbidden at cluster scope) it opened empty. Pinning the exact
        // wording here so a future edit to this string is forced to reckon
        // with whether the picker it points at can still back it up.
        let msg = forbidden_message("pods", None, "pods is forbidden");
        assert!(msg.contains("press n to pick one"), "got {msg}");
    }

    #[test]
    fn namespace_scoped_forbidden_message_does_not_suggest_a_flag_already_used() {
        // Negative case for the test above: the user already scoped to a
        // namespace and still got denied, so re-suggesting `-n` would be
        // nonsensical. A wrong implementation that always emits the
        // cluster-scope remedy would pass the cluster-scope test above but
        // fail this one.
        let msg = forbidden_message("pods", Some("payments"), "pods is forbidden");
        assert!(
            !msg.contains("-n <namespace>"),
            "already namespace-scoped — suggesting -n again is not actionable; got {msg}"
        );
        assert!(msg.contains("payments"), "got {msg}");
    }

    #[test]
    fn not_found_message_mentions_the_kind_is_gone() {
        let msg = not_found_message("widgets", "widgets.example.com not found");
        assert!(msg.contains("widgets"), "got {msg}");
    }

    // --- drive_watch: permanent failures stop the loop, transient ones don't ---

    fn test_store() -> SharedStore {
        Arc::new(RwLock::new(ResourceStore::new()))
    }

    #[tokio::test]
    async fn a_forbidden_error_stops_the_watch_and_marks_it_failed() {
        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, _rx) = mpsc::unbounded_channel();

        // A wrong "doesn't break" implementation would go on to apply the
        // event that follows the forbidden error; asserting the store never
        // saw it is what actually proves the loop stopped, not just that it
        // eventually reports Failed (which the old 3-strikes escalation
        // already did).
        let events = vec![
            Err(forbidden_error("pods is forbidden")),
            Ok(watcher::Event::Apply(pod("should-never-be-applied"))),
        ];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);

        drive_watch(s, gvk.clone(), ar, None, store.clone(), tx).await;

        assert_eq!(store.read().await.status(&gvk), WatchStatus::Failed);
        assert!(
            store.read().await.objects(&gvk).is_empty(),
            "the event after a permanent failure must never reach the store"
        );
    }

    // --- per-kind availability lives in the store, alongside status ---

    #[test]
    fn availability_defaults_to_watching_for_an_unknown_kind() {
        let store = ResourceStore::new();
        assert_eq!(
            store.availability(&pod_gvk()),
            KindAvailability::Watching,
            "absence means nothing permanent has been observed yet, not broken"
        );
    }

    #[tokio::test]
    async fn a_forbidden_watch_records_availability_in_the_store_not_just_a_message() {
        // The sidebar renders availability as a state read from the store
        // snapshot. Deriving it by matching on the AppEvent::Error string
        // would break the first time kube-rs rewords that message, silently,
        // with no test able to catch it — this asserts the structured state
        // exists instead.
        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, _rx) = mpsc::unbounded_channel();

        let events = vec![Err(forbidden_error("pods is forbidden"))];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);

        drive_watch(s, gvk.clone(), ar, None, store.clone(), tx).await;

        assert!(matches!(
            store.read().await.availability(&gvk),
            KindAvailability::Unavailable { .. }
        ));
    }

    #[tokio::test]
    async fn one_kinds_unavailability_does_not_affect_another() {
        // On a corporate cluster the user lacks RBAC on some kinds and not
        // others — this is the normal case, not an edge case. One forbidden
        // kind must not make every other kind read as broken too.
        let forbidden_gvk = pod_gvk();
        let healthy_gvk = GroupVersionKind::gvk("apps", "v1", "Deployment");
        let forbidden_ar = ApiResource::erase::<Pod>(&());
        let healthy_ar = ApiResource::from_gvk(&healthy_gvk);
        let store = test_store();

        let (tx1, _rx1) = mpsc::unbounded_channel();
        let forbidden_events = vec![Err(forbidden_error("pods is forbidden"))];
        let s1 = futures::stream::iter(forbidden_events);
        futures::pin_mut!(s1);
        drive_watch(
            s1,
            forbidden_gvk.clone(),
            forbidden_ar,
            None,
            store.clone(),
            tx1,
        )
        .await;

        let (tx2, _rx2) = mpsc::unbounded_channel();
        let healthy_events = vec![Ok(watcher::Event::InitDone)];
        let s2 = futures::stream::iter(healthy_events);
        futures::pin_mut!(s2);
        drive_watch(
            s2,
            healthy_gvk.clone(),
            healthy_ar,
            None,
            store.clone(),
            tx2,
        )
        .await;

        assert!(
            matches!(
                store.read().await.availability(&forbidden_gvk),
                KindAvailability::Unavailable { .. }
            ),
            "the forbidden kind must be marked unavailable"
        );
        assert_eq!(
            store.read().await.availability(&healthy_gvk),
            KindAvailability::Watching,
            "a different kind's forbidden watch must not leak into this one's availability"
        );
    }

    #[tokio::test]
    async fn a_not_found_error_stops_the_watch_and_marks_it_failed() {
        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, _rx) = mpsc::unbounded_channel();

        let events = vec![
            Err(not_found_error("widgets not found")),
            Ok(watcher::Event::Apply(pod("should-never-be-applied"))),
        ];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);

        drive_watch(s, gvk.clone(), ar, None, store.clone(), tx).await;

        assert_eq!(store.read().await.status(&gvk), WatchStatus::Failed);
        assert!(store.read().await.objects(&gvk).is_empty());
    }

    #[tokio::test]
    async fn a_transient_error_does_not_stop_the_watch() {
        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, mut rx) = mpsc::unbounded_channel();

        // The fake stream is finite, so `drive_watch` always reaches its
        // "stream ended unexpectedly" epilogue and the *final* stored status
        // ends up Failed regardless of this fix — a real long-lived watch
        // never naturally ends, so that epilogue is a separate concern from
        // what's under test here. What proves the transient error didn't
        // stop anything is that the event after it was still processed
        // (reflected in the object count) and that the watch did reach
        // Synced at some point along the way (reflected in the emitted
        // status events), not the terminal snapshot.
        let events = vec![
            Err(watcher::Error::NoResourceVersion),
            Ok(watcher::Event::Apply(pod("a"))),
            Ok(watcher::Event::InitDone),
        ];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);

        drive_watch(s, gvk.clone(), ar, None, store.clone(), tx).await;

        assert_eq!(
            store.read().await.objects(&gvk).len(),
            1,
            "a transient error must not stop the watch from processing what follows it"
        );

        let mut saw_synced = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::WatchStatus {
                status: WatchStatus::Synced,
                ..
            } = ev
            {
                saw_synced = true;
            }
        }
        assert!(
            saw_synced,
            "the watch must reach Synced after the transient error clears"
        );
    }

    // --- per-kind fetched table data lives in the store, keyed by gvk ---

    #[test]
    fn table_data_defaults_to_none_before_any_fetch_completes() {
        let store = ResourceStore::new();
        assert!(
            store.table_data(&pod_gvk()).is_none(),
            "absence means no fetch has completed yet, not an error"
        );
    }

    #[test]
    fn table_data_is_recorded_and_isolated_per_kind() {
        use crate::store::table::{TableColumn, TableRow};
        let mut store = ResourceStore::new();
        let other = GroupVersionKind::gvk("apps", "v1", "Deployment");
        let table = TableData {
            columns: vec![TableColumn {
                name: "Name".to_string(),
                priority: 0,
            }],
            rows: vec![TableRow {
                cells: vec!["a".to_string()],
                identity: None,
            }],
        };
        store.set_table_data(pod_gvk(), table.clone());
        assert_eq!(
            store.table_data(&pod_gvk()).expect("just set").rows,
            table.rows
        );
        assert!(
            store.table_data(&other).is_none(),
            "one kind's fetched table must not leak into another's"
        );
    }

    #[tokio::test]
    async fn a_forbidden_error_emits_the_actionable_message_not_the_raw_one() {
        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let events = vec![Err(forbidden_error(
            "pods is forbidden: User \"u\" cannot list resource \"pods\" at the cluster scope",
        ))];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);

        drive_watch(s, gvk, ar, None, store, tx).await;

        let mut saw_remedy = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Error(msg) = ev
                && msg.contains("-n <namespace>")
            {
                saw_remedy = true;
            }
        }
        assert!(
            saw_remedy,
            "a forbidden watch must emit the actionable -n remedy, not just the raw apiserver text"
        );
    }

    #[tokio::test]
    async fn a_watch_failing_on_a_credential_plugin_never_emits_the_token() {
        // Exec credentials refresh lazily, per request — so this fires long
        // after startup, repeatedly, straight into the status bar. The error's
        // own `Display` is the plugin's stdout in plaintext (proved below), so
        // formatting it verbatim is a live bearer token on screen.
        const TOKEN: &str = "SUPER-SECRET-TOKEN-abc123";
        let raw = credential_leak_error(TOKEN);
        assert!(
            raw.to_string().contains(TOKEN),
            "the fixture must actually leak, or this test guards nothing: {raw}"
        );

        let gvk = pod_gvk();
        let ar = ApiResource::erase::<Pod>(&());
        let store = test_store();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let events = vec![Err(credential_leak_error(TOKEN))];
        let s = futures::stream::iter(events);
        futures::pin_mut!(s);
        drive_watch(s, gvk, ar, None, store, tx).await;

        let mut saw_error = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::Error(msg) = ev {
                assert!(
                    !msg.contains(TOKEN),
                    "a bearer token reached the status bar: {msg}"
                );
                saw_error = true;
            }
        }
        assert!(
            saw_error,
            "the watch must still report the failure — silence would be a worse fix than the leak"
        );
    }
}
