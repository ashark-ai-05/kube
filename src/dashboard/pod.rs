//! Pod facts and resource budgets, kept independent from terminal presentation.
use super::metrics::{Usage, quantity};
use crate::ui::workspace::{Health, health};
use chrono::{DateTime, Utc};
use kube::{ResourceExt, api::DynamicObject};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub namespace: String,
    pub name: String,
    pub uid: Option<String>,
}
impl Identity {
    pub fn of(pod: &DynamicObject) -> Self {
        Self {
            namespace: pod.namespace().unwrap_or_default(),
            name: pod.name_any(),
            uid: pod.uid(),
        }
    }
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Budget {
    pub cpu_request: Option<f64>,
    pub cpu_limit: Option<f64>,
    pub memory_request: Option<f64>,
    pub memory_limit: Option<f64>,
}
impl Budget {
    pub fn of(pod: &DynamicObject) -> Self {
        let containers: Vec<_> = pod.data["spec"]["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(
                pod.data["spec"]["initContainers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["restartPolicy"] == "Always"),
            )
            .collect();
        let total = |kind: &str, resource: &str| {
            if let Some(value) = pod.data["spec"]["resources"][kind][resource].as_str() {
                return quantity(value).filter(|n| *n > 0.);
            }
            if containers.is_empty() {
                return None;
            }
            let sum = containers.iter().try_fold(0., |n, c| {
                Some(n + quantity(c["resources"][kind][resource].as_str()?)?)
            })?;
            (sum > 0. && sum.is_finite()).then_some(sum)
        };
        Self {
            cpu_request: total("requests", "cpu").map(|n| n * 1000.),
            cpu_limit: total("limits", "cpu").map(|n| n * 1000.),
            memory_request: total("requests", "memory"),
            memory_limit: total("limits", "memory"),
        }
    }
    pub fn memory_percent(self, usage: Option<Usage>) -> Option<f64> {
        Some(usage?.memory_bytes / self.memory_limit? * 100.)
    }
}
pub fn restarts(pod: &DynamicObject) -> u64 {
    statuses(pod)
        .filter_map(|c| c["restartCount"].as_u64())
        .fold(0, u64::saturating_add)
}
pub fn statuses(pod: &DynamicObject) -> impl Iterator<Item = &Value> {
    ["containerStatuses", "initContainerStatuses"]
        .into_iter()
        .flat_map(|key| pod.data["status"][key].as_array().into_iter().flatten())
}
pub fn ready(pod: &DynamicObject) -> String {
    let total = pod.data["spec"]["containers"]
        .as_array()
        .map_or(0, Vec::len)
        + pod.data["spec"]["initContainers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["restartPolicy"] == "Always")
            .count();
    let ready = pod.data["status"]["containerStatuses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["ready"] == true)
        .count()
        + pod.data["status"]["initContainerStatuses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| {
                c["ready"] == true
                    && pod.data["spec"]["initContainers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|spec| spec["name"] == c["name"] && spec["restartPolicy"] == "Always")
            })
            .count();
    format!("{ready}/{total}")
}
pub fn age_seconds(pod: &DynamicObject) -> Option<u64> {
    let created = pod.metadata.creation_timestamp.as_ref()?.0.as_second();
    Some((Utc::now().timestamp() - created).max(0) as u64)
}
pub fn duration(seconds: u64) -> String {
    match seconds {
        n if n < 60 => format!("{n}s"),
        n if n < 3600 => format!("{}m", n / 60),
        n if n < 86400 => format!("{}h", n / 3600),
        n => format!("{}d", n / 86400),
    }
}
pub fn since(timestamp: &str) -> Option<u64> {
    Some(
        (Utc::now() - timestamp.parse::<DateTime<Utc>>().ok()?)
            .num_seconds()
            .max(0) as u64,
    )
}
pub fn last_restart(pod: &DynamicObject) -> Option<u64> {
    statuses(pod)
        .filter(|c| c["restartCount"].as_u64().unwrap_or(0) > 0)
        .filter_map(|c| {
            c["lastState"]["terminated"]["finishedAt"]
                .as_str()
                .or_else(|| c["state"]["running"]["startedAt"].as_str())
                .and_then(since)
        })
        .min()
}
pub fn owner(pod: &DynamicObject) -> String {
    pod.metadata
        .owner_references
        .as_ref()
        .and_then(|refs| {
            refs.iter()
                .find(|o| o.controller == Some(true))
                .or_else(|| refs.first())
        })
        .map(|o| format!("{} / {}", o.kind, o.name))
        .unwrap_or_else(|| "Standalone pod".into())
}
pub fn termination(pod: &DynamicObject) -> String {
    statuses(pod)
        .filter_map(|c| {
            let ended = c.get("lastState")?.get("terminated")?;
            Some(format!(
                "{}: {} (exit {})",
                c["name"].as_str().unwrap_or("container"),
                ended["reason"].as_str().unwrap_or("Terminated"),
                ended["exitCode"]
            ))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    NotReady,
    Restarts,
    HighMemory,
}
impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::NotReady => "Not ready",
            Self::Restarts => "Restarts",
            Self::HighMemory => "Mem ≥80%",
        }
    }
    pub fn matches(self, pod: &DynamicObject, usage: Option<Usage>, budget: Budget) -> bool {
        match self {
            Self::All => true,
            Self::NotReady => matches!(health(pod), Health::Attention | Health::Unknown),
            Self::Restarts => restarts(pod) > 0,
            Self::HighMemory => budget.memory_percent(usage).is_some_and(|n| n >= 80.),
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "all" => Some(Self::All),
            "not-ready" => Some(Self::NotReady),
            "restarts" => Some(Self::Restarts),
            "memory" => Some(Self::HighMemory),
            _ => None,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::NotReady => "not-ready",
            Self::Restarts => "restarts",
            Self::HighMemory => "memory",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Name,
    Ready,
    Restarts,
    Age,
    Cpu,
    Memory,
}
impl Sort {
    pub const ALL: [Self; 6] = [
        Self::Name,
        Self::Ready,
        Self::Restarts,
        Self::Age,
        Self::Cpu,
        Self::Memory,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Ready => "Ready",
            Self::Restarts => "Restarts",
            Self::Age => "Age",
            Self::Cpu => "CPU",
            Self::Memory => "Memory",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.label().eq_ignore_ascii_case(value))
    }
}

/// Completed one-shot init containers do not hide the running application's state.
pub fn status(pod: &DynamicObject) -> String {
    if pod.metadata.deletion_timestamp.is_some() {
        return "Terminating".into();
    }
    for key in ["initContainerStatuses", "containerStatuses"] {
        for c in pod.data["status"][key].as_array().into_iter().flatten() {
            if let Some(reason) = c["state"]["waiting"]["reason"].as_str() {
                return reason.into();
            }
            if let Some(reason) = c["state"]["terminated"]["reason"].as_str()
                && (key == "containerStatuses"
                    || c["state"]["terminated"]["exitCode"]
                        .as_i64()
                        .is_some_and(|n| n != 0))
            {
                return reason.into();
            }
        }
    }
    pod.data["status"]["phase"]
        .as_str()
        .unwrap_or("Unknown")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_limits_are_unknown_and_restartable_sidecars_count() {
        let mut p:DynamicObject=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"test"},"spec":{"containers":[{"name":"app","resources":{"limits":{"memory":"100Mi","cpu":"500m"}}}],"initContainers":[{"name":"sidecar","restartPolicy":"Always","resources":{"limits":{"memory":"50Mi","cpu":"100m"}}},{"name":"init","resources":{"limits":{"memory":"1Gi"}}}]}})).unwrap();
        let budget = Budget::of(&p);
        assert_eq!(budget.memory_limit, Some(150. * 1048576.));
        assert_eq!(budget.cpu_limit, Some(600.));
        p.data["spec"]["containers"][0]["resources"]["limits"]["memory"] = Value::Null;
        assert!(Budget::of(&p).memory_limit.is_none());
        assert!(!Filter::HighMemory.matches(
            &p,
            Some(Usage {
                cpu_milli: 5.,
                memory_bytes: 1000.
            }),
            Budget::of(&p)
        ));
    }
    #[test]
    fn missing_metrics_are_never_high_usage() {
        let p:DynamicObject=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"test"},"spec":{"containers":[{"resources":{"limits":{"memory":"100Mi"}}}]}})).unwrap();
        assert!(!Filter::HighMemory.matches(&p, None, Budget::of(&p)));
        assert!(Filter::HighMemory.matches(
            &p,
            Some(Usage {
                cpu_milli: 0.,
                memory_bytes: 90. * 1048576.
            }),
            Budget::of(&p)
        ));
    }
}
