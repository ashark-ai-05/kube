//! Relationships follow owner UIDs and selectors, never name prefixes.
use crate::cluster::discovery::KindInfo;
use anyhow::{Result, anyhow};
use k8s_openapi::{api::core::v1::Pod, apimachinery::pkg::apis::meta::v1::LabelSelector};
use kube::{
    Api, Client, ResourceExt,
    api::{ApiResource, DynamicObject, GroupVersionKind, ListParams},
    core::Selector,
};
use std::collections::HashSet;

pub fn kind(obj: &DynamicObject) -> &str {
    obj.types
        .as_ref()
        .map(|t| t.kind.as_str())
        .unwrap_or("Resource")
}
pub fn owned_by(obj: &DynamicObject, uid: &str) -> bool {
    !uid.is_empty()
        && obj
            .metadata
            .owner_references
            .as_ref()
            .is_some_and(|refs| refs.iter().any(|r| r.uid == uid))
}
pub fn selector(obj: &DynamicObject) -> Result<String> {
    let value = obj
        .data
        .pointer("/spec/selector")
        .ok_or_else(|| anyhow!("{} has no pod selector", kind(obj)))?;
    let selector: LabelSelector = if kind(obj) == "Service" {
        LabelSelector {
            match_labels: Some(serde_json::from_value(value.clone())?),
            ..Default::default()
        }
    } else {
        serde_json::from_value(value.clone())?
    };
    let text = Selector::try_from(selector)?.to_string();
    if text.is_empty() {
        return Err(anyhow!(
            "This resource has no selector; refusing to select every pod"
        ));
    }
    Ok(text)
}

pub async fn list(
    client: Client,
    kind: &KindInfo,
    ns: Option<&str>,
    mut params: ListParams,
) -> Result<Vec<DynamicObject>> {
    let api: Api<DynamicObject> = match ns.filter(|_| kind.namespaced) {
        Some(ns) => Api::namespaced_with(client, ns, &kind.resource),
        None => Api::all_with(client, &kind.resource),
    };
    params.limit = Some(500);
    let mut out = Vec::new();
    loop {
        let page = api.list(&params).await?;
        out.extend(page.items.into_iter().map(|mut object| {
            object.types.get_or_insert_with(|| kube::core::TypeMeta {
                api_version: kind.resource.api_version.clone(),
                kind: kind.gvk.kind.clone(),
            });
            object
        }));
        match page.metadata.continue_.filter(|s| !s.is_empty()) {
            Some(next) => params.continue_token = Some(next),
            None => break,
        }
    }
    Ok(out)
}

pub async fn pods_for(client: Client, obj: &DynamicObject) -> Result<Vec<Pod>> {
    let ns = obj
        .namespace()
        .ok_or_else(|| anyhow!("Select a pod or namespaced workload for logs"))?;
    let api: Api<Pod> = Api::namespaced(client.clone(), &ns);
    if kind(obj) == "Pod" {
        let pod = api.get(&obj.name_any()).await?;
        if obj.uid().is_some() && obj.uid() != pod.uid() {
            return Err(anyhow!("The selected pod was replaced; reopen it"));
        }
        return Ok(vec![pod]);
    }
    if !matches!(
        kind(obj),
        "Deployment" | "ReplicaSet" | "StatefulSet" | "DaemonSet" | "Job" | "CronJob" | "Service"
    ) {
        return Err(anyhow!(
            "Logs are available for pods, workloads and services"
        ));
    }
    if kind(obj) == "CronJob" {
        let jobs: Api<k8s_openapi::api::batch::v1::Job> = Api::namespaced(client, &ns);
        let uid = obj.uid().unwrap_or_default();
        let jobs = jobs.list(&ListParams::default()).await?;
        let uids: HashSet<_> = jobs
            .items
            .iter()
            .filter(|job| job.owner_references().iter().any(|r| r.uid == uid))
            .filter_map(|j| j.uid())
            .collect();
        let pods = api.list(&ListParams::default()).await?.items;
        return Ok(pods
            .into_iter()
            .filter(|p| p.owner_references().iter().any(|r| uids.contains(&r.uid)))
            .collect());
    }
    let labels = selector(obj)?;
    Ok(api
        .list(&ListParams::default().labels(&labels))
        .await?
        .items)
}

pub struct Related {
    pub objects: Vec<DynamicObject>,
    pub notes: Vec<String>,
}

pub async fn fetch_related(client: Client, obj: &DynamicObject, kinds: &[KindInfo]) -> Related {
    let mut result = Related {
        objects: vec![],
        notes: vec![],
    };
    let ns = obj.namespace();
    let uid = obj.uid().unwrap_or_default();
    // Fetch the exact owner reference, including API group and UID.
    for owner in obj.owner_references() {
        if let Some(k) = kinds
            .iter()
            .find(|k| k.resource.api_version == owner.api_version && k.gvk.kind == owner.kind)
        {
            let api: Api<DynamicObject> = match ns.as_deref().filter(|_| k.namespaced) {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &k.resource),
                None => Api::all_with(client.clone(), &k.resource),
            };
            match api.get(&owner.name).await {
                Ok(parent) if parent.uid().as_deref() == Some(&owner.uid) => {
                    result.objects.push(parent)
                }
                Ok(_) => result
                    .notes
                    .push(format!("Owner {} has been replaced", owner.name)),
                Err(e) => result.notes.push(crate::cluster::safe_source_text(&e)),
            }
        }
    }
    let child_kinds: &[&str] = match kind(obj) {
        "Deployment" => &["ReplicaSet"],
        "CronJob" => &["Job"],
        "ReplicaSet" | "StatefulSet" | "DaemonSet" | "Job" => &["Pod"],
        "Service" => &["EndpointSlice"],
        "Node" => &["Pod"],
        _ => &[],
    };
    for child_kind in child_kinds {
        if let Some(k) = kinds.iter().find(|k| k.gvk.kind == *child_kind) {
            let params = if kind(obj) == "Service" {
                ListParams::default()
                    .labels(&format!("kubernetes.io/service-name={}", obj.name_any()))
            } else if kind(obj) == "Node" {
                ListParams::default().fields(&format!("spec.nodeName={}", obj.name_any()))
            } else {
                ListParams::default()
            };
            match list(client.clone(), k, ns.as_deref(), params).await {
                Ok(objects) => result.objects.extend(
                    objects
                        .into_iter()
                        .filter(|o| matches!(kind(obj), "Service" | "Node") || owned_by(o, &uid)),
                ),
                Err(e) => result.notes.push(crate::cluster::safe_error_text(&e)),
            }
        }
    }
    if matches!(kind(obj), "Deployment" | "CronJob" | "Service") {
        match pods_for(client.clone(), obj).await {
            Ok(pods) => {
                for p in pods {
                    if let Ok(o) = serde_json::to_value(p).and_then(serde_json::from_value) {
                        result.objects.push(o);
                    }
                }
            }
            Err(e) => result.notes.push(crate::cluster::safe_error_text(&e)),
        }
    }
    let mut refs: Vec<(&str, String)> = vec![];
    if kind(obj) == "Pod" {
        for (path, target_kind) in [
            ("/spec/nodeName", "Node"),
            ("/spec/serviceAccountName", "ServiceAccount"),
        ] {
            if let Some(name) = obj.data.pointer(path).and_then(|v| v.as_str()) {
                refs.push((target_kind, name.into()));
            }
        }
        if let Some(volumes) = obj.data.pointer("/spec/volumes").and_then(|v| v.as_array()) {
            for v in volumes {
                for (path, target_kind) in [
                    ("/persistentVolumeClaim/claimName", "PersistentVolumeClaim"),
                    ("/configMap/name", "ConfigMap"),
                    ("/secret/secretName", "Secret"),
                ] {
                    if let Some(name) = v.pointer(path).and_then(|v| v.as_str()) {
                        refs.push((target_kind, name.into()));
                    }
                }
            }
        }
    }
    if kind(obj) == "PersistentVolumeClaim"
        && let Some(name) = obj
            .data
            .pointer("/spec/volumeName")
            .and_then(|v| v.as_str())
    {
        refs.push(("PersistentVolume", name.into()));
    }
    if kind(obj) == "Ingress" {
        fn services(value: &serde_json::Value, out: &mut Vec<String>) {
            if let Some(name) = value.pointer("/service/name").and_then(|v| v.as_str()) {
                out.push(name.into());
            }
            match value {
                serde_json::Value::Object(m) => {
                    for v in m.values() {
                        services(v, out);
                    }
                }
                serde_json::Value::Array(a) => {
                    for v in a {
                        services(v, out);
                    }
                }
                _ => {}
            }
        }
        let mut names = vec![];
        services(&obj.data, &mut names);
        refs.extend(names.into_iter().map(|name| ("Service", name)));
    }
    for (target_kind, name) in refs {
        if let Some(k) = kinds.iter().find(|k| k.gvk.kind == target_kind) {
            let api: Api<DynamicObject> = match ns.as_deref().filter(|_| k.namespaced) {
                Some(ns) => Api::namespaced_with(client.clone(), ns, &k.resource),
                None => Api::all_with(client.clone(), &k.resource),
            };
            match api.get(&name).await {
                Ok(o) => result.objects.push(o),
                Err(e) => result.notes.push(crate::cluster::safe_source_text(&e)),
            }
        }
    }
    let mut seen = HashSet::new();
    result
        .objects
        .retain(|o| seen.insert((kind(o).to_string(), o.namespace(), o.name_any())));
    result
        .objects
        .sort_by_key(|o| (kind(o).to_string(), o.name_any()));
    result
}

pub fn resource_for(obj: &DynamicObject, kinds: &[KindInfo]) -> Result<ApiResource> {
    let typ = obj
        .types
        .as_ref()
        .ok_or_else(|| anyhow!("Resource has no type metadata"))?;
    kinds
        .iter()
        .find(|k| k.resource.api_version == typ.api_version && k.gvk.kind == typ.kind)
        .map(|k| k.resource.clone())
        .ok_or_else(|| anyhow!("Resource kind is no longer available"))
}
pub fn gvk(obj: &DynamicObject) -> GroupVersionKind {
    let typ = obj.types.as_ref();
    let api = typ.map(|t| t.api_version.as_str()).unwrap_or("v1");
    let (group, version) = api.split_once('/').unwrap_or(("", api));
    GroupVersionKind::gvk(group, version, kind(obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn object(value: serde_json::Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn selectors_preserve_expressions_and_reject_empty() {
        let obj = object(
            serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web"},"spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"tier","operator":"In","values":["frontend"]}]}}}),
        );
        let s = selector(&obj).unwrap();
        assert!(s.contains("app=web"));
        assert!(s.contains("tier in (frontend)"));
        let service = object(
            serde_json::json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"external"},"spec":{"selector":{}}}),
        );
        assert!(selector(&service).is_err());
    }
    #[test]
    fn ownership_uses_uid_not_reused_names() {
        let obj = object(
            serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web-1","ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"web","uid":"new"}]}}),
        );
        assert!(!owned_by(&obj, "old"));
        assert!(owned_by(&obj, "new"));
        assert!(!owned_by(&obj, ""));
    }
}
