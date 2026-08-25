//! Cluster-backed tests, marked #[ignore] so `cargo test` stays green on
//! machines with no cluster. Run them with:
//!   ./scripts/dev-cluster.sh && cargo test -- --ignored

use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, GroupVersionKind};
use kube_tui::app::event::{AppEvent, WatchStatus};
use kube_tui::store::watch::{ResourceStore, SharedStore, spawn_watch};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard, RwLock, mpsc};

/// The two tests share the `demo` namespace and its deployment: one asserts a
/// pod count while the other deletes a pod. Rust runs tests in parallel by
/// default, so they must be serialised against each other. A mutex enforces
/// this in the code rather than relying on someone remembering
/// `--test-threads=1`.
///
/// This is `tokio::sync::Mutex`, not `std::sync::Mutex`: the guard is held
/// across `.await` points in both tests (connecting, watching, deleting), and
/// holding a blocking std mutex across an await is a real hazard — clippy's
/// `await_holding_lock` lint (denied via `-D warnings`) catches exactly this.
/// A useful side effect: `tokio::sync::Mutex` never poisons, so a panic in
/// one test simply drops the guard and the next test acquires cleanly —
/// no `PoisonError` recovery needed.
async fn cluster_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().await
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn watch_populates_the_store_from_a_real_cluster() {
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect()
        .await
        .expect("connect to cluster");
    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");

    let _h = spawn_watch(
        client,
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx,
    );

    // Wait for the initial sync rather than sleeping a fixed amount.
    let synced = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = rx.recv().await {
            if let AppEvent::WatchStatus {
                status: WatchStatus::Synced,
                ..
            } = ev
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("watch did not sync within 30s");

    assert!(synced, "expected a Synced status event");

    let objects = store.read().await.objects(&gvk);
    assert!(
        objects.len() >= 3,
        "expected at least the 3 demo pods, found {}",
        objects.len()
    );
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn store_reflects_a_deletion_made_during_the_watch() {
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect().await.expect("connect");
    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");

    let _h = spawn_watch(
        client.clone(),
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx,
    );

    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = rx.recv().await {
            if let AppEvent::WatchStatus {
                status: WatchStatus::Synced,
                ..
            } = ev
            {
                return;
            }
        }
    })
    .await
    .expect("initial sync timed out");

    let before = store.read().await.objects(&gvk);
    let victim = before
        .first()
        .expect("at least one pod")
        .metadata
        .name
        .clone()
        .unwrap();

    let pods: kube::Api<Pod> = kube::Api::namespaced(client, "demo");
    use kube::api::DeleteParams;
    let _ = pods.delete(&victim, &DeleteParams::default()).await;

    // The deployment replaces the pod, so assert the specific name disappears
    // rather than asserting on the count.
    let gone = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let names: Vec<String> = store
                .read()
                .await
                .objects(&gvk)
                .iter()
                .filter_map(|o| o.metadata.name.clone())
                .collect();
            if !names.contains(&victim) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(
        gone,
        "deleted pod {victim} never disappeared from the store"
    );
}
