//! Local inspect answers from already-fetched objects, with citations.
//!
//! The app runs without OPENAI_API_KEY. If that env var is set, `maybe_llm`
//! can add a short model note; it never invents cluster state.

use crate::store::events::EventRow;
use kube::api::{DynamicObject, ResourceExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub namespace: Option<String>,
    pub name: String,
    pub kind: String,
    pub note: String,
}

/// Redact Secret `.data` / `.stringData` values so they never reach the UI.
pub fn redact_object(mut obj: DynamicObject) -> DynamicObject {
    let is_secret = obj
        .types
        .as_ref()
        .map(|t| t.kind == "Secret")
        .unwrap_or(false)
        || obj.types.is_none() && obj.data.get("data").is_some() && obj.data.get("kind").is_none();
    if is_secret || object_kind(&obj) == "Secret" {
        for key in ["data", "stringData"] {
            if let Some(map) = obj.data.get_mut(key).and_then(|v| v.as_object_mut()) {
                for (_k, v) in map.iter_mut() {
                    *v = serde_json::Value::String("***".into());
                }
            }
        }
    }
    obj
}

fn object_kind(obj: &DynamicObject) -> &str {
    obj.types
        .as_ref()
        .map(|t| t.kind.as_str())
        .unwrap_or("Object")
}

/// Answer from the selected object plus its events. No network.
pub fn inspect_local(obj: &DynamicObject, events: &[EventRow]) -> (String, Vec<Citation>) {
    let obj = redact_object(obj.clone());
    let ns = obj.namespace();
    let name = obj.name_any();
    let kind = object_kind(&obj).to_string();
    let mut citations = vec![Citation {
        namespace: ns.clone(),
        name: name.clone(),
        kind: kind.clone(),
        note: "selected object".into(),
    }];

    let phase = obj
        .data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .unwrap_or("unknown");
    let mut lines = vec![format!(
        "{kind} {ns}/{name} — status {phase}.",
        ns = ns.as_deref().unwrap_or("-")
    )];

    if events.is_empty() {
        lines.push("No events were fetched for this object.".into());
    } else {
        lines.push(format!("{} event(s):", events.len()));
        for e in events.iter().take(12) {
            lines.push(format!(
                "- [{}/{}/{}/{}] {} (×{}, {})",
                ns.as_deref().unwrap_or("-"),
                name,
                e.reason,
                e.kind,
                e.message,
                e.count,
                e.age
            ));
            citations.push(Citation {
                namespace: ns.clone(),
                name: name.clone(),
                kind: "Event".into(),
                note: e.reason.clone(),
            });
        }
    }

    if std::env::var_os("OPENAI_API_KEY").is_none() {
        lines.push(
            "Inspect is local (no OPENAI_API_KEY). Citations are ns/name/reason from fetched objects."
                .into(),
        );
    }

    (lines.join("\n"), citations)
}

/// Optional extra sentence from OpenAI. Returns None if no key or the call fails.
pub async fn maybe_llm(prompt: &str) -> Option<String> {
    let key = std::env::var("OPENAI_API_KEY").ok()?;
    if key.is_empty() {
        return None;
    }
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .ok()?;
    let body = serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [
            {"role": "system", "content": "You explain Kubernetes objects. Use only facts in the user message. Cite ns/name/reason. Do not invent cluster state."},
            {"role": "user", "content": prompt}
        ],
        "max_tokens": 400
    });
    let res = client
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let v: serde_json::Value = res.json().await.ok()?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;

    fn pod(name: &str) -> DynamicObject {
        let mut o = DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("demo");
        o.types = Some(kube::api::TypeMeta {
            api_version: "v1".into(),
            kind: "Pod".into(),
        });
        o.data = serde_json::json!({"status": {"phase": "Running"}});
        o
    }

    fn secret() -> DynamicObject {
        let mut o = DynamicObject::new("creds", &ApiResource::erase::<Pod>(&())).within("demo");
        o.types = Some(kube::api::TypeMeta {
            api_version: "v1".into(),
            kind: "Secret".into(),
        });
        o.data = serde_json::json!({
            "data": {"token": "c2VjcmV0"},
            "stringData": {"password": "hunter2"}
        });
        o
    }

    #[test]
    fn inspect_cites_namespace_name_and_event_reason() {
        let events = vec![EventRow {
            kind: "Warning".into(),
            reason: "BackOff".into(),
            message: "Back-off restarting failed container".into(),
            age: "2m".into(),
            count: 12,
        }];
        let (text, cites) = inspect_local(&pod("web"), &events);
        assert!(text.contains("Pod demo/web"));
        assert!(text.contains("[demo/web/BackOff/Warning]"));
        assert!(cites.iter().any(|c| c.note == "BackOff" && c.kind == "Event"));
        assert!(cites.iter().any(|c| c.name == "web" && c.namespace.as_deref() == Some("demo")));
    }

    #[test]
    fn secret_data_is_redacted() {
        let red = redact_object(secret());
        assert_eq!(red.data["data"]["token"], "***");
        assert_eq!(red.data["stringData"]["password"], "***");
    }

    #[test]
    fn inspect_works_without_openai_key() {
        // SAFETY: test process; we only unset for this assertion.
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        let (text, _) = inspect_local(&pod("web"), &[]);
        assert!(text.contains("no OPENAI_API_KEY"));
    }
}
