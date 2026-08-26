use crate::app::event::AppEvent;
use crate::cluster::discovery::KindInfo;
use crate::cluster::{ClusterId, ClusterRegistry, ConnectionState, NamespaceListError};
use crate::store::handles::WatchHandles;
use crate::store::watch::{ResourceStore, SharedStore};
use kube::Client;
use kube::api::GroupVersionKind;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

/// What a cluster switch is doing, for the status bar and the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Connecting(ClusterId),
    Connected(ClusterId),
    ConnectFailed { id: ClusterId, reason: String },
}

/// Distinguish "we aborted this watch on purpose" from "this watch died".
///
/// Aborting a task makes its `JoinHandle` resolve to `Err`, exactly as a panic
/// does. A supervisor that cannot tell the two apart would quit the app on the
/// first cluster switch, because switching is implemented by aborting watches.
pub fn is_deliberate_abort(e: &tokio::task::JoinError) -> bool {
    e.is_cancelled()
}

/// Everything that belongs to the cluster currently on screen.
pub struct Session {
    pub registry: ClusterRegistry,
    pub handles: WatchHandles,
    /// Replaced wholesale on every switch — never cleared and reused.
    pub store: SharedStore,
    /// The `Client` for whichever cluster `registry.active()` currently
    /// reports.
    ///
    /// Lives here, behind the SAME lock as `registry`/`store`/`handles`,
    /// rather than in a side cell somewhere else: a client read from a
    /// separate lock — even one updated correctly by `switch_cluster` — can
    /// be captured, then go stale in the gap before the reader acquires
    /// ITS OWN lock, if a concurrent switch completes in between. Reading it
    /// from the same guard used for whatever else needs it (see
    /// `restart_watch`) makes that gap not exist rather than requiring two
    /// sources of truth to be kept in sync — found in review of this
    /// session's own first version, which is exactly what this replaced.
    pub client: Client,
    /// The namespace scope the CURRENT watch was started with. `None` means
    /// every namespace.
    ///
    /// Lives here for the same reason `client` does, and written under the
    /// same guard: it is *derived from* the watch that is actually running,
    /// so anything that displays the scope must read it from here rather than
    /// keep its own copy. A separate copy goes wrong on the first switch
    /// anyone makes — `kube -n payments` on prod, pick dev, and the switch
    /// deliberately watches all namespaces while the status bar still reads
    /// `dev · payments`, naming a scope no watch is using.
    pub namespace: Option<String>,
    /// True only when `namespace` is the "default" we fell back to because
    /// the context named none — the condition for the "try -A" hint.
    ///
    /// It can only ever be set by `Session::new`: every other way the scope
    /// changes (`switch_cluster`, `restart_watch`) is a deliberate choice by
    /// the user, and both clear it. That makes "hint showing after the user
    /// picked a namespace" unrepresentable rather than something each call
    /// site has to remember to reset.
    pub namespace_is_fallback: bool,
    /// The last answer to "what namespaces does the API say exist", if any
    /// fetch has completed. `None` until the namespace picker has been
    /// opened at least once (see `main.rs`, which spawns the fetch when it
    /// opens and delivers the result back through `AppEvent`).
    ///
    /// Lives here, under the same lock as `client`/`namespace`, rather than
    /// in a local the event loop threads through frames on its own — a
    /// second place to keep this in sync with a cluster switch (which must
    /// invalidate a listing fetched against the PREVIOUS cluster) is exactly
    /// the two-sources-of-truth shape earlier reviews of this project
    /// flagged for `client` and `namespace` themselves.
    pub namespaces_from_api: Option<Result<Vec<String>, NamespaceListError>>,
    /// Every browsable kind discovery found on the cluster currently on
    /// screen, in `cluster::discovery::sort_kinds`' stable group-then-kind
    /// order — which is the order the sidebar draws. Empty until the first
    /// discovery for this cluster completes.
    ///
    /// Lives here, under the same lock as `client`/`namespace`/`store`, for
    /// the reason every other per-cluster fact does: a switch replaces the
    /// cluster's kinds along with its client, its scope and its store, and a
    /// copy kept anywhere else would have to be invalidated in lockstep with
    /// all four. This project has produced five review findings from exactly
    /// that shape. The `prioritise`d ordering used to decide which kinds fit
    /// under the watch cap is deliberately NOT stored: it is computed from
    /// this list when the watches are started, so the display order and the
    /// cap decision cannot drift into two different answers about "the kinds"
    /// (see `store::multi`, whose two functions are separate for this reason).
    pub kinds: Vec<KindInfo>,
    /// The kind the table is showing. `default_kind()` until the user picks
    /// another in the sidebar.
    ///
    /// Here rather than in the event loop for the same reason as `kinds`:
    /// a cluster switch must reset it (the new cluster may not have the old
    /// kind at all), and it is read on every frame beside the store the
    /// objects come from. Reading it from a local would let the table draw
    /// one kind's objects under another kind's columns for a frame after a
    /// switch.
    pub active_kind: GroupVersionKind,
    /// Bumped by every switch so a slow connect that has been superseded can
    /// tell it is stale and stand down.
    pub generation: u64,
    /// Which attempt owns each cluster's `Connecting` marker, by generation.
    ///
    /// A superseded attempt must retract only the marker it set itself. The
    /// user can pick the *same* slow cluster twice, so by the time the first
    /// attempt returns the entry may be `Connecting` again — or `Connected`
    /// and streaming — on behalf of a later attempt. Bookkeeping only: the
    /// registry remains the source of truth for what to display.
    pub pending: HashMap<ClusterId, u64>,
}

impl Session {
    /// `namespace` is the scope the initial watch is being started with —
    /// `None` for all namespaces. `namespace_is_fallback` is true only when
    /// that scope is a "default" nobody actually chose.
    pub fn new(
        registry: ClusterRegistry,
        client: Client,
        namespace: Option<String>,
        namespace_is_fallback: bool,
    ) -> Self {
        Self {
            registry,
            handles: WatchHandles::new(),
            store: Arc::new(RwLock::new(ResourceStore::new())),
            client,
            namespace,
            namespace_is_fallback,
            namespaces_from_api: None,
            kinds: Vec::new(),
            active_kind: default_kind(),
            generation: 0,
            pending: HashMap::new(),
        }
    }
}

/// The kind the table opens on, and the one a cluster switch resets to.
///
/// `core/v1 Pod` is the only kind guaranteed to exist on every Kubernetes
/// cluster and the one an operator opens the tool to look at. Carrying the
/// previous cluster's active kind across a switch would point the table at a
/// kind the new cluster may not have at all — a CRD from the old cluster —
/// which renders as a permanently empty table with no explanation.
pub fn default_kind() -> GroupVersionKind {
    GroupVersionKind::gvk("", "v1", "Pod")
}

pub type SharedSession = Arc<Mutex<Session>>;

/// Connect to `id` and, only once that has succeeded, replace the session:
/// tear down the outgoing cluster's watches, mint a fresh store, and watch it.
///
/// **Connect first, tear down second.** Committing to the switch before
/// knowing it will work leaves a failed attempt showing the old cluster's name
/// above an empty table with nothing watching it — a state worse than either
/// endpoint. On a corporate kubeconfig where some clusters are permanently
/// unreachable behind the VPN that is a routine path, not an edge case, so a
/// failure here changes nothing at all: the old cluster keeps its store, its
/// watches and its place in the status bar. The cost is one extra live
/// connection for the duration of the attempt.
///
/// `connect` and `spawn_watches` are injected rather than called directly so
/// that switching is testable without a cluster, a network or a kubeconfig.
/// `main` passes `cluster::connect_with` and `store::watch::spawn_watch`.
///
/// **`spawn_watches` is called with the session lock held.** It must do no more
/// than start the watch and hand back its handle: acquiring the session lock
/// inside it — directly, or by awaiting anything that does — deadlocks the
/// switch and, with it, the event loop.
///
/// Run this on a spawned task. It awaits `connect`, which for a cluster the
/// current VPN cannot reach takes tens of seconds; called inline from the
/// event loop it would freeze the UI for the whole attempt. The session lock
/// is deliberately released before that await for the same reason — the event
/// loop takes it on every pass to read the store.
pub async fn switch_cluster<C, F, W>(
    session: SharedSession,
    id: ClusterId,
    namespace: Option<String>,
    tx: UnboundedSender<AppEvent>,
    connect: C,
    spawn_watches: W,
) where
    C: FnOnce() -> F,
    F: Future<Output = anyhow::Result<Client>>,
    W: FnOnce(Client, SharedStore, Option<String>) -> JoinHandle<()>,
{
    // 1. Mark the TARGET as connecting. Nothing belonging to the cluster
    //    currently on screen is touched here — see the note above.
    let generation = {
        let mut s = session.lock().await;

        // `set_active` rejects an id the registry does not know, so a switch to
        // one could never complete. Say so now rather than spending a
        // thirty-second connect attempt discovering it.
        if s.registry.find(&id).is_none() {
            drop(s);
            let reason = format!("unknown cluster '{}'", id.0);
            let _ = tx.send(AppEvent::Session(SessionEvent::ConnectFailed {
                id,
                reason,
            }));
            return;
        }

        s.registry.set_state(&id, ConnectionState::Connecting);

        // A second switch started while this one is still connecting must win.
        // Re-checked after the connect, which is where it does the real work: a
        // stale success would otherwise tear down the cluster the user is on
        // now in order to install an obsolete one.
        s.generation += 1;
        let generation = s.generation;

        // Record that the marker just written belongs to THIS attempt, so a
        // later one targeting the same cluster can take ownership of it.
        s.pending.insert(id.clone(), generation);
        generation
    };

    // 2. Announce the wait and arm a redraw before the attempt begins.
    let _ = tx.send(AppEvent::Session(SessionEvent::Connecting(id.clone())));

    // 3. Connect with no lock held. See the note on this function.
    let connected = connect().await;

    let mut s = session.lock().await;
    if s.generation != generation {
        // Superseded while we were connecting. Any client we obtained is
        // dropped here rather than used: the session belongs to another cluster
        // now, and both reporting this one as connected and tearing that
        // cluster down to install this one would be wrong.
        //
        // A superseded attempt may retract only the marker it set ITSELF. The
        // user can pick the same slow cluster twice, so this entry may by now
        // be `Connected` and streaming on behalf of a later attempt — writing
        // `Disconnected` over that is a permanent lie about the cluster the
        // user is actually on, with nothing to reset it until the next switch
        // — or `Connecting` for a later attempt still in flight, where it
        // would erase the indicator mid-attempt.
        if s.pending.get(&id) == Some(&generation) {
            s.pending.remove(&id);
            s.registry.set_state(&id, ConnectionState::Disconnected);
        }
        return;
    }
    // Not superseded, so nothing can have taken the marker: this attempt still
    // owns it and is about to replace it with its own outcome.
    s.pending.remove(&id);

    match connected {
        Ok(client) => {
            // 4. The new cluster is reachable, so now — and only now — the old
            //    one can go. Stop its watches first.
            s.handles.abort_all();

            // 5. Replace the store; never clear and reuse it. `abort()` takes
            //    effect at the task's next suspension point, so a watch that
            //    has already read an event off its stream can finish its
            //    `apply` after `abort_all()` has returned. Against a reused
            //    store that write surfaces as an object from the old cluster
            //    listed under the new one; against a replaced store it lands
            //    somewhere nobody reads, and the race stops mattering instead
            //    of needing to be timed.
            s.store = Arc::new(RwLock::new(ResourceStore::new()));

            // Record the client BEFORE handing it to `spawn_watches`, which
            // consumes it — and while still holding this same guard, so
            // `client` and `active`/`state` change together atomically. A
            // reader taking the lock either sees the whole switch or none of
            // it, never a client for one cluster paired with another's id.
            s.client = client.clone();

            // Likewise the scope: recorded here, and handed to
            // `spawn_watches` as the SAME value, so what the session reports
            // and what the watch actually watches cannot disagree. Picking a
            // cluster is a deliberate choice of scope, so any "we fell back
            // to default" hint from startup no longer applies.
            s.namespace = namespace.clone();
            s.namespace_is_fallback = false;

            // A namespace listing fetched against the OUTGOING cluster names
            // namespaces that may not even exist on this one. Clearing it
            // rather than carrying it over is what makes "picker shows a
            // stale cluster's namespaces after a switch" unrepresentable,
            // the same reasoning `client` and `namespace` are replaced
            // wholesale for above rather than patched in place.
            s.namespaces_from_api = None;

            // 6. Watch the store minted just above — never the one the previous
            //    cluster's watches were writing into.
            let store = s.store.clone();
            let handle = spawn_watches(client, store, namespace);
            s.handles.push(handle);

            s.registry.set_active(&id);
            s.registry.set_state(&id, ConnectionState::Connected);
            drop(s);
            let _ = tx.send(AppEvent::Session(SessionEvent::Connected(id)));
        }
        Err(e) => {
            // Nothing else changes. The previous cluster keeps its store, its
            // watches and its place in the status bar: a failed connection must
            // leave the user working with what they already had.
            let reason = format!("{e:#}");
            s.registry.set_state(
                &id,
                ConnectionState::Failed {
                    reason: reason.clone(),
                },
            );
            drop(s);
            let _ = tx.send(AppEvent::Session(SessionEvent::ConnectFailed {
                id,
                reason,
            }));
        }
    }
}

/// Restart the watch for the cluster the session is CURRENTLY on — used when
/// only the namespace scope changes, not the cluster itself. Tears down the
/// existing watch(es), mints a fresh store (same reasoning as
/// `switch_cluster` step 5: a stray write from a not-yet-cancelled watch must
/// land somewhere nobody reads), and spawns a new one.
///
/// **Reads `client` from the SAME lock guard used for the teardown and store
/// swap**, rather than from an earlier, separate acquisition — a client
/// captured before taking the lock can go stale if a concurrent
/// `switch_cluster` completes in the gap between capturing it and actually
/// acquiring the lock. `Session` holding the client (rather than a side cell
/// elsewhere) is what makes that gap not exist rather than requiring two
/// locks to be kept in sync — see the doc comment on `Session::client`.
///
/// **`spawn_watches` is called with the session lock held**, the same
/// constraint as `switch_cluster`'s: it must do no more than start the watch
/// and hand back its handle.
pub async fn restart_watch<W>(session: SharedSession, namespace: Option<String>, spawn_watches: W)
where
    W: FnOnce(Client, SharedStore, Option<String>) -> JoinHandle<()>,
{
    let mut s = session.lock().await;
    s.handles.abort_all();
    s.store = Arc::new(RwLock::new(ResourceStore::new()));
    let store = s.store.clone();
    let client = s.client.clone();
    // Recorded under the same guard as the teardown, and handed to
    // `spawn_watches` as the same value — the session's reported scope and
    // the watch's actual scope are one fact, not two to keep in sync. The
    // user chose this namespace, so the startup fallback hint is done.
    s.namespace = namespace.clone();
    s.namespace_is_fallback = false;
    let handle = spawn_watches(client, store, namespace);
    s.handles.push(handle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{AuthMethod, ContextInfo};
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::runtime::watcher;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    fn pod_ar() -> ApiResource {
        ApiResource::erase::<Pod>(&())
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &pod_ar()).within("default")
    }

    fn ctx(name: &str, current: bool) -> ContextInfo {
        ContextInfo {
            name: name.to_string(),
            cluster: format!("{name}-cluster"),
            namespace: None,
            is_current: current,
            auth: AuthMethod::None,
        }
    }

    /// A session over the named clusters, the first of which is active —
    /// standing in for the kubeconfig's current-context.
    fn session_over(names: &[&str]) -> SharedSession {
        let contexts = names
            .iter()
            .enumerate()
            .map(|(i, n)| ctx(n, i == 0))
            .collect();
        Arc::new(Mutex::new(Session::new(
            ClusterRegistry::from_contexts(contexts),
            offline_client(),
            None,
            false,
        )))
    }

    fn id(name: &str) -> ClusterId {
        ClusterId(name.to_string())
    }

    /// A client that never talks to anything. Building one is pure local
    /// construction — no DNS, no socket — so these tests need neither a
    /// cluster nor a network, and nothing here ever issues a request.
    fn offline_client() -> Client {
        let uri: http::Uri = "http://127.0.0.1:1/"
            .parse()
            .expect("a static, well-formed URI");
        Client::try_from(kube::Config::new(uri)).expect("building a client performs no I/O")
    }

    /// An offline client tagged so a test can tell which one ended up in
    /// use. `Client` exposes no other identity a test can read back without
    /// performing I/O; `default_namespace` is a public `Config` field that
    /// survives into the built `Client` and back out via
    /// `Client::default_namespace()`.
    fn tagged_client(tag: &str) -> Client {
        let uri: http::Uri = "http://127.0.0.1:1/"
            .parse()
            .expect("a static, well-formed URI");
        let mut cfg = kube::Config::new(uri);
        cfg.default_namespace = tag.to_string();
        Client::try_from(cfg).expect("building a client performs no I/O")
    }

    /// A watch stand-in that stays alive until aborted.
    fn live_watch() -> JoinHandle<()> {
        tokio::spawn(async {
            std::future::pending::<()>().await;
        })
    }

    fn session_events(rx: &mut UnboundedReceiver<AppEvent>) -> Vec<SessionEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            if let AppEvent::Session(s) = e {
                out.push(s);
            }
        }
        out
    }

    #[tokio::test]
    async fn a_deliberately_aborted_watch_is_not_treated_as_a_failure() {
        let h = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        h.abort();
        let err = h.await.expect_err("an aborted task must join as Err");
        assert!(
            is_deliberate_abort(&err),
            "switching clusters aborts watches; treating that as a crash would quit the app"
        );
    }

    #[tokio::test]
    async fn a_panicking_watch_is_still_treated_as_a_failure() {
        let h = tokio::spawn(async { panic!("boom") });
        let err = h.await.expect_err("a panicking task must join as Err");
        assert!(
            !is_deliberate_abort(&err),
            "a real panic must not be mistaken for a cluster switch"
        );
    }

    #[tokio::test]
    async fn a_write_from_an_aborted_watch_cannot_reach_the_new_store() {
        // `abort()` takes effect at the task's next suspension point, so a
        // watch that has already read an event off its stream can complete its
        // `apply` after `abort_all()` returns. Replacing the store rather than
        // clearing it makes that write land somewhere nobody reads.
        let session = session_over(&["prod", "dev"]);
        let gvk = pod_gvk();

        // The old cluster's store as a live watch would have left it.
        let old_store = session.lock().await.store.clone();
        old_store.write().await.apply(
            &gvk,
            &pod_ar(),
            watcher::Event::Apply(pod("pod-from-old-cluster")),
        );

        // Deliberately NOT registered in `WatchHandles`: this stands in for a
        // task that is already past its last suspension point, which is
        // precisely the one `abort_all()` cannot stop in time. Registering it
        // would let the abort cancel it before it wrote, and the test would
        // then prove nothing.
        let stale_writer = {
            let store = old_store.clone();
            let gvk = gvk.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(20)).await;
                store.write().await.apply(
                    &gvk,
                    &pod_ar(),
                    watcher::Event::Apply(pod("stale-from-old-cluster")),
                );
            })
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;

        let _ = stale_writer.await;

        let new_store = session.lock().await.store.clone();
        assert!(
            new_store.read().await.objects(&gvk).is_empty(),
            "a write from the previous cluster must not appear in the new cluster's store"
        );
        // Without this the test would also pass if the stale write never
        // happened at all, which would make the assertion above vacuous.
        let old = old_store.read().await.objects(&gvk);
        assert_eq!(
            old.len(),
            2,
            "the stale write must actually have landed — in the OLD store, got {:?}",
            old.iter()
                .map(|o| o.metadata.name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn the_new_clusters_watch_is_spawned_against_the_new_store() {
        // Replacing the store is only half the fix: the watch we start for the
        // new cluster must be handed the replacement, not the store the old
        // cluster's watches were writing into.
        let session = session_over(&["prod", "dev"]);
        let old_store = session.lock().await.store.clone();
        let seen: Arc<StdMutex<Option<SharedStore>>> = Arc::new(StdMutex::new(None));

        let (tx, _rx) = mpsc::unbounded_channel();
        let recorder = {
            let seen = seen.clone();
            move |_client, store: SharedStore, _ns| {
                *seen.lock().expect("uncontended in a test") = Some(store);
                live_watch()
            }
        };
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            recorder,
        )
        .await;

        let given = seen
            .lock()
            .expect("uncontended in a test")
            .clone()
            .expect("a successful switch must spawn a watch");
        let current = session.lock().await.store.clone();
        assert!(
            Arc::ptr_eq(&given, &current),
            "the new watch must write into the store the UI now reads"
        );
        assert!(
            !Arc::ptr_eq(&given, &old_store),
            "the new watch must not write into the previous cluster's store"
        );
    }

    #[tokio::test]
    async fn switching_clusters_repeatedly_does_not_accumulate_watches() {
        let session = session_over(&["prod", "dev"]);
        let (tx, _rx) = mpsc::unbounded_channel();
        for _ in 0..20 {
            switch_cluster(
                session.clone(),
                id("dev"),
                None,
                tx.clone(),
                || async { Ok(offline_client()) },
                |_, _, _| live_watch(),
            )
            .await;
        }
        assert_eq!(
            session.lock().await.handles.len(),
            1,
            "twenty switches must leave one live watch, not twenty"
        );
    }

    #[tokio::test]
    async fn a_failed_connect_leaves_the_previous_cluster_active() {
        let session = session_over(&["prod", "dev"]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let spawned = Arc::new(StdMutex::new(false));

        let watcher_spawn = {
            let spawned = spawned.clone();
            move |_client, _store, _ns| {
                *spawned.lock().expect("uncontended in a test") = true;
                live_watch()
            }
        };
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Err(anyhow::anyhow!("no route to host")) },
            watcher_spawn,
        )
        .await;

        let s = session.lock().await;
        assert_eq!(
            s.registry.active().map(|e| e.id.0.as_str()),
            Some("prod"),
            "a failed connection must not strand the user with no cluster at all"
        );
        match &s.registry.find(&id("dev")).expect("dev is known").state {
            ConnectionState::Failed { reason } => assert!(
                reason.contains("no route to host"),
                "the reason must survive for the UI to show, got {reason:?}"
            ),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            !*spawned.lock().expect("uncontended in a test"),
            "no watch may be started against a cluster we failed to reach"
        );
        drop(s);

        assert_eq!(
            session_events(&mut rx),
            vec![
                SessionEvent::Connecting(id("dev")),
                SessionEvent::ConnectFailed {
                    id: id("dev"),
                    reason: "no route to host".to_string(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_connect_leaves_the_previous_cluster_working() {
        // Tearing down before knowing the new cluster is reachable strands the
        // user on a live cluster name with an empty table and nothing watching
        // it. On a corporate kubeconfig some clusters are permanently
        // unreachable behind the VPN, so this is a routine path, not an edge
        // case: a failed switch must change nothing at all.
        let session = session_over(&["prod", "dev"]);
        let gvk = pod_gvk();

        // Cluster A as the user left it: data on screen and a live watch.
        let store_before = session.lock().await.store.clone();
        store_before
            .write()
            .await
            .apply(&gvk, &pod_ar(), watcher::Event::Apply(pod("prod-pod")));
        session.lock().await.handles.push(live_watch());
        let handles_before = session.lock().await.handles.len();

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Err(anyhow::anyhow!("no route to host")) },
            |_, _, _| live_watch(),
        )
        .await;

        let (active, store_after, handles_after) = {
            let s = session.lock().await;
            (
                s.registry.active().map(|e| e.id.0.clone()),
                s.store.clone(),
                s.handles.len(),
            )
        };
        assert_eq!(
            active.as_deref(),
            Some("prod"),
            "a failed switch must not change the active cluster"
        );
        assert!(
            !store_after.read().await.objects(&gvk).is_empty(),
            "prod's data must survive a failed switch to dev"
        );
        assert_eq!(
            handles_after, handles_before,
            "prod's watches must not be torn down by a failed switch"
        );
        assert!(
            Arc::ptr_eq(&store_after, &store_before),
            "prod's store must not even have been replaced"
        );
    }

    #[tokio::test]
    async fn connecting_is_announced_and_the_lock_released_before_the_attempt() {
        // Some of these clusters sit behind a VPN and take tens of seconds to
        // fail. The UI must already say "connecting", and the session lock —
        // which the event loop takes on every iteration to read the store —
        // must be free, or the whole app freezes for the duration.
        let session = session_over(&["prod", "dev"]);
        let old_store = session.lock().await.store.clone();
        // What the connect attempt saw: the target's state and whether the
        // store was still the old one. `None` overall means the lock was held.
        type Snapshot = Option<(Option<ConnectionState>, bool)>;
        let observed: Arc<StdMutex<Snapshot>> = Arc::new(StdMutex::new(None));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let connect = {
            let session = session.clone();
            let observed = observed.clone();
            let old_store = old_store.clone();
            move || async move {
                let snapshot = session.try_lock().ok().map(|s| {
                    (
                        s.registry.find(&id("dev")).map(|e| e.state.clone()),
                        Arc::ptr_eq(&s.store, &old_store),
                    )
                });
                *observed.lock().expect("uncontended in a test") = snapshot;
                Ok(offline_client())
            }
        };
        switch_cluster(session.clone(), id("dev"), None, tx, connect, |_, _, _| {
            live_watch()
        })
        .await;

        let observed = observed.lock().expect("uncontended in a test").clone();
        let (state, same_store) = observed.expect(
            "the session lock was still held while connecting: the event loop would be frozen",
        );
        assert_eq!(
            state,
            Some(ConnectionState::Connecting),
            "the cluster must already read as connecting when the attempt starts"
        );
        assert!(
            same_store,
            "the previous cluster's store must still be intact while we connect: \
             nothing is torn down until the new cluster answers"
        );
        assert_eq!(
            session_events(&mut rx).first(),
            Some(&SessionEvent::Connecting(id("dev"))),
            "the redraw that shows 'connecting' must be armed before the attempt"
        );
    }

    #[tokio::test]
    async fn a_successful_switch_activates_the_cluster_and_announces_it() {
        let session = session_over(&["prod", "dev"]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;

        let s = session.lock().await;
        assert_eq!(s.registry.active().map(|e| e.id.0.as_str()), Some("dev"));
        assert_eq!(
            s.registry.find(&id("dev")).expect("dev is known").state,
            ConnectionState::Connected
        );
        assert_eq!(s.handles.len(), 1, "the new cluster must be watched");
        drop(s);

        assert_eq!(
            session_events(&mut rx),
            vec![
                SessionEvent::Connecting(id("dev")),
                SessionEvent::Connected(id("dev")),
            ],
            "connecting must be announced before connected, or the UI never shows the wait"
        );
    }

    #[tokio::test]
    async fn a_superseded_switch_does_not_activate_its_cluster() {
        // The user picks a slow cluster, waits, then picks another. The first
        // connect can still succeed afterwards; if it does, it must not steal
        // the session back or start a watch against the second cluster's store.
        let session = session_over(&["prod", "slow", "quick"]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let slow_spawned = Arc::new(StdMutex::new(false));

        let slow = {
            let session = session.clone();
            let tx = tx.clone();
            let slow_spawned = slow_spawned.clone();
            tokio::spawn(async move {
                switch_cluster(
                    session,
                    id("slow"),
                    None,
                    tx,
                    || async move {
                        let _ = entered_tx.send(());
                        let _ = release_rx.await;
                        Ok(offline_client())
                    },
                    move |_, _, _| {
                        *slow_spawned.lock().expect("uncontended in a test") = true;
                        live_watch()
                    },
                )
                .await;
            })
        };

        entered_rx.await.expect("the slow connect must start");
        switch_cluster(
            session.clone(),
            id("quick"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;
        let quicks_store = session.lock().await.store.clone();
        let _ = release_tx.send(());
        slow.await.expect("the superseded switch must not panic");

        let s = session.lock().await;
        assert_eq!(
            s.registry.active().map(|e| e.id.0.as_str()),
            Some("quick"),
            "a superseded connect must not steal the session back"
        );
        assert_ne!(
            s.registry.find(&id("slow")).expect("slow is known").state,
            ConnectionState::Connected,
            "we dropped that client, so reporting it as connected would be a lie"
        );
        assert_eq!(
            s.handles.len(),
            1,
            "the superseded switch must not add a watch of its own"
        );
        assert!(
            !*slow_spawned.lock().expect("uncontended in a test"),
            "the superseded switch must not watch the new cluster's store"
        );
        assert!(
            Arc::ptr_eq(&s.store, &quicks_store),
            "teardown happens on success, so a stale success must not tear down \
             the cluster the user is actually on"
        );
    }

    /// Start a switch to `id` whose connect blocks until released, and wait
    /// until it is actually inside the connect. Returns (task, release).
    ///
    /// The two tests below both target the SAME cluster twice, which is what
    /// makes a superseded attempt dangerous: the entry it marked `Connecting`
    /// is no longer its own by the time it returns.
    fn stalled_switch(
        session: SharedSession,
        target: ClusterId,
        tx: UnboundedSender<AppEvent>,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            switch_cluster(
                session,
                target,
                None,
                tx,
                || async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    Ok(offline_client())
                },
                |_, _, _| live_watch(),
            )
            .await;
        });
        (entered_rx, release_tx, task)
    }

    #[tokio::test]
    async fn a_superseded_connect_does_not_disconnect_a_now_live_cluster() {
        // The user picks a VPN-slow cluster, gets impatient, and picks the SAME
        // one again. The second attempt succeeds and the table fills. When the
        // first attempt finally returns it must not mark that live, streaming
        // cluster as disconnected — a state nothing resets until the next
        // switch, and one Task 9 renders in its own colour.
        let session = session_over(&["prod", "dev"]);
        let (tx, _rx) = mpsc::unbounded_channel();

        let (entered, release_first, first) =
            stalled_switch(session.clone(), id("dev"), tx.clone());
        entered.await.expect("the first attempt must start");

        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;
        let live_store = session.lock().await.store.clone();

        let _ = release_first.send(());
        first.await.expect("the superseded attempt must not panic");

        let s = session.lock().await;
        assert_eq!(
            s.registry.find(&id("dev")).expect("dev is known").state,
            ConnectionState::Connected,
            "the cluster the user is on is live and streaming; a stale attempt \
             must not report it as disconnected"
        );
        assert_eq!(s.registry.active().map(|e| e.id.0.as_str()), Some("dev"));
        assert_eq!(
            s.handles.len(),
            1,
            "one live watch, from the second attempt"
        );
        assert!(
            Arc::ptr_eq(&s.store, &live_store),
            "the stale attempt must not have touched the live store"
        );
    }

    #[tokio::test]
    async fn a_superseded_connect_does_not_erase_a_later_attempts_connecting_marker() {
        // Same cluster twice, but this time the first attempt returns while the
        // second is still connecting. Retracting the marker here would blank
        // the "connecting" indicator mid-attempt — hazard 3's symptom, arriving
        // through the back door.
        let session = session_over(&["prod", "dev"]);
        let (tx, _rx) = mpsc::unbounded_channel();

        let (entered_a, release_a, first) = stalled_switch(session.clone(), id("dev"), tx.clone());
        entered_a.await.expect("the first attempt must start");
        let (entered_b, release_b, second) = stalled_switch(session.clone(), id("dev"), tx.clone());
        entered_b.await.expect("the second attempt must start");

        let _ = release_a.send(());
        first.await.expect("the superseded attempt must not panic");

        assert_eq!(
            session
                .lock()
                .await
                .registry
                .find(&id("dev"))
                .expect("dev is known")
                .state,
            ConnectionState::Connecting,
            "the second attempt is still connecting; the first must not retract \
             a marker that is no longer its own"
        );

        // And the attempt that owns it still completes normally.
        let _ = release_b.send(());
        second.await.expect("the live attempt must not panic");
        let s = session.lock().await;
        assert_eq!(
            s.registry.find(&id("dev")).expect("dev is known").state,
            ConnectionState::Connected
        );
        assert_eq!(s.registry.active().map(|e| e.id.0.as_str()), Some("dev"));
    }

    #[tokio::test]
    async fn switching_to_an_unknown_cluster_leaves_the_session_untouched() {
        // `set_active` rejects an unknown id, so a switch that tore everything
        // down first would leave the user staring at an empty table with no
        // watches and no way back.
        let session = session_over(&["prod"]);
        let store_before = session.lock().await.store.clone();
        session.lock().await.handles.push(live_watch());

        let (tx, mut rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("nope"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;

        let s = session.lock().await;
        assert_eq!(s.registry.active().map(|e| e.id.0.as_str()), Some("prod"));
        assert!(
            Arc::ptr_eq(&s.store, &store_before),
            "an unknown cluster must not discard the data we are showing"
        );
        assert_eq!(s.handles.len(), 1, "the live watch must survive");
        drop(s);

        assert!(
            matches!(
                session_events(&mut rx).as_slice(),
                [SessionEvent::ConnectFailed { .. }]
            ),
            "the failure must be reported rather than silently ignored"
        );
    }

    #[tokio::test]
    async fn a_switch_can_be_driven_on_a_spawned_task() {
        // Task 9 runs this on `tokio::spawn`, which needs Send + 'static.
        // Nothing in `main.rs` calls `switch_cluster` yet, so without this the
        // first wiring attempt would be the first time anyone found out.
        let session = session_over(&["prod", "dev"]);
        let (tx, _rx) = mpsc::unbounded_channel();
        let handle: JoinHandle<()> = tokio::spawn(switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        ));
        handle.await.expect("the switch must not panic");
        assert_eq!(
            session
                .lock()
                .await
                .registry
                .active()
                .map(|e| e.id.0.as_str()),
            Some("dev")
        );
    }

    #[tokio::test]
    async fn a_successful_switch_installs_the_new_clusters_client() {
        // Task 9's review: a status/namespace path must never have to guess
        // which client belongs to the cluster the registry now reports
        // active. `switch_cluster` installing it is the other half of that
        // guarantee — `restart_watch` (below) is the half that reads it.
        let session = session_over(&["prod", "dev"]);
        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(tagged_client("dev-client")) },
            |_, _, _| live_watch(),
        )
        .await;
        assert_eq!(
            session.lock().await.client.default_namespace(),
            "dev-client",
            "the session's client must be the one the successful connect produced"
        );
    }

    /// A session started with `kube -n payments` on prod: a real namespace
    /// scope, deliberately chosen, so any of it surviving a switch is
    /// visible.
    fn session_scoped_to(namespace: Option<&str>, is_fallback: bool) -> SharedSession {
        Arc::new(Mutex::new(Session::new(
            ClusterRegistry::from_contexts(vec![ctx("prod", true), ctx("dev", false)]),
            offline_client(),
            namespace.map(|s| s.to_string()),
            is_fallback,
        )))
    }

    #[tokio::test]
    async fn a_switch_records_the_scope_the_new_watch_is_actually_started_with() {
        // `kube -n payments` on prod, press c, pick dev. The switch
        // deliberately watches ALL namespaces, so a scope tracked anywhere
        // but here still reads `dev · payments · 412 items · live` — naming a
        // namespace nothing is watching, over another cluster's data. First
        // switch anyone makes.
        let session = session_scoped_to(Some("payments"), false);
        assert_eq!(
            session.lock().await.namespace.as_deref(),
            Some("payments"),
            "sanity: the session starts on the -n scope"
        );

        let watched: Arc<StdMutex<Option<Option<String>>>> = Arc::new(StdMutex::new(None));
        let recorder = {
            let watched = watched.clone();
            move |_client, _store, ns: Option<String>| {
                *watched.lock().expect("uncontended in a test") = Some(ns);
                live_watch()
            }
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None, // the switch watches every namespace
            tx,
            || async { Ok(offline_client()) },
            recorder,
        )
        .await;

        assert_eq!(
            session.lock().await.namespace,
            None,
            "the session must report the scope the new watch uses, not the one \
             the previous cluster was on"
        );
        assert_eq!(
            watched.lock().expect("uncontended in a test").clone(),
            Some(None),
            "and the watch must have been started with that same scope — one \
             value, handed to both, so they cannot disagree"
        );
    }

    #[tokio::test]
    async fn a_switch_to_a_namespaced_scope_records_that_namespace() {
        // The other direction, so the test above cannot pass merely because
        // something always writes `None`.
        let session = session_scoped_to(None, false);
        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            Some("kube-system".to_string()),
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;
        assert_eq!(
            session.lock().await.namespace.as_deref(),
            Some("kube-system")
        );
    }

    #[tokio::test]
    async fn a_failed_switch_leaves_the_previous_scope_in_place() {
        // Same rule as the store, the watches and the active cluster: a
        // failed connect changes nothing. Reporting the target's scope over
        // the cluster we are still on would misname the data on screen.
        let session = session_scoped_to(Some("payments"), false);
        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Err(anyhow::anyhow!("no route to host")) },
            |_, _, _| live_watch(),
        )
        .await;
        assert_eq!(
            session.lock().await.namespace.as_deref(),
            Some("payments"),
            "prod is still on screen, so prod's scope must still be reported"
        );
    }

    #[tokio::test]
    async fn a_switch_clears_a_namespace_listing_fetched_against_the_previous_cluster() {
        // A namespace list fetched against prod names namespaces that may
        // not exist on dev at all. Carrying it over would let the picker
        // offer (or silently accept) a name that is valid nowhere near the
        // cluster the user just switched to.
        let session = session_scoped_to(Some("payments"), false);
        session.lock().await.namespaces_from_api = Some(Ok(vec!["payments".to_string()]));

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;

        assert_eq!(
            session.lock().await.namespaces_from_api,
            None,
            "a switch must not carry the previous cluster's namespace listing forward"
        );
    }

    #[tokio::test]
    async fn a_switch_clears_the_context_default_fallback_hint() {
        // The hint says "no pods here — try -A", which only makes sense while
        // the scope is a `default` nobody chose. Picking a cluster is a
        // choice, and the switch watches all namespaces anyway: continuing to
        // suggest -A there is advice to do what we already did.
        let session = session_scoped_to(Some("default"), true);
        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;
        assert!(
            !session.lock().await.namespace_is_fallback,
            "a deliberate switch must retire the startup fallback hint"
        );
    }

    // --- Task 10: per-cluster kinds and the active kind ---

    /// A `KindInfo` for a kind that exists only on one cluster — a CRD, the
    /// case that makes carrying `active_kind` across a switch visibly wrong.
    fn kind_info(group: &str, kind: &str) -> KindInfo {
        KindInfo {
            gvk: GroupVersionKind::gvk(group, "v1", kind),
            resource: ApiResource {
                group: group.to_string(),
                api_version: if group.is_empty() {
                    "v1".to_string()
                } else {
                    format!("{group}/v1")
                },
                kind: kind.to_string(),
                version: "v1".to_string(),
                plural: format!("{}s", kind.to_lowercase()),
            },
            namespaced: true,
            group_label: if group.is_empty() {
                "core".to_string()
            } else {
                group.to_string()
            },
        }
    }

    #[tokio::test]
    async fn a_session_starts_on_the_kind_every_cluster_has_with_nothing_discovered_yet() {
        let session = session_over(&["prod"]);
        let s = session.lock().await;
        assert_eq!(
            s.active_kind,
            GroupVersionKind::gvk("", "v1", "Pod"),
            "the table must open on a kind that exists everywhere"
        );
        assert!(
            s.kinds.is_empty(),
            "nothing has been discovered before the first discovery completes"
        );
    }

    #[tokio::test]
    async fn a_switch_discards_the_previous_clusters_kinds_and_active_kind() {
        // prod has an operator installed and the user is browsing its CRD.
        // dev does not have that operator. Carrying either the kind list or
        // the active kind across the switch points the sidebar and the table
        // at resources dev has never heard of — a table that is permanently
        // empty with no explanation, under a sidebar listing kinds that are
        // not there.
        let session = session_over(&["prod", "dev"]);
        {
            let mut s = session.lock().await;
            s.kinds = vec![kind_info("", "Pod"), kind_info("acme.io", "Widget")];
            s.active_kind = GroupVersionKind::gvk("acme.io", "v1", "Widget");
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Ok(offline_client()) },
            |_, _, _| live_watch(),
        )
        .await;

        let s = session.lock().await;
        assert!(
            s.kinds.is_empty(),
            "the new cluster's kinds are not known until its own discovery \
             completes; showing the old cluster's is a lie, got {:?}",
            s.kinds.iter().map(|k| &k.gvk.kind).collect::<Vec<_>>()
        );
        assert_eq!(
            s.active_kind,
            default_kind(),
            "the active kind must fall back to one every cluster has"
        );
    }

    #[tokio::test]
    async fn a_failed_switch_leaves_the_kinds_and_active_kind_exactly_as_they_were() {
        // Same rule as the store, the watches, the scope and the client: a
        // connect that failed changes nothing. Resetting here would empty the
        // sidebar of the cluster the user is still on and working with.
        let session = session_over(&["prod", "dev"]);
        {
            let mut s = session.lock().await;
            s.kinds = vec![kind_info("", "Pod"), kind_info("acme.io", "Widget")];
            s.active_kind = GroupVersionKind::gvk("acme.io", "v1", "Widget");
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        switch_cluster(
            session.clone(),
            id("dev"),
            None,
            tx,
            || async { Err(anyhow::anyhow!("no route to host")) },
            |_, _, _| live_watch(),
        )
        .await;

        let s = session.lock().await;
        assert_eq!(s.kinds.len(), 2, "prod's sidebar must survive intact");
        assert_eq!(
            s.active_kind,
            GroupVersionKind::gvk("acme.io", "v1", "Widget"),
            "prod is still on screen, so prod's active kind must still be active"
        );
    }

    #[tokio::test]
    async fn a_namespace_change_keeps_the_kinds_and_the_active_kind() {
        // Re-scoping is the same cluster: its kinds have not changed, and
        // throwing the user back to Pods every time they change namespace
        // would be gratuitous.
        let session = session_over(&["prod"]);
        {
            let mut s = session.lock().await;
            s.kinds = vec![kind_info("", "Pod"), kind_info("apps", "Deployment")];
            s.active_kind = GroupVersionKind::gvk("apps", "v1", "Deployment");
        }

        restart_watch(
            session.clone(),
            Some("payments".to_string()),
            |_client, _store, _ns| live_watch(),
        )
        .await;

        let s = session.lock().await;
        assert_eq!(s.kinds.len(), 2);
        assert_eq!(
            s.active_kind,
            GroupVersionKind::gvk("apps", "v1", "Deployment")
        );
    }

    #[tokio::test]
    async fn restart_watch_records_the_scope_it_was_given() {
        let session = session_scoped_to(Some("default"), true);
        let watched: Arc<StdMutex<Option<Option<String>>>> = Arc::new(StdMutex::new(None));
        let watched2 = watched.clone();
        restart_watch(
            session.clone(),
            Some("payments".to_string()),
            move |_client, _store, ns| {
                *watched2.lock().expect("uncontended in a test") = Some(ns);
                live_watch()
            },
        )
        .await;

        let s = session.lock().await;
        assert_eq!(
            s.namespace.as_deref(),
            Some("payments"),
            "the picked namespace must be what the session reports"
        );
        assert!(
            !s.namespace_is_fallback,
            "the user just chose a namespace, so the 'try -A' hint no longer applies"
        );
        drop(s);
        assert_eq!(
            watched.lock().expect("uncontended in a test").clone(),
            Some(Some("payments".to_string())),
            "and the watch must be started on that same namespace"
        );
    }

    #[tokio::test]
    async fn restart_watch_to_all_namespaces_records_the_all_scope() {
        let session = session_scoped_to(Some("payments"), false);
        restart_watch(session.clone(), None, |_client, _store, _ns| live_watch()).await;
        assert_eq!(session.lock().await.namespace, None);
    }

    #[tokio::test]
    async fn restart_watch_tears_down_the_old_watch_and_replaces_the_store() {
        let session = session_over(&["prod"]);
        session.lock().await.handles.push(live_watch());
        let old_store = session.lock().await.store.clone();

        restart_watch(session.clone(), None, |_client, _store, _ns| live_watch()).await;

        let s = session.lock().await;
        assert_eq!(
            s.handles.len(),
            1,
            "the old watch must be replaced, not added to"
        );
        assert!(
            !Arc::ptr_eq(&s.store, &old_store),
            "a fresh store must be minted, same reasoning as switch_cluster \
             — a stray write from a not-yet-cancelled watch must land \
             somewhere nobody reads"
        );
    }

    #[tokio::test]
    async fn restart_watch_uses_the_sessions_current_client() {
        let session = Arc::new(Mutex::new(Session::new(
            ClusterRegistry::from_contexts(vec![ctx("prod", true)]),
            tagged_client("prod-client"),
            None,
            false,
        )));
        let seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let seen2 = seen.clone();
        restart_watch(session, None, move |client, _store, _ns| {
            *seen2.lock().expect("uncontended in a test") =
                Some(client.default_namespace().to_string());
            live_watch()
        })
        .await;
        assert_eq!(
            seen.lock().expect("uncontended in a test").clone(),
            Some("prod-client".to_string())
        );
    }

    #[tokio::test]
    async fn restart_watch_reads_the_client_atomically_with_the_teardown_a_racing_switch_cannot_leave_it_stale()
     {
        // The exact interleaving Task 9's review traced: a switch to "dev"
        // is in flight, and — before it resolves — something needs to
        // restart the watch (originally: a namespace change). The buggy
        // shape `main.rs` had before this fix read the client from ONE lock
        // acquisition, then did the teardown in a SEPARATE one; a
        // concurrent `switch_cluster` completing in the gap between those
        // two acquisitions leaves the second one using a client for a
        // cluster the registry no longer calls active.
        //
        // First: reproduce that shape by hand, with an explicit gate
        // forcing the race window open, and confirm it really does go
        // stale — proving the hazard is real, not hypothetical. Then: run
        // the identical race against the real `restart_watch` and confirm
        // it cannot, because it has no second acquisition to race into —
        // client and teardown come from the same guard.
        let session = session_over(&["prod", "dev"]);
        assert_eq!(
            session.lock().await.client.default_namespace(),
            "default",
            "sanity: prod starts as session_over's default (untagged) client"
        );

        let (entered, release, switch) =
            stalled_switch_to_client(session.clone(), id("dev"), tagged_client("dev-client"));
        entered.await.expect("the switch to dev must start");

        // The buggy, pre-fix shape: capture the client, THEN wait for a
        // gate, THEN take the lock to do the rest — reproduced here by hand
        // since the real (fixed) `restart_watch` no longer has this seam to
        // reproduce it in.
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let stale_seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let buggy = {
            let session = session.clone();
            let stale_seen = stale_seen.clone();
            tokio::spawn(async move {
                let captured = session.lock().await.client.clone();
                let _ = gate_rx.await;
                let mut s = session.lock().await;
                s.handles.abort_all();
                *stale_seen.lock().expect("uncontended in a test") =
                    Some(captured.default_namespace().to_string());
            })
        };

        // Let dev's switch complete NOW, while `buggy` is parked at the
        // gate holding a client captured before dev won.
        let _ = release.send(());
        switch.await.expect("the switch must not panic");
        assert_eq!(
            session
                .lock()
                .await
                .registry
                .active()
                .map(|e| e.id.0.clone()),
            Some("dev".to_string()),
            "dev must be fully active before the stale read is allowed to proceed"
        );

        let _ = gate_tx.send(());
        buggy.await.expect("must not panic");
        assert_eq!(
            stale_seen.lock().expect("uncontended in a test").clone(),
            Some("default".to_string()),
            "reproduces the bug: the two-acquisition shape used prod's \
             client while the registry already said dev"
        );

        // Now the fix, under the identical interleaving: restart_watch is
        // asked to run only AFTER dev has already won, same as above — and
        // because it reads the client and does the teardown under one lock,
        // there is no earlier acquisition whose result could have gone
        // stale.
        let fixed_seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        let fixed_seen2 = fixed_seen.clone();
        restart_watch(session.clone(), None, move |client, _store, _ns| {
            *fixed_seen2.lock().expect("uncontended in a test") =
                Some(client.default_namespace().to_string());
            live_watch()
        })
        .await;
        assert_eq!(
            fixed_seen.lock().expect("uncontended in a test").clone(),
            Some("dev-client".to_string()),
            "restart_watch must use dev's client, matching the registry"
        );
    }

    /// As `stalled_switch`, but resolves the connect to a caller-chosen
    /// client rather than always `offline_client()` — needed to tell which
    /// cluster's client actually ended up in use.
    fn stalled_switch_to_client(
        session: SharedSession,
        target: ClusterId,
        client: Client,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
        JoinHandle<()>,
    ) {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (tx, _rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            switch_cluster(
                session,
                target,
                None,
                tx,
                move || async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    Ok(client)
                },
                |_, _, _| live_watch(),
            )
            .await;
        });
        (entered_rx, release_tx, task)
    }
}
