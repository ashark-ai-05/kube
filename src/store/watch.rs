use crate::app::event::{AppEvent, WatchStatus};
use crate::store::cache::KindCache;
use futures::StreamExt;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube::runtime::watcher;
use kube::{Api, Client};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// All cached kinds plus their watch health.
pub struct ResourceStore {
    kinds: HashMap<GroupVersionKind, KindCache>,
    statuses: HashMap<GroupVersionKind, WatchStatus>,
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
    }

    pub fn objects(&self, gvk: &GroupVersionKind) -> Vec<Arc<DynamicObject>> {
        self.kinds.get(gvk).map(|c| c.objects()).unwrap_or_default()
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
}

pub type SharedStore = Arc<RwLock<ResourceStore>>;

/// Drive a watcher for one kind into the store, emitting an event after each delta.
///
/// `watcher` already handles relist-on-410-Gone internally, so this loop only
/// has to translate errors into visible status rather than reconnect by hand.
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

        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    let synced =
                        matches!(event, watcher::Event::InitDone | watcher::Event::Apply(_));
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
                Err(e) => {
                    store
                        .write()
                        .await
                        .set_status(gvk.clone(), WatchStatus::Reconnecting);
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status: WatchStatus::Reconnecting,
                    });
                    let _ = tx.send(AppEvent::Error(format!("watch {}: {e}", ar.kind)));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::runtime::watcher;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("default")
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
}
