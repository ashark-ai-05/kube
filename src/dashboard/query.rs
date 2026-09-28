//! Compiled, AND-combined pod predicates. Missing numeric evidence never satisfies a comparison.
use super::{
    metrics::{Usage, quantity},
    pod::{self, Budget},
};
use kube::api::DynamicObject;

#[derive(Clone, Copy, Debug)]
enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}
impl Op {
    fn matches(self, a: f64, b: f64) -> bool {
        match self {
            Self::Eq => a == b,
            Self::Ne => a != b,
            Self::Gt => a > b,
            Self::Ge => a >= b,
            Self::Lt => a < b,
            Self::Le => a <= b,
        }
    }
}
#[derive(Clone, Copy, Debug)]
enum Metric {
    Restarts,
    Age,
    Cpu,
    Memory,
    MemoryPercent,
}
#[derive(Clone, Debug)]
enum Clause {
    Text(String),
    Label(String, Option<(bool, String)>),
    Ready(Option<bool>),
    Field(String, bool, String),
    Numeric(Metric, Op, f64),
    Unhealthy,
}
#[derive(Clone, Debug, Default)]
pub struct Query {
    clauses: Vec<Clause>,
    pub needs_metrics: bool,
}
impl Query {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.chars().any(char::is_control) {
            return Err("Filter text cannot contain control characters".into());
        }
        if text.len() > 2048 {
            return Err("Query is too long (maximum 2048 bytes)".into());
        }
        let mut query = Self::default();
        for word in text.split_whitespace() {
            if query.clauses.len() == 32 {
                return Err("Use at most 32 filter conditions".into());
            }
            let clause = if word == "health:unhealthy" {
                Clause::Unhealthy
            } else if let Some(label) = word.strip_prefix("label:") {
                let (key, value) = if let Some((key, value)) = label.split_once("!=") {
                    (key, Some((false, value.to_string())))
                } else if let Some((key, value)) = label.split_once('=') {
                    (key, Some((true, value.to_string())))
                } else {
                    (label, None)
                };
                if key.is_empty() || key.contains(['<', '>', '!', '=']) {
                    return Err(format!("Invalid label condition: {word}"));
                }
                Clause::Label(key.into(), value)
            } else if let Some(pos) = word.find(['=', '!', '<', '>']) {
                let (field, tail) = word.split_at(pos);
                let (op, value) = [
                    (">=", Op::Ge),
                    ("<=", Op::Le),
                    ("!=", Op::Ne),
                    ("=", Op::Eq),
                    (">", Op::Gt),
                    ("<", Op::Lt),
                ]
                .into_iter()
                .find_map(|(token, op)| tail.strip_prefix(token).map(|v| (op, v)))
                .ok_or_else(|| format!("Invalid operator: {word}"))?;
                if value.is_empty() || value.contains(['=', '!', '<', '>']) {
                    return Err(format!("Complete the condition: {word}"));
                }
                match field {
                    "ready" => {
                        if !matches!(op, Op::Eq) {
                            return Err(
                                "Use ready=true, ready=false or ready=unknown explicitly".into()
                            );
                        }
                        let ready = match value {
                            "true" => Some(true),
                            "false" => Some(false),
                            "unknown" => None,
                            _ => return Err("Ready must be true, false or unknown".into()),
                        };
                        Clause::Ready(ready)
                    }
                    "name" | "namespace" | "node" | "status" => {
                        if !matches!(op, Op::Eq | Op::Ne) {
                            return Err(format!("{field} uses = or !="));
                        }
                        Clause::Field(field.into(), matches!(op, Op::Eq), value.into())
                    }
                    "restarts" | "age" | "cpu" | "memory" | "mem" | "memory/limit"
                    | "mem/limit" => {
                        let (metric, number) =
                            match field {
                                "restarts" => (
                                    Metric::Restarts,
                                    value.parse::<u64>().ok().map(|n| n as f64),
                                ),
                                "age" => (Metric::Age, crate::store::table::age_seconds(value)),
                                "cpu" => (Metric::Cpu, quantity(value).map(|n| n * 1000.)),
                                _ if value.ends_with('%') => (
                                    Metric::MemoryPercent,
                                    value.strip_suffix('%').and_then(|v| v.parse().ok()),
                                ),
                                "memory/limit" | "mem/limit" => return Err(
                                    "Memory / limit requires a percentage, e.g. memory/limit>80%"
                                        .into(),
                                ),
                                _ => (Metric::Memory, quantity(value)),
                            };
                        let number = number.filter(|n| n.is_finite() && *n >= 0.)
                            .ok_or_else(|| format!("Invalid value: {word}. Use restarts>3, age>1h, cpu>500m or memory>128Mi"))?;
                        query.needs_metrics |=
                            matches!(metric, Metric::Cpu | Metric::Memory | Metric::MemoryPercent);
                        Clause::Numeric(metric, op, number)
                    }
                    _ => {
                        return Err(format!(
                            "Unknown field '{field}' · use name, namespace, node, status, ready, restarts, age, cpu, memory or label:key=value"
                        ));
                    }
                }
            } else {
                Clause::Text(word.to_lowercase())
            };
            query.clauses.push(clause);
        }
        Ok(query)
    }
    pub fn matches(
        &self,
        object: &DynamicObject,
        usage: Option<Usage>,
        budget: Budget,
        restarts: u64,
        age: Option<u64>,
    ) -> bool {
        self.clauses.iter().all(|clause| match clause {
            Clause::Text(text) => crate::ui::command::fuzzy_match(
                text,
                &format!(
                    "{}/{}",
                    object.metadata.namespace.as_deref().unwrap_or(""),
                    object.metadata.name.as_deref().unwrap_or("")
                ),
            ),
            Clause::Label(key, expected) => {
                let actual = object
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get(key));
                match expected {
                    None => actual.is_some(),
                    Some((true, value)) => actual == Some(value),
                    Some((false, value)) => actual != Some(value),
                }
            }
            Clause::Unhealthy => {
                crate::ui::workspace::health(object) == crate::ui::workspace::Health::Attention
            }
            Clause::Ready(expected) => {
                let actual = object.data["status"]["conditions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|condition| condition["type"] == "Ready")
                    .and_then(|condition| match condition["status"].as_str() {
                        Some("True") => Some(true),
                        Some("False") => Some(false),
                        _ => None,
                    });
                actual == *expected
            }
            Clause::Field(field, equal, expected) => {
                let actual = match field.as_str() {
                    "name" => object.metadata.name.as_deref(),
                    "namespace" => object.metadata.namespace.as_deref(),
                    "node" => object.data["spec"]["nodeName"].as_str(),
                    _ => return (*equal) == (pod::status(object) == *expected),
                };
                actual.is_some_and(|actual| (*equal) == (actual == expected))
            }
            Clause::Numeric(metric, op, expected) => {
                let actual = match metric {
                    Metric::Restarts => Some(restarts as f64),
                    Metric::Age => age.map(|n| n as f64),
                    Metric::Cpu => usage.map(|u| u.cpu_milli),
                    Metric::Memory => usage.map(|u| u.memory_bytes),
                    Metric::MemoryPercent => budget.memory_percent(usage),
                };
                actual.is_some_and(|actual| actual.is_finite() && op.matches(actual, *expected))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pod() -> DynamicObject {
        serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"payments-api","namespace":"prod","labels":{"app":"payments"}},"spec":{"nodeName":"node-a"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"False"}]}})).unwrap()
    }
    #[test]
    fn combined_numeric_boolean_text_and_label_predicates() {
        let q = Query::parse("payapi restarts>3 ready=false cpu>=500m memory>80% age<2h label:app=payments namespace=prod node=node-a").unwrap();
        let usage = Some(Usage {
            cpu_milli: 500.,
            memory_bytes: 90. * 1048576.,
        });
        let budget = Budget {
            memory_limit: Some(100. * 1048576.),
            ..Budget::default()
        };
        assert!(q.matches(&pod(), usage, budget, 4, Some(3600)));
        assert!(!q.matches(&pod(), usage, budget, 3, Some(3600)));
        assert!(!q.matches(&pod(), None, budget, 4, Some(3600)));
        assert!(!q.matches(&pod(), usage, Budget::default(), 4, Some(3600)));
    }
    #[test]
    fn absent_metrics_and_readiness_never_look_like_zero_or_false() {
        for text in ["cpu=0", "cpu!=10m", "memory<1Gi", "memory/limit<80%"] {
            assert!(
                !Query::parse(text)
                    .unwrap()
                    .matches(&pod(), None, Budget::default(), 0, None)
            );
        }
        let mut object = pod();
        object.data["status"]["conditions"] = serde_json::json!([]);
        assert!(!Query::parse("ready=false").unwrap().matches(
            &object,
            None,
            Budget::default(),
            0,
            None
        ));
        assert!(Query::parse("ready=unknown").unwrap().matches(
            &object,
            None,
            Budget::default(),
            0,
            None
        ));
    }
    #[test]
    fn invalid_conditions_are_errors_not_silent_text_filters() {
        for text in [
            "cpu>",
            "cpu>NaN",
            "cpu>inf",
            "cpu>-1",
            "age>10",
            "restarts>1.5",
            "ready=yes",
            "ready!=true",
            "unknown=1",
            "cpu==1",
            "label:=a",
            "memory/limit>80",
        ] {
            assert!(Query::parse(text).is_err(), "{text}");
        }
        for text in [
            "memory>128Mi",
            "cpu>0.5",
            "age>=2d3h",
            "label:app!=web",
            "status=Running",
            "name=payments-api",
            "ready=true",
        ] {
            assert!(Query::parse(text).is_ok(), "{text}");
        }
    }
}
