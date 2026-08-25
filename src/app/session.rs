use crate::app::event::AppEvent;
use crate::cluster::{ClusterId, ClusterRegistry, ConnectionState};
use crate::store::handles::WatchHandles;
use crate::store::watch::{ResourceStore, SharedStore};
use kube::Client;
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
pub fn is_deliberate_abort(_e: &tokio::task::JoinError) -> bool {
    todo!("is_deliberate_abort")
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
}

impl Session {
    pub fn new(registry: ClusterRegistry) -> Self {
        Self {
            registry,
            handles: WatchHandles::new(),
            store: Arc::new(RwLock::new(ResourceStore::new())),
            generation: 0,
        }
    }
}

pub type SharedSession = Arc<Mutex<Session>>;

/// Tear down the active cluster's watches, connect to `id`, and start again.
pub async fn switch_cluster<C, F, W>(
    _session: SharedSession,
    _id: ClusterId,
    _tx: UnboundedSender<AppEvent>,
    _connect: C,
    _spawn_watches: W,
) where
    C: FnOnce() -> F,
    F: Future<Output = anyhow::Result<Client>>,
    W: FnOnce(Client, SharedStore) -> JoinHandle<()>,
{
    todo!("switch_cluster")
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
            old.iter().map(|o| o.metadata.name.clone()).collect::<Vec<_>>()
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
    async fn connecting_is_announced_and_the_lock_released_before_the_attempt() {
        // Some of these clusters sit behind a VPN and take tens of seconds to
        // fail. The UI must already say "connecting", and the session lock —
        // which the event loop takes on every iteration to read the store —
        // must be free, or the whole app freezes for the duration.
        let session = session_over(&["prod", "dev"]);
        let old_store = session.lock().await.store.clone();
        let observed: Arc<StdMutex<Option<(Option<ConnectionState>, bool)>>> =
            Arc::new(StdMutex::new(None));

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
            !same_store,
            "the store must already have been replaced when the attempt starts"
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
            session.lock().await.registry.active().map(|e| e.id.0.as_str()),
            Some("dev")
        );
    }
}
