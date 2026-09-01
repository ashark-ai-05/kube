//! Kubernetes Secret values are redacted by default. Keys remain visible.
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const REDACTED: &str = "[redacted]";

/// Replace every secret data value with `[redacted]`, preserving keys.
pub fn redact_secret_data(data: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    data.keys()
        .map(|k| (k.clone(), REDACTED.to_string()))
        .collect()
}

/// Redact `data` and `binaryData` maps on a Secret-shaped JSON object.
/// Non-secret objects are returned unchanged. Never returns live values.
pub fn redact_secret_json(v: &Value) -> Value {
    let mut out = v.clone();
    let is_secret = out
        .get("kind")
        .and_then(|k| k.as_str())
        .is_some_and(|k| k == "Secret");
    if !is_secret {
        return out;
    }
    if let Some(obj) = out.as_object_mut() {
        redact_map_field(obj, "data");
        redact_map_field(obj, "binaryData");
        redact_map_field(obj, "stringData");
    }
    out
}

fn redact_map_field(obj: &mut Map<String, Value>, field: &str) {
    if let Some(Value::Object(map)) = obj.get_mut(field) {
        for (_k, v) in map.iter_mut() {
            *v = Value::String(REDACTED.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_redacted_keys_kept() {
        let mut data = BTreeMap::new();
        data.insert("password".into(), "hunter2".into());
        data.insert("token".into(), "live-token".into());
        let out = redact_secret_data(&data);
        assert_eq!(out.get("password").unwrap(), REDACTED);
        assert_eq!(out.get("token").unwrap(), REDACTED);
        assert!(!out.values().any(|v| v.contains("hunter") || v.contains("live")));
    }

    #[test]
    fn secret_json_redacts_all_payload_maps() {
        let secret = json!({
            "kind": "Secret",
            "metadata": { "name": "db", "namespace": "prod" },
            "data": { "password": "c2VjcmV0" },
            "binaryData": { "cert": "Y2VydA==" },
            "stringData": { "plain": "visible-secret" }
        });
        let out = redact_secret_json(&secret);
        assert_eq!(out["data"]["password"], REDACTED);
        assert_eq!(out["binaryData"]["cert"], REDACTED);
        assert_eq!(out["stringData"]["plain"], REDACTED);
        assert_eq!(out["metadata"]["name"], "db");
        let s = out.to_string();
        assert!(!s.contains("c2VjcmV0"));
        assert!(!s.contains("visible-secret"));
    }

    #[test]
    fn non_secret_json_is_untouched() {
        let cfg = json!({
            "kind": "ConfigMap",
            "data": { "url": "https://example" }
        });
        let out = redact_secret_json(&cfg);
        assert_eq!(out["data"]["url"], "https://example");
    }
}
