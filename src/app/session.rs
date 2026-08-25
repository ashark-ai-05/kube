use crate::app::event::AppEvent;
use crate::cluster::{ClusterId, ClusterRegistry, ConnectionState};
use crate::store::handles::WatchHandles;
use crate::store::watch::{ResourceStore, SharedStore};
use kube::Client;
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
    pub fn new(registry: ClusterRegistry) -> Self {
        Self {
            registry,
            handles: WatchHandles::new(),
            store: Arc::new(RwLock::new(ResourceStore::new())),
            generation: 0,
            pending: HashMap::new(),
        }
    }
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
    tx: UnboundedSender<AppEvent>,
    connect: C,
    spawn_watches: W,
) where
    C: FnOnce() -> F,
    F: Future<Output = anyhow::Result<Client>>,
    W: FnOnce(Client, SharedStore) -> JoinHandle<()>,
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

            // 6. Watch the store minted just above — never the one the previous
            //    cluster's watches were writing into.
            let store = s.store.clone();
            let handle = spawn_watches(client, store);
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
        Arc::new(Mutex::new(Session::new(ClusterRegistry::from_contexts(
            contexts,
        ))))
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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
            move |_client, store: SharedStore| {
                *seen.lock().expect("uncontended in a test") = Some(store);
                live_watch()
            }
        };
        switch_cluster(
            session.clone(),
            id("dev"),
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
                tx.clone(),
                || async { Ok(offline_client()) },
                |_, _| live_watch(),
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
            move |_client, _store| {
                *spawned.lock().expect("uncontended in a test") = true;
                live_watch()
            }
        };
        switch_cluster(
            session.clone(),
            id("dev"),
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
            tx,
            || async { Err(anyhow::anyhow!("no route to host")) },
            |_, _| live_watch(),
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
        switch_cluster(session.clone(), id("dev"), tx, connect, |_, _| live_watch()).await;

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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
                    tx,
                    || async move {
                        let _ = entered_tx.send(());
                        let _ = release_rx.await;
                        Ok(offline_client())
                    },
                    move |_, _| {
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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
                tx,
                || async move {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                    Ok(offline_client())
                },
                |_, _| live_watch(),
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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
            tx,
            || async { Ok(offline_client()) },
            |_, _| live_watch(),
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
}
