//! Cluster-backed tests, marked #[ignore] so `cargo test` stays green on
//! machines with no cluster. Run them with:
//!   ./scripts/dev-cluster.sh && cargo test -- --ignored

use k8s_openapi::api::core::v1::{Pod, Secret};
use kube::api::{ApiResource, GroupVersionKind};
use kube_tui::app::event::{AppEvent, WatchStatus};
use kube_tui::cluster::discovery::discover_kinds;
use kube_tui::store::events::fetch_events;
use kube_tui::store::handles::WatchHandles;
use kube_tui::store::multi::KindAvailability;
use kube_tui::store::table::fetch_table;
use kube_tui::store::watch::{ResourceStore, SharedStore, spawn_watch};
use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard, RwLock, mpsc};

/// These tests share the `demo` namespace and its deployment — some assert a
/// pod count or its rendered columns, others delete a pod. Rust runs tests in
/// parallel by default, so they must be serialised against each other. A
/// mutex enforces this in the code rather than relying on someone remembering
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

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn fetch_table_returns_kubectl_equivalent_columns() {
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect()
        .await
        .expect("connect to cluster");

    // fetch_table is a hand-rolled raw HTTP request (kube 4.2 has no Table
    // support at all — see docs/superpowers/plan2-api-reference.md B4): it
    // sets `Accept: application/json;as=Table;v=1;g=meta.k8s.io` on a request
    // built by hand and hopes the server honours it. No unit test can catch a
    // drifted header, because decode_table only ever sees hand-written JSON
    // that already looks like a Table. This is the first time that header
    // actually leaves the process.
    let pods: kube::Api<Pod> = kube::Api::namespaced(client.clone(), "demo");
    let table = fetch_table(&client, pods.resource_url()).await.expect(
        "fetch_table failed — if the Accept header has drifted from what this \
         server accepts, it silently answers with an ordinary PodList instead \
         of a Table, and decode_table rejects that for having no \
         columnDefinitions",
    );

    let column_names: Vec<&str> = table.columns.iter().map(|c| c.name.as_str()).collect();
    for expected in ["Name", "Ready", "Status"] {
        assert!(
            column_names.contains(&expected),
            "expected a {expected} column among kubectl's own printer columns for \
             pods, got {column_names:?}"
        );
    }

    assert!(
        !table.rows.is_empty(),
        "expected at least one row for the demo namespace's pods, got none — \
         either the fixture is missing or the server returned an empty Table \
         despite reporting the right columns"
    );

    // decode_table pads ragged rows and truncates over-long ones so a real
    // row always matches the declared column count; confirm that held for
    // whatever the server actually sent, not just for decode_table's own
    // synthetic fixtures.
    for row in &table.rows {
        assert_eq!(
            row.cells.len(),
            table.columns.len(),
            "a real row's width must match the declared column count, got {row:?}"
        );
    }

    // fetch_table also requests includeObject=Metadata (appended to the URI
    // by hand — kube-core 4.2's ListParams has no field for it) so each row
    // carries the identity of the object it displays, rather than leaving
    // row selection to positionally match a separately-refreshed watch list.
    // This is the first time that parameter leaves the process too; no unit
    // test can confirm a real apiserver actually honours it.
    assert!(
        table.rows.iter().all(|r| r.identity.is_some()),
        "expected every row to carry an identity once includeObject=Metadata \
         was requested — if this fails, either the apiserver ignored the \
         parameter or decode_table's PartialObjectMetadata parsing drifted \
         from what it actually sent back, got {:?}",
        table.rows
    );
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn switching_clusters_aborts_the_previous_watch() {
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect().await.expect("connect");
    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");

    let handle = spawn_watch(
        client.clone(),
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx.clone(),
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

    let mut before: Vec<String> = store
        .read()
        .await
        .objects(&gvk)
        .iter()
        .filter_map(|o| o.metadata.name.clone())
        .collect();
    before.sort();
    let victim = before.first().cloned().expect("at least one demo pod");

    // Abort exactly the way a cluster switch does: register the watch's
    // `JoinHandle` in the same `WatchHandles` registry `Session` uses, then
    // call `abort_all()` — not `handle.abort()` directly — so this exercises
    // the real teardown path rather than a stand-in for it.
    let mut handles = WatchHandles::new();
    handles.push(handle);
    assert_eq!(
        handles.abort_all(),
        1,
        "the watch must actually be registered and aborted"
    );

    // Mutate the cluster: delete the pod the watch would have reported, were
    // it still alive. The `web` deployment immediately schedules a
    // replacement, so a LIVE watch would both drop `victim` from the store
    // and add the new pod within a few seconds — see
    // `store_reflects_a_deletion_made_during_the_watch` above, which asserts
    // exactly that disappearance, on a live watch, within 60s.
    let pods: kube::Api<Pod> = kube::Api::namespaced(client, "demo");
    use kube::api::DeleteParams;
    let _ = pods.delete(&victim, &DeleteParams::default()).await;

    // Give a live watch far more time than it would ever need to report this,
    // then assert nothing changed. If `abort_all()` had failed to stop the
    // task — the exact regression this test exists to catch — this would
    // fail the same way the sibling deletion test's `gone` check would:
    // `victim` would disappear and a replacement would appear in `after`.
    tokio::time::sleep(Duration::from_secs(10)).await;

    let mut after: Vec<String> = store
        .read()
        .await
        .objects(&gvk)
        .iter()
        .filter_map(|o| o.metadata.name.clone())
        .collect();
    after.sort();

    assert_eq!(
        before, after,
        "the store changed after its watch was aborted — a delta reached it \
         when abort_all() should have stopped delivery entirely"
    );
    assert!(
        after.contains(&victim),
        "the deleted pod must still be listed: an aborted watch cannot have \
         reported its removal"
    );
}

// --- Task 10: discovery, events, and RBAC against a real cluster ---

/// `gvk` as a comparable, printable key. `GroupVersionKind` is `Eq + Hash` but
/// not `Ord`, and these tests compare whole SETS of kinds and print the
/// difference when they disagree, which needs a stable ordering.
fn gvk_key(gvk: &GroupVersionKind) -> String {
    format!("{}/{}/{}", gvk.group, gvk.version, gvk.kind)
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn discovery_finds_the_core_workload_kinds() {
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect()
        .await
        .expect("connect to cluster");

    let kinds = discover_kinds(&client)
        .await
        .expect("discovery must succeed");
    let found: BTreeSet<String> = kinds.iter().map(|k| gvk_key(&k.gvk)).collect();

    // The three every operator opens the tool for. Deployment also proves the
    // walk does not stop at the core group, and Service proves the filter is
    // not requiring `deletecollection` — Services are one of the few builtin
    // resources that do not support it.
    for expected in ["/v1/Pod", "apps/v1/Deployment", "/v1/Service"] {
        assert!(
            found.contains(expected),
            "expected {expected} among the browsable kinds; got {} kinds: {:?}",
            found.len(),
            found
        );
    }

    // Browsable means the watch machinery can actually be pointed at it, so
    // each one must carry the `ApiResource` `spawn_watch` needs, with the
    // plural the apiserver uses in its own URLs.
    let pod = kinds
        .iter()
        .find(|k| gvk_key(&k.gvk) == "/v1/Pod")
        .expect("checked above");
    assert_eq!(pod.resource.plural, "pods");
    assert!(pod.namespaced, "pods are namespaced");
    assert_eq!(
        pod.group_label, "core",
        "the empty group must read as 'core'"
    );

    let node = kinds.iter().find(|k| gvk_key(&k.gvk) == "/v1/Node");
    assert!(
        node.is_some_and(|n| !n.namespaced),
        "a cluster-scoped kind must be reported as such, or the watch is \
         started with the wrong Api constructor"
    );
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn discovery_returns_every_kind_the_server_says_is_listable_and_watchable() {
    // The over-filter guard. `discover_kinds` needs a cluster, so tightening
    // its predicate — the review's example was additionally requiring `get`,
    // which would silently hide kinds — leaves the entire unit suite green.
    //
    // A happy-path check that Pods and Deployments are present cannot catch
    // that either: they support every verb. So this asserts SET EQUALITY
    // against the contract stated literally — "the server advertises list and
    // watch" — recomputed here from the raw discovery response rather than by
    // calling `is_browsable`, which would move with the implementation and
    // prove nothing.
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect()
        .await
        .expect("connect to cluster");

    let discovery = kube::discovery::Discovery::new(client.clone())
        .run()
        .await
        .expect("discovery must succeed");

    let mut expected: BTreeSet<String> = BTreeSet::new();
    // Which verbs, if wrongly required, this cluster could actually prove
    // wrong: a verb every browsable kind happens to support is one no test
    // run against THIS cluster can discriminate, and saying so is more useful
    // than implying the check is total.
    let candidates = [
        "get",
        "create",
        "update",
        "patch",
        "delete",
        "deletecollection",
    ];
    let mut discriminable: BTreeSet<&str> = BTreeSet::new();
    for group in discovery.groups() {
        for (resource, caps) in group.recommended_resources() {
            let listable = caps.operations.iter().any(|op| op == "list");
            let watchable = caps.operations.iter().any(|op| op == "watch");
            if listable && watchable {
                expected.insert(gvk_key(&GroupVersionKind::gvk(
                    group.name(),
                    &resource.version,
                    &resource.kind,
                )));
                for verb in candidates {
                    if !caps.operations.iter().any(|op| op == verb) {
                        discriminable.insert(verb);
                    }
                }
            }
        }
    }

    let kinds = discover_kinds(&client)
        .await
        .expect("discovery must succeed");
    let got: BTreeSet<String> = kinds.iter().map(|k| gvk_key(&k.gvk)).collect();

    let missing: Vec<&String> = expected.difference(&got).collect();
    assert!(
        missing.is_empty(),
        "discover_kinds dropped {} kind(s) the server says are listable AND \
         watchable — an over-filter hides them from the sidebar with no error \
         anywhere: {missing:?}",
        missing.len()
    );
    let extra: Vec<&String> = got.difference(&expected).collect();
    assert!(
        extra.is_empty(),
        "discover_kinds returned {} kind(s) the server does NOT say are both \
         listable and watchable — an under-filter puts a kind in the sidebar \
         whose count can never update: {extra:?}",
        extra.len()
    );
    assert!(
        expected.len() >= 20,
        "a stock cluster serves dozens of browsable kinds; only {} means \
         discovery is barely walking the groups at all and this comparison \
         proves little",
        expected.len()
    );
    assert!(
        discriminable.contains("deletecollection"),
        "this cluster serves no browsable kind lacking `deletecollection`, so \
         this test could not tell an over-filter on it from a correct one. \
         Verbs it CAN discriminate: {discriminable:?}. If `get` is missing \
         from that list, a `get` requirement is invisible here — that is a \
         property of the cluster, not of the check."
    );
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn events_for_a_real_pod_are_returned() {
    // `fetch_events` builds a field selector by hand
    // (`involvedObject.name=…,involvedObject.namespace=…`) and hands it to the
    // apiserver. A selector the server does not understand is rejected; one it
    // understands but that matches nothing comes back as an empty list —
    // indistinguishable, in `decode`-level unit tests, from a healthy object
    // with nothing to report. Only a real object with real events tells the
    // two apart.
    //
    // A freshly-created pod, not one of the `web` deployment's: Kubernetes
    // expires events after an hour by default, so a long-lived kind cluster's
    // original pods reliably have none.
    let _serial = cluster_lock().await;
    let client = kube_tui::cluster::connect().await.expect("connect");
    let pods: kube::Api<Pod> = kube::Api::namespaced(client.clone(), "demo");

    let name = "events-probe";
    let spec: Pod = serde_json::from_value(serde_json::json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": name },
        "spec": {
            "containers": [{ "name": "probe", "image": "nginx:alpine" }],
            // Nothing here waits for it to become Ready — scheduling alone
            // produces the events this asserts on.
            "terminationGracePeriodSeconds": 0,
        }
    }))
    .expect("a well-formed pod manifest");

    use kube::api::{DeleteParams, PostParams};
    // Left over from an interrupted previous run, if any.
    let _ = pods.delete(name, &DeleteParams::default()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    pods.create(&PostParams::default(), &spec)
        .await
        .expect("creating the probe pod");

    let rows = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match fetch_events(&client, "demo", name).await {
                Ok(rows) if !rows.is_empty() => return rows,
                Ok(_) => {}
                Err(e) => panic!("fetch_events failed against a real cluster: {e:#}"),
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;

    // Clean up before asserting, so a failure does not leave the pod behind
    // for the next run to trip over.
    let _ = pods.delete(name, &DeleteParams::default()).await;

    let rows = rows.expect(
        "no events for a pod created 90 seconds ago — either the field \
         selector does not match what the apiserver indexes, or events are \
         disabled on this cluster",
    );
    assert!(
        rows.iter().any(|r| !r.reason.is_empty()),
        "every event has a reason (Scheduled, Pulling, …); rows with none \
         mean the Event fields are being read from the wrong place: {rows:?}"
    );
    assert!(
        rows.iter().all(|r| r.age != "?"),
        "'?' is the honest fallback for an event with none of its three \
         timestamps set — a real pod's events always have at least one, so \
         this means `event_age` is reading fields the server does not \
         populate: {rows:?}"
    );
}

/// A `Client` authenticating as the `restricted` ServiceAccount that
/// `dev-cluster.sh` creates: list/watch on pods in `demo`, nothing else.
///
/// Built in-process from the token Secret rather than by adding a context to
/// the user's kubeconfig, so running the test leaves nothing behind.
async fn restricted_client(admin: &kube::Client) -> kube::Client {
    let secrets: kube::Api<Secret> = kube::Api::namespaced(admin.clone(), "demo");
    // The token controller fills `.data.token` in shortly after the Secret is
    // applied, so a freshly-created cluster can race this.
    let token = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(s) = secrets.get("restricted-token").await
                && let Some(data) = s.data.as_ref()
                && let Some(bytes) = data.get("token")
            {
                return String::from_utf8(bytes.0.clone()).expect("a token is ASCII");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect(
        "the restricted-token Secret was never populated — re-run \
         ./scripts/dev-cluster.sh, which creates it",
    );

    let mut config = kube::Config::infer()
        .await
        .expect("inferring the cluster's own connection details");
    // Same server and same CA, different identity.
    config.auth_info = kube::config::AuthInfo {
        token: Some(token.into()),
        ..Default::default()
    };
    kube::Client::try_from(config).expect("building a client performs no I/O")
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn a_forbidden_kind_is_marked_unavailable_not_retried_forever() {
    // On a corporate cluster, lacking RBAC on some kinds is the normal case.
    // Two things must hold, and only a real apiserver can produce the 403 that
    // proves either: the sidebar must be able to say WHY (so the kind does not
    // read as merely empty), and the watch must stop — a forbidden watch that
    // retries for ever is a permanent load on the API server and a permanent
    // "reconnecting" in the UI.
    let _serial = cluster_lock().await;
    let admin = kube_tui::cluster::connect().await.expect("connect");
    let restricted = restricted_client(&admin).await;

    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let secret_gvk = GroupVersionKind::gvk("", "v1", "Secret");

    let handle = spawn_watch(
        restricted.clone(),
        ApiResource::erase::<Secret>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx.clone(),
    );

    let failed = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = rx.recv().await {
            if let AppEvent::WatchStatus {
                status: WatchStatus::Failed,
                gvk,
            } = ev
                && gvk == secret_gvk
            {
                return true;
            }
        }
        false
    })
    .await
    .expect("the forbidden watch never reported Failed within 30s");
    assert!(failed);

    match store.read().await.availability(&secret_gvk) {
        KindAvailability::Unavailable { reason } => assert!(
            !reason.is_empty(),
            "the sidebar shows this reason instead of a count; an empty one \
             reads as 'this kind is empty'"
        ),
        other => panic!("expected Unavailable with the apiserver's reason, got {other:?}"),
    }

    // Stopped, not merely quiet: `drive_watch` RETURNS on a permanent failure,
    // so the task completes. Anything else — a `break` into the retry loop, a
    // reclassification of 403 as retryable — leaves it running.
    for _ in 0..40 {
        if handle.is_finished() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        handle.is_finished(),
        "the watch is still running ten seconds after a 403 — a forbidden \
         watch that retries for ever is permanent load on the API server"
    );

    // The control, without which the assertions above would also pass for a
    // client that simply could not reach the cluster at all: the SAME
    // restricted identity CAN watch pods, and does.
    let pod_gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let pod_store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (ptx, mut prx) = mpsc::unbounded_channel::<AppEvent>();
    let mut pod_handles = WatchHandles::new();
    pod_handles.push(spawn_watch(
        restricted,
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        pod_store.clone(),
        ptx,
    ));
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = prx.recv().await {
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
    .expect("the restricted identity must still be able to watch pods");
    assert_eq!(
        pod_store.read().await.availability(&pod_gvk),
        KindAvailability::Watching,
        "a kind the identity CAN watch must not be marked unavailable"
    );
    pod_handles.abort_all();
}
