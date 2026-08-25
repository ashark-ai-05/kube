use indexmap::IndexMap;
use kube::api::{ApiResource, DynamicObject, ResourceExt};
use kube::runtime::watcher;
use std::sync::Arc;

/// Identity of an object within a kind: (namespace, name).
pub type ObjKey = (Option<String>, String);

pub fn key_of(obj: &DynamicObject) -> ObjKey {
    (obj.namespace(), obj.name_any())
}

/// In-memory cache of one kind, driven by watcher deltas.
///
/// `Arc` lets the UI clone pointers rather than objects; rendering 5,000 rows
/// copies 5,000 pointers. `IndexMap` gives stable iteration order for rendering
/// plus O(1) keyed lookup.
pub struct KindCache {
    resource: ApiResource,
    objects: IndexMap<ObjKey, Arc<DynamicObject>>,
    /// Staging buffer for an in-progress resync. `Some` between Init and InitDone.
    init_buffer: Option<IndexMap<ObjKey, Arc<DynamicObject>>>,
}

impl KindCache {
    pub fn new(resource: ApiResource) -> Self {
        Self {
            resource,
            objects: IndexMap::new(),
            init_buffer: None,
        }
    }

    pub fn resource(&self) -> &ApiResource {
        &self.resource
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn objects(&self) -> Vec<Arc<DynamicObject>> {
        self.objects.values().cloned().collect()
    }

    pub fn get(&self, key: &ObjKey) -> Option<&Arc<DynamicObject>> {
        self.objects.get(key)
    }

    /// Fold one watcher delta into the cache.
    ///
    /// Init/InitApply/InitDone form a resync: objects accumulate in a staging
    /// buffer and replace the live map atomically on InitDone. Applying them
    /// directly would leave objects that were deleted while disconnected
    /// visible forever, since no Delete is ever emitted for them.
    pub fn apply(&mut self, event: watcher::Event<DynamicObject>) {
        match event {
            watcher::Event::Apply(obj) => {
                self.objects.insert(key_of(&obj), Arc::new(obj));
            }
            watcher::Event::Delete(obj) => {
                self.objects.shift_remove(&key_of(&obj));
            }
            watcher::Event::Init => {
                self.init_buffer = Some(IndexMap::new());
            }
            watcher::Event::InitApply(obj) => {
                if let Some(buf) = self.init_buffer.as_mut() {
                    buf.insert(key_of(&obj), Arc::new(obj));
                } else {
                    // InitApply without Init: tolerate rather than lose data.
                    self.objects.insert(key_of(&obj), Arc::new(obj));
                }
            }
            watcher::Event::InitDone => {
                if let Some(buf) = self.init_buffer.take() {
                    self.objects = buf;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;
    use kube::runtime::watcher;

    fn pod_ar() -> ApiResource {
        ApiResource::erase::<Pod>(&())
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &pod_ar()).within("default")
    }

    fn names(cache: &KindCache) -> Vec<String> {
        let mut n: Vec<String> = cache.objects().iter().map(|o| o.name_any()).collect();
        n.sort();
        n
    }

    #[test]
    fn apply_inserts_an_object() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        assert_eq!(names(&c), vec!["a"]);
    }

    #[test]
    fn apply_twice_updates_rather_than_duplicates() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("a")));
        assert_eq!(
            c.len(),
            1,
            "same namespace+name must replace, not duplicate"
        );
    }

    #[test]
    fn delete_removes_an_object() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("b")));
        c.apply(watcher::Event::Delete(pod("a")));
        assert_eq!(names(&c), vec!["b"]);
    }

    #[test]
    fn deleting_an_unknown_object_is_a_no_op() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Delete(pod("ghost")));
        assert!(c.is_empty());
    }

    #[test]
    fn objects_stay_visible_during_a_resync() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        assert_eq!(names(&c), vec!["a"], "must not blank the view mid-resync");
    }

    #[test]
    fn resync_drops_objects_deleted_while_disconnected() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("stale")));

        // Reconnect: the server reports only "a" still exists.
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        c.apply(watcher::Event::InitDone);

        assert_eq!(
            names(&c),
            vec!["a"],
            "'stale' was deleted while disconnected"
        );
    }

    #[test]
    fn resync_adds_objects_created_while_disconnected() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        c.apply(watcher::Event::InitApply(pod("new")));
        c.apply(watcher::Event::InitDone);
        assert_eq!(names(&c), vec!["a", "new"]);
    }

    #[test]
    fn empty_resync_clears_everything() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitDone);
        assert!(
            c.is_empty(),
            "server reported no objects, so cache must be empty"
        );
    }

    #[test]
    fn objects_in_different_namespaces_do_not_collide() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(
            DynamicObject::new("a", &pod_ar()).within("ns1"),
        ));
        c.apply(watcher::Event::Apply(
            DynamicObject::new("a", &pod_ar()).within("ns2"),
        ));
        assert_eq!(c.len(), 2, "namespace is part of identity");
    }

    #[test]
    fn a_second_init_discards_a_partial_first_resync() {
        // A failed list/watch attempt re-emits Init after some InitApply events
        // have already arrived (kube-runtime resets InitPage/InitialWatch errors
        // to Empty). The partial attempt's objects must not leak into the retry.
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("live")));

        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("partial")));

        // Connection drops mid-resync; the watcher restarts the sequence.
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("real")));
        c.apply(watcher::Event::InitDone);

        assert_eq!(
            names(&c),
            vec!["real"],
            "objects from the abandoned first resync must not survive"
        );
    }
}
