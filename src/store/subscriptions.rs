//! Demand-driven watches. Discovery does not imply subscribing to every kind.
use crate::app::{event::AppEvent, session::SharedSession};
use crate::cluster::{
    self,
    discovery::{KindInfo, discover_kinds},
};
use crate::store::{
    multi::KindAvailability,
    watch::{SharedStore, spawn_watch},
};
use futures::{StreamExt, stream::FuturesUnordered};
use kube::{Client, api::GroupVersionKind};
use std::collections::{HashMap, VecDeque};
use tokio::{
    sync::mpsc::UnboundedSender,
    task::{AbortHandle, JoinHandle},
};

pub const RECENT_WATCHES: usize = 8;

/// Keeps recently visited kinds warm, without a permanent exclusion list.
#[derive(Default)]
pub struct RecentKinds(VecDeque<GroupVersionKind>);
impl RecentKinds {
    pub fn touch(&mut self, kind: GroupVersionKind) -> Option<GroupVersionKind> {
        self.0.retain(|k| k != &kind);
        self.0.push_back(kind);
        (self.0.len() > RECENT_WATCHES)
            .then(|| self.0.pop_front())
            .flatten()
    }
}

pub fn scope_for(kind: &KindInfo, namespace: &Option<String>) -> Option<String> {
    if kind.namespaced {
        namespace.clone()
    } else {
        None
    }
}

struct Cancel(AbortHandle);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub fn spawn(
    session: SharedSession,
    client: Client,
    store: SharedStore,
    namespace: Option<String>,
    tx: UnboundedSender<AppEvent>,
    initial: KindInfo,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut requested = store.read().await.requested.subscribe();
        let mut recent = RecentKinds::default();
        let mut running = FuturesUnordered::new();
        let mut cancels: HashMap<GroupVersionKind, Cancel> = HashMap::new();
        let handle = spawn_watch(
            client.clone(),
            initial.resource.clone(),
            scope_for(&initial, &namespace),
            store.clone(),
            tx.clone(),
        );
        cancels.insert(initial.gvk.clone(), Cancel(handle.abort_handle()));
        running.push(handle);
        recent.touch(initial.gvk.clone());
        let kinds = match discover_kinds(&client).await {
            Ok(kinds) if !kinds.is_empty() => kinds,
            result => {
                if let Err(e) = result {
                    let _ = tx.send(AppEvent::Error(format!(
                        "Discovery: {}",
                        cluster::safe_error_text(&e)
                    )));
                }
                vec![initial.clone()]
            }
        };
        {
            let mut s = session.lock().await;
            if !std::sync::Arc::ptr_eq(&s.store, &store) {
                return;
            }
            s.kinds = kinds.clone();
        }
        {
            let mut s = store.write().await;
            for k in &kinds {
                if k.gvk != initial.gvk {
                    s.set_availability(k.gvk.clone(), KindAvailability::NotWatched);
                }
            }
        }
        let _ = tx.send(AppEvent::KindsDiscovered);
        loop {
            let wanted = requested.borrow_and_update().clone();
            if let Some(kind) = wanted.and_then(|gvk| kinds.iter().find(|k| k.gvk == gvk)) {
                if let Some(old) = recent.touch(kind.gvk.clone()) {
                    cancels.remove(&old);
                    store.write().await.evict(&old);
                }
                if !cancels.contains_key(&kind.gvk) {
                    store
                        .write()
                        .await
                        .set_availability(kind.gvk.clone(), KindAvailability::Watching);
                    let handle = spawn_watch(
                        client.clone(),
                        kind.resource.clone(),
                        scope_for(kind, &namespace),
                        store.clone(),
                        tx.clone(),
                    );
                    cancels.insert(kind.gvk.clone(), Cancel(handle.abort_handle()));
                    running.push(handle);
                }
            }
            tokio::select! {
                r = requested.changed() => { if r.is_err() { break; } }
                Some(result) = running.next(), if !running.is_empty() => {
                    if let Err(e) = result && !e.is_cancelled() {
                        let _ = tx.send(AppEvent::Error(format!("Watch task failed: {e}")));
                        let _ = tx.send(AppEvent::Quit);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::ApiResource;
    fn g(i: usize) -> GroupVersionKind {
        GroupVersionKind::gvk("test", "v1", &format!("Kind{i}"))
    }
    #[test]
    fn keeps_eight_recent_kinds_and_can_visit_any_discovered_kind() {
        let mut recent = RecentKinds::default();
        for i in 0..RECENT_WATCHES {
            assert_eq!(recent.touch(g(i)), None);
        }
        assert_eq!(recent.touch(g(0)), None);
        assert_eq!(recent.touch(g(500)), Some(g(1)));
        assert_eq!(recent.touch(g(1)), Some(g(2)));
    }
    #[test]
    fn cluster_scoped_resources_ignore_namespace_selection() {
        let kind = KindInfo {
            gvk: g(0),
            resource: ApiResource {
                group: "".into(),
                version: "v1".into(),
                api_version: "v1".into(),
                kind: "Node".into(),
                plural: "nodes".into(),
            },
            namespaced: false,
            group_label: "core".into(),
        };
        assert_eq!(scope_for(&kind, &Some("demo".into())), None);
        let namespaced = KindInfo {
            namespaced: true,
            ..kind
        };
        assert_eq!(
            scope_for(&namespaced, &Some("demo".into())),
            Some("demo".into())
        );
    }
}
