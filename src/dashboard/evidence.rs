//! Observed Kubernetes facts, with source paths and times. No inferred root causes.
use super::{
    Row,
    events::Events,
    pod::{self, Identity},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTarget {
    pub container: String,
    pub previous: bool,
}
#[derive(Clone, Debug)]
pub struct Finding {
    pub title: String,
    pub detail: String,
    pub source: String,
    pub timestamp: Option<String>,
    pub warning: bool,
    pub logs: Option<LogTarget>,
}
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(8192)
        .collect()
}
pub fn collect(row: &Row, events: &Events) -> Vec<Finding> {
    let object = &row.object;
    let mut findings = Vec::new();
    for key in ["initContainerStatuses", "containerStatuses"] {
        for container in object.data["status"][key].as_array().into_iter().flatten() {
            let name = container["name"].as_str().unwrap_or("container");
            for state in ["state", "lastState"] {
                let previous = state == "lastState";
                if let Some(ended) = container[state].get("terminated") {
                    let exit = ended["exitCode"].as_i64();
                    let reason = ended["reason"].as_str().unwrap_or("Terminated");
                    if !previous && key == "initContainerStatuses" && exit == Some(0) {
                        continue;
                    }
                    findings.push(Finding {
                        title:clean(&format!("{name} · {reason}{}",if previous{" · previous instance"}else{""})),
                        detail:clean(&format!("{}\nExit code: {} · signal: {} · recorded restarts: {}\nStarted: {}\nFinished: {}\n{}",
                            if previous {"The previous container instance terminated."}else{"The current container instance terminated."},
                            exit.map(|n|n.to_string()).unwrap_or_else(||"unknown".into()),
                            ended["signal"].as_i64().map(|n|n.to_string()).unwrap_or_else(||"not recorded".into()),
                            container["restartCount"].as_u64().map(|n|n.to_string()).unwrap_or_else(||"unknown".into()),
                            ended["startedAt"].as_str().unwrap_or("not recorded"),ended["finishedAt"].as_str().unwrap_or("not recorded"),
                            ended["message"].as_str().unwrap_or("No termination message recorded."))),
                        source:format!("status.{key}[{name}].{state}.terminated"),
                        timestamp:ended["finishedAt"].as_str().map(str::to_string),
                        warning:exit.is_some_and(|n|n!=0) || reason=="OOMKilled",
                        logs:Some(LogTarget{container:name.into(),previous}),
                    });
                }
            }
            if let Some(waiting) = container["state"].get("waiting") {
                findings.push(Finding {
                    title: clean(&format!(
                        "{name} · {}",
                        waiting["reason"].as_str().unwrap_or("Waiting")
                    )),
                    detail: clean(waiting["message"].as_str().unwrap_or(
                        "Kubernetes reports this container is waiting; no message was recorded.",
                    )),
                    source: format!("status.{key}[{name}].state.waiting"),
                    timestamp: None,
                    warning: true,
                    logs: container["lastState"].get("terminated").map(|_| LogTarget {
                        container: name.into(),
                        previous: true,
                    }),
                });
            }
        }
    }
    for condition in object.data["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let kind = condition["type"].as_str().unwrap_or("Condition");
        let status = condition["status"].as_str().unwrap_or("Unknown");
        findings.push(Finding {
            title: clean(&format!("{kind}: {status}")),
            detail: clean(&format!(
                "Reason: {}\n{}",
                condition["reason"].as_str().unwrap_or("not recorded"),
                condition["message"]
                    .as_str()
                    .unwrap_or("No condition message recorded.")
            )),
            source: format!("status.conditions[{kind}]"),
            timestamp: condition["lastTransitionTime"].as_str().map(str::to_string),
            warning: status != "True"
                && matches!(
                    kind,
                    "Ready"
                        | "ContainersReady"
                        | "Initialized"
                        | "PodScheduled"
                        | "PodReadyToStartContainers"
                ),
            logs: None,
        });
    }
    for event in events.rows_for(&Identity::of(object)) {
        findings.push(Finding { title:clean(&format!("{} · {}",event.kind,event.reason)),
            detail:clean(&format!("{}\nRecorded occurrences: {}\nAn event records an observation; it may describe a condition that has since resolved.",event.message,event.count)),
            source:"Kubernetes Event · selected pod UID".into(),timestamp:event.timestamp.clone(),warning:event.kind=="Warning",logs:None });
    }
    findings.sort_by_key(|f| !f.warning);
    if findings.is_empty() {
        findings.push(Finding {title:format!("Status: {}",pod::status(object)),detail:"No container failures or pod conditions are recorded in this snapshot. This does not establish that the application is healthy. Open Events or logs for more evidence.".into(),source:"status.phase".into(),timestamp:None,warning:false,logs:None});
    }
    findings
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn evidence_distinguishes_previous_termination_current_waiting_and_condition_times() {
        let object=Arc::new(serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"demo","uid":"new"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"False","reason":"ContainersNotReady","lastTransitionTime":"2026-09-28T01:00:01Z"}],"containerStatuses":[{"name":"worker","restartCount":4,"state":{"waiting":{"reason":"CrashLoopBackOff","message":"backoff\u{001b}"}},"lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"finishedAt":"2026-09-28T01:00:00Z"}}}]}})).unwrap());
        let row = Row {
            object,
            usage: None,
            budget: Default::default(),
            restarts: 4,
            age: None,
        };
        let findings = collect(&row, &Events::default());
        assert_eq!(findings.len(), 3);
        assert!(findings[0].detail.contains("137"));
        assert_eq!(
            findings[0].logs.as_ref().unwrap(),
            &LogTarget {
                container: "worker".into(),
                previous: true
            }
        );
        assert_eq!(
            findings[0].timestamp.as_deref(),
            Some("2026-09-28T01:00:00Z")
        );
        assert_eq!(findings[1].timestamp, None);
        assert!(!findings[1].detail.contains('\u{001b}'));
        assert!(findings[2].source.contains("Ready"));
    }
}
