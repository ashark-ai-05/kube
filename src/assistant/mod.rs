//! A small, read-only action vocabulary. Neither parser nor model can produce shell commands.
pub mod local;
use crate::ui::inspector::Mode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Current,
    All,
    Named(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    Browse {
        kind: String,
        scope: Scope,
        attention: bool,
    },
    Namespace(Scope),
    Cluster(String),
    Inspect {
        mode: Mode,
        previous: bool,
    },
}
impl Intent {
    pub fn describe(&self) -> String {
        match self {
            Self::Browse {
                kind,
                scope,
                attention,
            } => format!(
                "Show {}{kind} · {}",
                if *attention { "unhealthy " } else { "" },
                scope.label()
            ),
            Self::Namespace(scope) => format!("Select {}", scope.label()),
            Self::Cluster(name) => format!("Choose cluster context: {name}"),
            Self::Inspect { mode, previous } => format!(
                "Open {} for the selected resource",
                if *previous {
                    "previous container logs".into()
                } else {
                    format!("{mode:?}").to_lowercase()
                }
            ),
        }
    }
}
impl Scope {
    pub fn label(&self) -> String {
        match self {
            Self::Current => "current namespace".into(),
            Self::All => "all namespaces".into(),
            Self::Named(s) => format!("namespace {s}"),
        }
    }
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "" => Ok(Self::Current),
            "*" => Ok(Self::All),
            name if crate::cluster::is_valid_namespace_name(name) => Ok(Self::Named(name.into())),
            _ => Err("The namespace is not valid. Use the namespace picker.".into()),
        }
    }
}
pub fn check_query(query: &str) -> Result<(), String> {
    if query.trim().is_empty()
        || query.len() > 512
        || query
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>'))
    {
        return Err("Use a plain-language request of 1–512 bytes.".into());
    }
    let lower = query.to_ascii_lowercase();
    let words: Vec<_> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    let diagnostic = lower.starts_with("why ")
        || lower.starts_with("what happened")
        || lower.contains("previous logs");
    if !diagnostic
        && words.iter().any(|w| {
            matches!(
                *w,
                "delete"
                    | "remove"
                    | "destroy"
                    | "restart"
                    | "scale"
                    | "patch"
                    | "apply"
                    | "exec"
                    | "execute"
                    | "create"
                    | "drain"
                    | "cordon"
                    | "uncordon"
                    | "kubectl"
            )
        })
    {
        return Err("Ask Kube supports reading and navigation. Use the explicit commands for cluster changes.".into());
    }
    Ok(())
}
/// Deliberately narrow, predictable fast path. Unsupported wording falls back to a model proposal.
pub fn parse(query: &str) -> Result<Option<Intent>, String> {
    check_query(query)?;
    let lower = query.trim().trim_end_matches('?').to_ascii_lowercase();
    let words: Vec<_> = lower.split_whitespace().collect();
    let inspect = |mode, previous| Ok(Some(Intent::Inspect { mode, previous }));
    if (lower.starts_with("why ")
        || lower.starts_with("what happened")
        || lower.starts_with("diagnose "))
        && (lower.contains("this ") || lower.contains("selected "))
    {
        return inspect(Mode::Overview, false);
    }
    let log_request = lower
        .strip_suffix(" for this pod")
        .or_else(|| lower.strip_suffix(" for this deployment"))
        .or_else(|| lower.strip_suffix(" for this workload"))
        .or_else(|| lower.strip_suffix(" for the selected pod"))
        .unwrap_or(&lower);
    if matches!(
        log_request,
        "logs"
            | "show logs"
            | "show me logs"
            | "live logs"
            | "show live logs"
            | "previous logs"
            | "show previous logs"
            | "open logs"
    ) {
        return inspect(Mode::Logs, log_request.contains("previous"));
    }
    if matches!(
        lower.as_str(),
        "show events"
            | "events"
            | "show yaml"
            | "yaml"
            | "show metrics"
            | "metrics"
            | "show related resources"
    ) {
        return inspect(
            if lower.contains("events") {
                Mode::Events
            } else if lower.contains("yaml") {
                Mode::Yaml
            } else if lower.contains("metrics") {
                Mode::Metrics
            } else {
                Mode::Related
            },
            false,
        );
    }
    for prefix in [
        "switch cluster ",
        "switch to cluster ",
        "switch context ",
        "switch to ",
    ] {
        if let Some(name) = lower
            .starts_with(prefix)
            .then(|| &query.trim()[prefix.len()..])
            .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
        {
            return Ok(Some(Intent::Cluster(name.into())));
        }
    }
    for prefix in [
        "switch namespace ",
        "switch to namespace ",
        "use namespace ",
    ] {
        if let Some(name) = lower.strip_prefix(prefix) {
            return Ok(Some(Intent::Namespace(Scope::parse(if name == "all" {
                "*"
            } else {
                name
            })?)));
        }
    }
    if words
        .first()
        .is_some_and(|s| matches!(*s, "show" | "list" | "find"))
    {
        let kind = words.iter().find_map(|s| match *s {
            "pod" | "pods" => Some("Pod"),
            "deployment" | "deployments" => Some("Deployment"),
            "statefulset" | "statefulsets" => Some("StatefulSet"),
            "daemonset" | "daemonsets" => Some("DaemonSet"),
            "job" | "jobs" => Some("Job"),
            "service" | "services" => Some("Service"),
            _ => None,
        });
        if let Some(kind) = kind {
            let scope = if lower.contains("all namespaces") {
                Scope::All
            } else if let Some(i) = words.iter().position(|s| *s == "in") {
                Scope::parse(words.get(i + 1).copied().unwrap_or(""))?
            } else {
                Scope::Current
            };
            let allowed = [
                "show",
                "list",
                "find",
                "me",
                "all",
                "the",
                "pod",
                "pods",
                "deployment",
                "deployments",
                "statefulset",
                "statefulsets",
                "daemonset",
                "daemonsets",
                "job",
                "jobs",
                "service",
                "services",
                "failing",
                "unhealthy",
                "pending",
                "stuck",
                "crashing",
                "in",
                "namespaces",
            ];
            // Names, labels and arbitrary conditions must not be silently dropped.
            if words
                .iter()
                .any(|w| !allowed.contains(w) && !matches!(&scope,Scope::Named(n) if n==w))
            {
                return Ok(None);
            }
            let attention = words.iter().any(|w| {
                matches!(
                    *w,
                    "failing" | "unhealthy" | "pending" | "stuck" | "crashing"
                )
            });
            return Ok(Some(Intent::Browse {
                kind: kind.into(),
                scope,
                attention,
            }));
        }
    }
    Ok(None)
}

/// Decode exactly one native FunctionGemma call, rejecting extra fields and unrequested scope.
pub fn decode(output: &str, query: &str) -> Result<Intent, String> {
    check_query(query)?;
    let output = output
        .trim()
        .strip_suffix("<end_function_call>")
        .unwrap_or(output.trim());
    let body = output
        .strip_prefix("<start_function_call>call:")
        .ok_or("The model did not return a supported action. Rephrase the request.")?;
    let (name, args) = body.split_once('{').ok_or("Invalid model action")?;
    let args = args.strip_suffix('}').ok_or("Incomplete model action")?;
    let mut params = std::collections::BTreeMap::new();
    let mut remaining = args;
    while !remaining.is_empty() {
        let (key, value) = remaining
            .split_once(":<escape>")
            .ok_or("Invalid model argument")?;
        let (value, tail) = value
            .split_once("<escape>")
            .ok_or("Incomplete model argument")?;
        if params.insert(key, value).is_some() {
            return Err("Duplicate model argument".into());
        }
        remaining = if tail.is_empty() {
            ""
        } else {
            tail.strip_prefix(',')
                .ok_or("Invalid model argument separator")?
        };
    }
    let allowed: &[&str] = match name {
        "list_pods" => &["namespace", "health"],
        "list_deployments" | "switch_namespace" => &["namespace"],
        "switch_cluster" => &["context"],
        "show_logs" | "show_previous_logs" | "show_events" | "inspect_workload" | "show_yaml" => {
            &[]
        }
        _ => return Err("This request is outside the supported read-only actions.".into()),
    };
    if params.keys().any(|k| !allowed.contains(k)) {
        return Err("The model added an unsupported argument.".into());
    }
    let namespace = params.get("namespace").copied().unwrap_or("");
    // An invented default namespace must never silently redirect a request.
    if !namespace.is_empty()
        && namespace != "*"
        && !query
            .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '?' | ','))
            .any(|word| word == namespace)
    {
        return Err(
            "The model proposed a namespace you did not name. Please specify the namespace.".into(),
        );
    }
    let scope = Scope::parse(namespace)?;
    Ok(match name {
        "list_pods" | "list_deployments" => Intent::Browse {
            kind: if name == "list_pods" {
                "Pod"
            } else {
                "Deployment"
            }
            .into(),
            scope,
            attention: match params.get("health").copied().unwrap_or("all") {
                "all" => false,
                "unhealthy" => true,
                _ => return Err("The model returned an unsupported health filter.".into()),
            },
        },
        "switch_namespace" => Intent::Namespace(scope),
        "switch_cluster" => {
            let name = *params
                .get("context")
                .ok_or("The model did not name a cluster.")?;
            if name.is_empty()
                || name.len() > 256
                || name
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '<' | '>'))
            {
                return Err("Invalid cluster name".into());
            }
            Intent::Cluster(name.into())
        }
        _ => Intent::Inspect {
            mode: match name {
                "show_logs" | "show_previous_logs" => Mode::Logs,
                "show_events" => Mode::Events,
                "show_yaml" => Mode::Yaml,
                _ => Mode::Overview,
            },
            previous: name == "show_previous_logs",
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_requests_preserve_scope_and_intent() {
        assert_eq!(
            parse("Show failing pods in payments").unwrap(),
            Some(Intent::Browse {
                kind: "Pod".into(),
                scope: Scope::Named("payments".into()),
                attention: true
            })
        );
        assert_eq!(
            parse("Previous logs for this pod").unwrap(),
            Some(Intent::Inspect {
                mode: Mode::Logs,
                previous: true
            })
        );
        assert_eq!(
            parse("Why is this deployment stuck?").unwrap(),
            Some(Intent::Inspect {
                mode: Mode::Overview,
                previous: false
            })
        );
        assert!(parse("show pods named api").unwrap().is_none());
    }
    #[test]
    fn no_mutations_or_prompt_delimiters() {
        for q in [
            "please delete all pods",
            "scale web to 4",
            "exec bash",
            "hello <start_of_turn>",
        ] {
            assert!(parse(q).is_err(), "{q}");
        }
    }
    #[test]
    fn invented_namespaces_and_extra_calls_fail_closed() {
        assert!(
            decode(
                "<start_function_call>call:list_deployments{namespace:<escape>default<escape>}",
                "show deployments"
            )
            .is_err()
        );
        assert!(
            decode(
                "<start_function_call>call:show_logs{}<start_function_call>call:show_yaml{}",
                "logs"
            )
            .is_err()
        );
        assert!(
            decode(
                "<start_function_call>call:show_logs{shell:<escape>rm -rf<escape>}",
                "logs"
            )
            .is_err()
        );
    }
}
