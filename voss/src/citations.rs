//! Citations pointing at fetched Kubernetes objects (never invented state).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// One of: `ns`, `pod`, `event`, `log`.
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub extra: Option<String>,
}

impl Citation {
    pub fn ns(namespace: impl Into<String>) -> Self {
        let namespace = namespace.into();
        Self {
            kind: "ns".into(),
            name: namespace.clone(),
            namespace,
            extra: None,
        }
    }

    pub fn pod(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind: "pod".into(),
            namespace: namespace.into(),
            name: name.into(),
            extra: None,
        }
    }

    pub fn event(
        namespace: impl Into<String>,
        name: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            kind: "event".into(),
            namespace: namespace.into(),
            name: name.into(),
            extra: Some(reason.into()),
        }
    }

    pub fn log(
        namespace: impl Into<String>,
        pod: impl Into<String>,
        container: impl Into<String>,
    ) -> Self {
        Self {
            kind: "log".into(),
            namespace: namespace.into(),
            name: pod.into(),
            extra: Some(container.into()),
        }
    }

    pub fn display(&self) -> String {
        match self.kind.as_str() {
            "ns" => format!("ns/{}", self.namespace),
            "pod" => format!("ns/{}/pod/{}", self.namespace, self.name),
            "event" => format!(
                "ns/{}/event/{}{}",
                self.namespace,
                self.name,
                self.extra
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ),
            "log" => format!(
                "ns/{}/pod/{}/log/{}",
                self.namespace,
                self.name,
                self.extra.as_deref().unwrap_or("-")
            ),
            other => format!("{other}/{}", self.name),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchedObjects {
    pub namespace: String,
    pub pod_name: Option<String>,
    pub pod_phase: Option<String>,
    pub pod_ready: Option<String>,
    pub node: Option<String>,
    pub containers: Vec<String>,
    pub conditions: Vec<String>,
    pub events: Vec<FetchedEvent>,
    pub log_lines: Vec<String>,
    pub log_container: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedEvent {
    pub name: String,
    pub reason: String,
    pub message: String,
    pub type_: String,
}

/// Pull citations only from objects that were actually fetched.
pub fn extract_citations(objects: &FetchedObjects) -> Vec<Citation> {
    let mut out = Vec::new();
    if !objects.namespace.is_empty() {
        out.push(Citation::ns(&objects.namespace));
    }
    if let Some(pod) = &objects.pod_name {
        out.push(Citation::pod(&objects.namespace, pod));
    }
    for ev in &objects.events {
        out.push(Citation::event(&objects.namespace, &ev.name, &ev.reason));
    }
    if let (Some(pod), Some(c)) = (&objects.pod_name, &objects.log_container) {
        if !objects.log_lines.is_empty() {
            out.push(Citation::log(&objects.namespace, pod, c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> FetchedObjects {
        let raw = include_str!("../tests/fixtures/inspect_objects.json");
        serde_json::from_str(raw).expect("fixture json")
    }

    #[test]
    fn citations_from_fixture_include_ns_pod_event_log() {
        let cites = extract_citations(&fixture());
        let kinds: Vec<_> = cites.iter().map(|c| c.kind.as_str()).collect();
        assert!(kinds.contains(&"ns"));
        assert!(kinds.contains(&"pod"));
        assert!(kinds.contains(&"event"));
        assert!(kinds.contains(&"log"));
        let displays: Vec<_> = cites.iter().map(|c| c.display()).collect();
        assert!(displays.iter().any(|d| d == "ns/payments"));
        assert!(displays.iter().any(|d| d == "ns/payments/pod/api-7f9"));
        assert!(displays.iter().any(|d| d.contains("event/api-7f9.1")));
        assert!(displays.iter().any(|d| d.contains("FailedScheduling") || d.contains("BackOff")));
        assert!(displays.iter().any(|d| d == "ns/payments/pod/api-7f9/log/app"));
    }

    #[test]
    fn empty_fetch_yields_no_invented_citations() {
        let cites = extract_citations(&FetchedObjects::default());
        assert!(cites.is_empty());
    }

    #[test]
    fn logs_without_lines_are_not_cited() {
        let mut o = FetchedObjects {
            namespace: "ns".into(),
            pod_name: Some("p".into()),
            log_container: Some("c".into()),
            ..Default::default()
        };
        let cites = extract_citations(&o);
        assert!(cites.iter().all(|c| c.kind != "log"));
        o.log_lines.push("hello".into());
        let cites = extract_citations(&o);
        assert!(cites.iter().any(|c| c.kind == "log"));
    }
}
