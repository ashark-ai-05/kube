//! Live cluster reads. Empty UI unless these succeed — never invent state.
use crate::citations::{FetchedEvent, FetchedObjects};
use crate::kubeconfig::{require_kubeconfig, resolve_kubeconfig_path};
use crate::rbac::map_error_text;
use crate::redact::{redact_secret_data, REDACTED};
use chrono::{DateTime, Utc};
use k8s_openapi::api::core::v1::{Event, Namespace, Pod, Secret};
use kube::api::{ListParams, LogParams};
use kube::{Api, Client, ResourceExt};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Connection {
    pub context: String,
    pub kubeconfig_path: String,
}

#[derive(Debug, Clone)]
pub struct PodRow {
    pub name: String,
    pub namespace: String,
    pub ready: String,
    pub phase: String,
    pub restarts: i32,
    pub age: String,
    pub node: String,
}

#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub name: String,
    pub image: String,
    pub ready: bool,
    pub restarts: i32,
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct PodDetail {
    pub row: PodRow,
    pub containers: Vec<ContainerInfo>,
    pub conditions: Vec<String>,
    pub events: Vec<FetchedEvent>,
    pub log_container: Option<String>,
    pub log_lines: Vec<String>,
}

pub fn kube_error_message(err: &kube::Error) -> String {
    match err {
        kube::Error::Api(status) => map_error_text(
            Some(status.code),
            &status.reason,
            &status.message,
        ),
        other => other.to_string(),
    }
}

pub async fn connect() -> Result<(Client, Connection), String> {
    let path = resolve_kubeconfig_path().map_err(|e| e.to_string())?;
    require_kubeconfig(&path).map_err(|e| e.to_string())?;
    let cfg = kube::Config::infer()
        .await
        .map_err(|e| format!("kubeconfig/auth: {e}"))?;
    let context_name = parse_current_context(
        &std::fs::read_to_string(&path).unwrap_or_default(),
    )
    .unwrap_or_else(|| "current-context".to_string());
    let client = Client::try_from(cfg).map_err(|e| format!("auth/client: {e}"))?;
    Ok((
        client,
        Connection {
            context: context_name,
            kubeconfig_path: path.display().to_string(),
        },
    ))
}

pub async fn list_namespaces(client: Client) -> Result<Vec<String>, String> {
    let api: Api<Namespace> = Api::all(client);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| kube_error_message(&e))?;
    let mut names: Vec<_> = list.iter().map(|n| n.name_any()).collect();
    names.sort();
    Ok(names)
}

pub async fn list_pods(client: Client, namespace: String) -> Result<Vec<PodRow>, String> {
    let api: Api<Pod> = Api::namespaced(client, &namespace);
    let list = api
        .list(&ListParams::default())
        .await
        .map_err(|e| kube_error_message(&e))?;
    Ok(list.iter().map(|p| pod_row(p, &namespace)).collect())
}

fn pod_row(p: &Pod, namespace: &str) -> PodRow {
    let spec = p.spec.as_ref();
    let status = p.status.as_ref();
    let total = spec.map(|s| s.containers.len()).unwrap_or(0);
    let ready_n = status
        .and_then(|s| s.container_statuses.as_ref())
        .map(|cs| cs.iter().filter(|c| c.ready).count())
        .unwrap_or(0);
    let restarts = status
        .and_then(|s| s.container_statuses.as_ref())
        .map(|cs| cs.iter().map(|c| c.restart_count).sum::<i32>())
        .unwrap_or(0);
    let phase = status
        .and_then(|s| s.phase.clone())
        .unwrap_or_else(|| "Unknown".into());
    let node = spec
        .and_then(|s| s.node_name.clone())
        .unwrap_or_default();
    let age = p
        .creation_timestamp()
        .map(|t| age_from(t.0))
        .unwrap_or_else(|| "-".into());
    PodRow {
        name: p.name_any(),
        namespace: namespace.to_string(),
        ready: format!("{ready_n}/{total}"),
        phase,
        restarts,
        age,
        node,
    }
}

fn age_from(ts: DateTime<Utc>) -> String {
    let secs = (Utc::now() - ts).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

pub async fn load_pod_detail(
    client: Client,
    namespace: String,
    name: String,
    log_tail: i64,
) -> Result<PodDetail, String> {
    let api: Api<Pod> = Api::namespaced(client.clone(), &namespace);
    let pod = api.get(&name).await.map_err(|e| kube_error_message(&e))?;
    let row = pod_row(&pod, &namespace);
    let mut containers = Vec::new();
    if let Some(status) = &pod.status {
        if let Some(cs) = &status.container_statuses {
            for c in cs {
                let state = match &c.state {
                    Some(k8s_openapi::api::core::v1::ContainerState {
                        running: Some(_),
                        ..
                    }) => "Running".into(),
                    Some(k8s_openapi::api::core::v1::ContainerState {
                        waiting: Some(w),
                        ..
                    }) => w.reason.clone().unwrap_or_else(|| "Waiting".into()),
                    Some(k8s_openapi::api::core::v1::ContainerState {
                        terminated: Some(t),
                        ..
                    }) => t.reason.clone().unwrap_or_else(|| "Terminated".into()),
                    _ => "Unknown".into(),
                };
                containers.push(ContainerInfo {
                    name: c.name.clone(),
                    image: c.image.clone(),
                    ready: c.ready,
                    restarts: c.restart_count,
                    state,
                });
            }
        }
    }
    if containers.is_empty() {
        if let Some(spec) = &pod.spec {
            for c in &spec.containers {
                containers.push(ContainerInfo {
                    name: c.name.clone(),
                    image: c.image.clone().unwrap_or_default(),
                    ready: false,
                    restarts: 0,
                    state: "Unknown".into(),
                });
            }
        }
    }
    let conditions = pod
        .status
        .as_ref()
        .and_then(|s| s.conditions.as_ref())
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    format!(
                        "{}={} {}",
                        c.type_,
                        c.status,
                        c.reason.clone().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let events = load_events(client.clone(), &namespace, &name).await?;
    let log_container = containers.first().map(|c| c.name.clone());
    let log_lines = if let Some(c) = &log_container {
        load_logs(client, &namespace, &name, c, log_tail).await.unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(PodDetail {
        row,
        containers,
        conditions,
        events,
        log_container,
        log_lines,
    })
}

async fn load_events(
    client: Client,
    namespace: &str,
    pod: &str,
) -> Result<Vec<FetchedEvent>, String> {
    let api: Api<Event> = Api::namespaced(client, namespace);
    let lp = ListParams::default().fields(&format!(
        "involvedObject.name={pod},involvedObject.kind=Pod"
    ));
    let list = api.list(&lp).await.map_err(|e| kube_error_message(&e))?;
    Ok(list
        .iter()
        .map(|e| FetchedEvent {
            name: e.name_any(),
            reason: e.reason.clone().unwrap_or_default(),
            message: e.message.clone().unwrap_or_default(),
            type_: e.type_.clone().unwrap_or_default(),
        })
        .collect())
}

async fn load_logs(
    client: Client,
    namespace: &str,
    pod: &str,
    container: &str,
    tail: i64,
) -> Result<Vec<String>, String> {
    let api: Api<Pod> = Api::namespaced(client, namespace);
    let params = LogParams {
        container: Some(container.to_string()),
        tail_lines: Some(tail),
        ..Default::default()
    };
    let text = api.logs(pod, &params).await.map_err(|e| kube_error_message(&e))?;
    Ok(text.lines().map(|s| s.to_string()).collect())
}

pub async fn peek_secret_keys(
    client: Client,
    namespace: String,
    name: String,
) -> Result<BTreeMap<String, String>, String> {
    let api: Api<Secret> = Api::namespaced(client, &namespace);
    let secret = api.get(&name).await.map_err(|e| kube_error_message(&e))?;
    let mut data = BTreeMap::new();
    if let Some(d) = secret.data {
        for (k, _) in d {
            data.insert(k, REDACTED.to_string());
        }
    }
    if let Some(d) = secret.string_data {
        for (k, v) in d {
            data.insert(k, v);
        }
    }
    Ok(redact_secret_data(&data))
}

pub fn detail_to_objects(namespace: &str, detail: &PodDetail) -> FetchedObjects {
    FetchedObjects {
        namespace: namespace.to_string(),
        pod_name: Some(detail.row.name.clone()),
        pod_phase: Some(detail.row.phase.clone()),
        pod_ready: Some(detail.row.ready.clone()),
        node: if detail.row.node.is_empty() {
            None
        } else {
            Some(detail.row.node.clone())
        },
        containers: detail.containers.iter().map(|c| c.name.clone()).collect(),
        conditions: detail.conditions.clone(),
        events: detail.events.clone(),
        log_lines: detail.log_lines.clone(),
        log_container: detail.log_container.clone(),
    }
}

/// Current kubeconfig context name from the kubeconfig file (no cluster call).
pub fn current_context_name() -> Result<String, String> {
    let path = resolve_kubeconfig_path().map_err(|e| e.to_string())?;
    require_kubeconfig(&path).map_err(|e| e.to_string())?;
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    parse_current_context(&raw).ok_or_else(|| "kubeconfig has no current-context".into())
}

pub fn parse_current_context(yaml: &str) -> Option<String> {
    for line in yaml.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("current-context:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_current_context_from_yaml() {
        let y = "apiVersion: v1\nkind: Config\ncurrent-context: prod-eu\n";
        assert_eq!(parse_current_context(y).as_deref(), Some("prod-eu"));
    }
}
