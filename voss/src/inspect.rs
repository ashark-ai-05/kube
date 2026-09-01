//! AI inspect: answers from fetched objects with citations.
//! Optional OPENAI_API_KEY — the app runs without it (deterministic retrieval).
use crate::citations::{extract_citations, Citation, FetchedObjects};

#[derive(Debug, Clone)]
pub struct InspectAnswer {
    pub text: String,
    pub citations: Vec<Citation>,
    pub used_openai: bool,
}

/// Deterministic retrieval + summary. Never invents cluster state.
pub fn summarize(objects: &FetchedObjects) -> InspectAnswer {
    let citations = extract_citations(objects);
    if objects.pod_name.is_none() && objects.events.is_empty() && objects.log_lines.is_empty() {
        return InspectAnswer {
            text: "No fetched objects to inspect. Connect a cluster and select a pod — Voss will not invent live state.".into(),
            citations,
            used_openai: false,
        };
    }

    let mut lines = Vec::new();
    if let Some(pod) = &objects.pod_name {
        lines.push(format!(
            "Pod {} in ns {} is phase {} (ready {}), node {}.",
            pod,
            objects.namespace,
            objects.pod_phase.as_deref().unwrap_or("Unknown"),
            objects.pod_ready.as_deref().unwrap_or("?"),
            objects.node.as_deref().unwrap_or("(unscheduled)")
        ));
    }
    if !objects.containers.is_empty() {
        lines.push(format!("Containers: {}.", objects.containers.join(", ")));
    }
    if !objects.conditions.is_empty() {
        lines.push(format!("Conditions: {}.", objects.conditions.join("; ")));
    }
    for ev in &objects.events {
        lines.push(format!(
            "Event {}: {} — {}",
            ev.reason, ev.type_, ev.message
        ));
    }
    if !objects.log_lines.is_empty() {
        let tail = objects
            .log_lines
            .iter()
            .rev()
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        lines.push(format!("Recent log tail: {}", tail.join(" | ")));
    }
    lines.push("Citations:".into());
    for c in &citations {
        lines.push(format!("  - {}", c.display()));
    }

    InspectAnswer {
        text: lines.join("\n"),
        citations,
        used_openai: false,
    }
}

pub fn openai_api_key_present() -> bool {
    std::env::var("OPENAI_API_KEY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// If `OPENAI_API_KEY` is set, optionally rewrite the deterministic summary.
/// Failures fall back to the local summary so the app never depends on the API.
pub async fn inspect_with_optional_openai(objects: &FetchedObjects) -> InspectAnswer {
    let local = summarize(objects);
    if !openai_api_key_present() {
        return local;
    }
    match call_openai(&local.text).await {
        Ok(text) => InspectAnswer {
            text: format!("{text}\n\nCitations:\n{}", citation_block(&local.citations)),
            citations: local.citations,
            used_openai: true,
        },
        Err(_) => local,
    }
}

fn citation_block(cites: &[Citation]) -> String {
    cites
        .iter()
        .map(|c| format!("  - {}", c.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn call_openai(prompt: &str) -> Result<String, anyhow::Error> {
    let key = std::env::var("OPENAI_API_KEY")?;
    let client = reqwest::Client::builder().use_rustls_tls().build()?;
    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [
            {"role": "system", "content": "Summarize only the provided Kubernetes objects. Cite ns/pod/event/log locators already in the user message. Do not invent cluster state."},
            {"role": "user", "content": prompt}
        ]
    });
    let resp = client
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(key)
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("openai http {}", resp.status());
    }
    let v: serde_json::Value = resp.json().await?;
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing content"))?;
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::citations::FetchedEvent;

    #[test]
    fn summary_cites_fixture_objects() {
        let raw = include_str!("../tests/fixtures/inspect_objects.json");
        let objects: FetchedObjects = serde_json::from_str(raw).unwrap();
        let ans = summarize(&objects);
        assert!(!ans.used_openai);
        assert!(ans.text.contains("api-7f9"));
        assert!(ans.text.contains("FailedScheduling"));
        assert!(ans.text.contains("ns/payments/pod/api-7f9"));
        assert!(ans.citations.iter().any(|c| c.kind == "event"));
        assert!(ans.citations.iter().any(|c| c.kind == "log"));
    }

    #[test]
    fn empty_objects_do_not_invent_state() {
        let ans = summarize(&FetchedObjects::default());
        assert!(ans.text.contains("will not invent"));
        assert!(ans.citations.is_empty());
    }

    #[test]
    fn openai_absent_by_default_in_tests() {
        // Tests must not require a key.
        let _ = openai_api_key_present();
        let ans = summarize(&FetchedObjects {
            namespace: "default".into(),
            pod_name: Some("web".into()),
            pod_phase: Some("Running".into()),
            events: vec![FetchedEvent {
                name: "web.1".into(),
                reason: "Started".into(),
                message: "Started container".into(),
                type_: "Normal".into(),
            }],
            ..Default::default()
        });
        assert!(!ans.used_openai);
    }
}
