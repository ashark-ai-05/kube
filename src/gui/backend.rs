//! Cluster I/O helpers shared by the iced GUI. Reuses store + session + discovery.

use crate::app::event::{AppEvent, FetchedEvents};
use crate::app::session::{SharedSession, is_deliberate_abort};
use crate::cluster;
use crate::cluster::discovery::{KindInfo, discover_kinds, group_label_for};
use crate::store::events::fetch_events;
use crate::store::multi::{DEFAULT_MAX_EAGER_WATCHES, KindAvailability, kinds_to_watch, prioritise};
use crate::store::table::fetch_table;
use crate::store::watch::{StoreId, spawn_watch};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind, LogParams};
use kube::Client;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const MAX_ERROR_CHARS: usize = 240;

pub fn truncate_error(e: String) -> String {
    if e.chars().count() <= MAX_ERROR_CHARS {
        return e;
    }
    let mut out: String = e.chars().take(MAX_ERROR_CHARS).collect();
    out.push('…');
    out
}

struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn supervise_children(children: Vec<tokio::task::JoinHandle<()>>, tx: mpsc::UnboundedSender<AppEvent>) {
    let mut running: futures::stream::FuturesUnordered<_> = children.into_iter().collect();
    while let Some(result) = running.next().await {
        match result {
            Ok(()) => {}
            Err(e) if is_deliberate_abort(&e) => {}
            Err(e) => {
                let _ = tx.send(AppEvent::Error(format!("watch task failed: {e}")));
                let _ = tx.send(AppEvent::Quit);
            }
        }
    }
}

pub fn pod_kind() -> KindInfo {
    let resource = ApiResource::erase::<Pod>(&());
    KindInfo {
        gvk: GroupVersionKind::gvk("", "v1", "Pod"),
        namespaced: true,
        group_label: group_label_for(&resource.group),
        resource,
    }
}

pub fn spawn_discovery_and_watches(
    session: SharedSession,
    client: Client,
    store: crate::store::watch::SharedStore,
    namespace: Option<String>,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let kinds: Vec<KindInfo> = match discover_kinds(&client).await {
            Ok(kinds) if !kinds.is_empty() => kinds,
            Ok(_) => vec![pod_kind()],
            Err(e) => {
                let _ = tx.send(AppEvent::Error(truncate_error(format!(
                    "discovering kinds: {} — showing pods only",
                    cluster::safe_error_text(&e)
                ))));
                vec![pod_kind()]
            }
        };

        let mut ranked = kinds.clone();
        prioritise(&mut ranked);
        let (watched, skipped) = kinds_to_watch(&ranked, DEFAULT_MAX_EAGER_WATCHES);
        let watched_gvks: HashSet<GroupVersionKind> = watched.iter().map(|k| k.gvk.clone()).collect();
        let resources: Vec<ApiResource> = watched.iter().map(|k| k.resource.clone()).collect();

        {
            let mut s = session.lock().await;
            if !Arc::ptr_eq(&s.store, &store) {
                return;
            }
            s.kinds = kinds.clone();
        }

        {
            let mut s = store.write().await;
            for kind in &kinds {
                if !watched_gvks.contains(&kind.gvk) {
                    s.set_availability(kind.gvk.clone(), KindAvailability::NotWatched);
                }
            }
        }
        if skipped > 0 {
            let _ = tx.send(AppEvent::Error(format!(
                "{skipped} of {} kinds are not being watched (cap {DEFAULT_MAX_EAGER_WATCHES})",
                kinds.len()
            )));
        }
        let _ = tx.send(AppEvent::KindsDiscovered);

        let mut children = Vec::with_capacity(resources.len());
        let mut cancel_children = Vec::with_capacity(resources.len());
        for resource in resources {
            let handle = spawn_watch(
                client.clone(),
                resource,
                namespace.clone(),
                store.clone(),
                tx.clone(),
            );
            cancel_children.push(AbortOnDrop(handle.abort_handle()));
            children.push(handle);
        }

        supervise_children(children, tx).await;
        drop(cancel_children);
    })
}

pub fn spawn_table_fetch(
    client: Client,
    resource: ApiResource,
    namespace: Option<String>,
    gvk: GroupVersionKind,
    store: crate::store::watch::SharedStore,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        store.write().await.note_table_fetch(gvk.clone(), std::time::Instant::now());
        let api: Api<DynamicObject> = match namespace.as_deref() {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
            None => Api::all_with(client.clone(), &resource),
        };
        let url = api.resource_url().to_string();
        match fetch_table(&client, &url).await {
            Ok(data) => {
                store.write().await.set_table_data(gvk.clone(), data);
                let _ = tx.send(AppEvent::StoreChanged { gvk });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::Error(truncate_error(format!(
                    "fetching {} columns: {}",
                    gvk.kind,
                    cluster::safe_error_text(&e)
                ))));
            }
        }
    });
}

pub fn spawn_events_fetch(
    client: Client,
    gvk: GroupVersionKind,
    namespace: Option<String>,
    name: String,
    store: StoreId,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let result = fetch_events(&client, namespace.as_deref().unwrap_or(""), &name)
            .await
            .map_err(|e| truncate_error(cluster::safe_error_text(&e)));
        let _ = tx.send(AppEvent::EventsFetched(FetchedEvents {
            gvk,
            namespace,
            name,
            store,
            result,
        }));
    });
}

pub fn spawn_refetch_wake(tx: mpsc::UnboundedSender<AppEvent>, after: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let _ = tx.send(AppEvent::Wake);
    });
}

pub async fn fetch_pod_logs(client: Client, namespace: Option<String>, name: String) -> String {
    let ns = namespace.unwrap_or_else(|| "default".into());
    let api: Api<Pod> = Api::namespaced(client, &ns);
    let params = LogParams {
        tail_lines: Some(200),
        timestamps: true,
        ..Default::default()
    };
    match api.logs(&name, &params).await {
        Ok(s) => s,
        Err(e) => crate::cluster::safe_source_text(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_error_caps_length() {
        let long = "x".repeat(500);
        let out = truncate_error(long);
        assert_eq!(out.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(out.ends_with('…'));
    }
}
