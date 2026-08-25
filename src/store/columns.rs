use chrono::{DateTime, Utc};
use kube::api::{DynamicObject, GroupVersionKind, ResourceExt};
use ratatui::layout::Constraint;

/// One table column: a header plus a pure extraction from an object.
pub struct Column {
    pub header: &'static str,
    pub width: Constraint,
    pub extract: fn(&DynamicObject) -> String,
}

fn container_statuses(obj: &DynamicObject) -> &[serde_json::Value] {
    obj.data
        .get("status")
        .and_then(|s| s.get("containerStatuses"))
        .and_then(|c| c.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// "ready/total" across containers. A pod that has been scheduled but whose
/// containers have not started reports no statuses at all, so this must not
/// assume the array exists.
pub fn pod_ready(obj: &DynamicObject) -> String {
    let statuses = container_statuses(obj);
    let ready = statuses
        .iter()
        .filter(|c| c.get("ready").and_then(|r| r.as_bool()).unwrap_or(false))
        .count();
    format!("{}/{}", ready, statuses.len())
}

pub fn pod_restarts(obj: &DynamicObject) -> String {
    let total: i64 = container_statuses(obj)
        .iter()
        .filter_map(|c| c.get("restartCount").and_then(|r| r.as_i64()))
        .sum();
    total.to_string()
}

pub fn pod_phase(obj: &DynamicObject) -> String {
    obj.data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .unwrap_or("Unknown")
        .to_string()
}

/// Compact age, matching kubectl's convention of one significant unit.
pub fn format_age(created: &str, now: DateTime<Utc>) -> String {
    let Ok(ts) = created.parse::<DateTime<Utc>>() else {
        return "?".to_string();
    };
    let secs = (now - ts).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

fn extract_name(obj: &DynamicObject) -> String {
    obj.name_any()
}

fn extract_age(obj: &DynamicObject) -> String {
    match obj.metadata.creation_timestamp.as_ref() {
        Some(t) => {
            // Convert jiff::Timestamp to RFC3339 string
            let ts_str = format!("{}", t.0);
            format_age(&ts_str, Utc::now())
        }
        None => "?".to_string(),
    }
}

/// Columns for a kind. Kinds without an entry fall back to name and age, which
/// is always available via metadata — so CRDs render usefully with no per-kind code.
pub fn columns_for(gvk: &GroupVersionKind) -> Vec<Column> {
    if gvk.group.is_empty() && gvk.kind == "Pod" {
        return vec![
            Column {
                header: "NAME",
                width: Constraint::Fill(2),
                extract: extract_name,
            },
            Column {
                header: "READY",
                width: Constraint::Length(7),
                extract: pod_ready,
            },
            Column {
                header: "STATUS",
                width: Constraint::Length(14),
                extract: pod_phase,
            },
            Column {
                header: "RESTARTS",
                width: Constraint::Length(9),
                extract: pod_restarts,
            },
            Column {
                header: "AGE",
                width: Constraint::Length(6),
                extract: extract_age,
            },
        ];
    }
    vec![
        Column {
            header: "NAME",
            width: Constraint::Fill(1),
            extract: extract_name,
        },
        Column {
            header: "AGE",
            width: Constraint::Length(6),
            extract: extract_age,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;

    fn pod_with(status: serde_json::Value) -> DynamicObject {
        let mut o = DynamicObject::new("p", &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({ "status": status });
        o
    }

    #[test]
    fn ready_counts_ready_containers() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [
                {"ready": true, "restartCount": 0},
                {"ready": false, "restartCount": 0}
            ]
        }));
        assert_eq!(pod_ready(&o), "1/2");
    }

    #[test]
    fn ready_handles_all_ready() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [{"ready": true, "restartCount": 0}]
        }));
        assert_eq!(pod_ready(&o), "1/1");
    }

    #[test]
    fn ready_on_a_pending_pod_with_no_statuses_is_zero_of_zero() {
        let o = pod_with(serde_json::json!({"phase": "Pending"}));
        assert_eq!(
            pod_ready(&o),
            "0/0",
            "a scheduled-but-not-started pod must not panic"
        );
    }

    #[test]
    fn restarts_sums_across_containers() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [
                {"ready": true, "restartCount": 3},
                {"ready": true, "restartCount": 4}
            ]
        }));
        assert_eq!(pod_restarts(&o), "7");
    }

    #[test]
    fn phase_falls_back_to_unknown_when_absent() {
        let o = pod_with(serde_json::json!({}));
        assert_eq!(pod_phase(&o), "Unknown");
    }

    #[test]
    fn phase_reads_the_status_phase() {
        let o = pod_with(serde_json::json!({"phase": "Running"}));
        assert_eq!(pod_phase(&o), "Running");
    }

    #[test]
    fn age_formats_compactly_by_magnitude() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap();
        assert_eq!(format_age("2026-08-25T11:59:30Z", now), "30s");
        assert_eq!(format_age("2026-08-25T11:45:00Z", now), "15m");
        assert_eq!(format_age("2026-08-25T08:00:00Z", now), "4h");
        assert_eq!(format_age("2026-08-21T12:00:00Z", now), "4d");
    }

    #[test]
    fn age_of_an_unparseable_timestamp_is_unknown() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap();
        assert_eq!(format_age("not-a-timestamp", now), "?");
    }

    #[test]
    fn pod_columns_are_the_expected_five() {
        let cols = columns_for(&GroupVersionKind::gvk("", "v1", "Pod"));
        let headers: Vec<&str> = cols.iter().map(|c| c.header).collect();
        assert_eq!(headers, vec!["NAME", "READY", "STATUS", "RESTARTS", "AGE"]);
    }

    #[test]
    fn unknown_kind_falls_back_to_name_and_age() {
        let cols = columns_for(&GroupVersionKind::gvk("example.com", "v1", "Widget"));
        let headers: Vec<&str> = cols.iter().map(|c| c.header).collect();
        assert_eq!(
            headers,
            vec!["NAME", "AGE"],
            "unknown kinds still render something useful"
        );
    }
}
