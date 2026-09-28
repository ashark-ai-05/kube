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
            | "show current container output"
            | "show current logs"
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
    if matches!(
        lower.as_str(),
        "select every namespace"
            | "select all namespaces"
            | "use all namespaces"
            | "use every namespace"
    ) {
        return Ok(Some(Intent::Namespace(Scope::All)));
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

/// Closed JSON vocabulary shared with constrained native inference. Unknown or duplicate
/// fields are errors, never ignored instructions or executable commands.
#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum ModelAction {
    Browse {
        kind: String,
        namespace: String,
        attention: bool,
    },
    Inspect {
        view: String,
        previous: bool,
    },
    Namespace {
        namespace: String,
    },
    Cluster {
        context: String,
    },
    Reject,
}
pub fn decode(output: &str, query: &str) -> Result<Intent, String> {
    check_query(query)?;
    let action: ModelAction = serde_json::from_str(output)
        .map_err(|_| "The model did not return one supported action. Rephrase the request.")?;
    let lower = query.to_ascii_lowercase();
    let words: Vec<_> = query
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '?' | ',' | '.' | ':'))
        .filter(|s| !s.is_empty())
        .collect();
    let all_requested = regex::Regex::new(r"\b(?:all|every|each)\s+(?:of the\s+)?namespaces?\b")
        .unwrap()
        .is_match(&lower);
    let scope = |name: &str| -> Result<Scope, String> {
        let scope = Scope::parse(name)?;
        if all_requested != (scope == Scope::All) {
            return Err("The model changed the requested namespace scope. Please rephrase.".into());
        }
        if let Scope::Named(name) = &scope
            && !words.contains(&name.as_str())
        {
            return Err(
                "The model proposed a namespace you did not name. Please specify the namespace."
                    .into(),
            );
        }
        Ok(scope)
    };
    let intent = match action {
        ModelAction::Browse {
            kind,
            namespace,
            attention,
        } => {
            if ![
                "Pod",
                "Deployment",
                "StatefulSet",
                "DaemonSet",
                "Job",
                "Service",
            ]
            .contains(&kind.as_str())
            {
                return Err("Unsupported resource kind".into());
            }
            Intent::Browse {
                kind,
                scope: scope(&namespace)?,
                attention,
            }
        }
        ModelAction::Namespace { namespace } => Intent::Namespace(scope(&namespace)?),
        ModelAction::Cluster { context } => {
            if context.is_empty() || context.len() > 256 || !words.contains(&context.as_str()) {
                return Err(
                    "The model proposed a context you did not name. Use the cluster picker.".into(),
                );
            }
            Intent::Cluster(context)
        }
        ModelAction::Inspect { view, previous } => {
            let mode = match view.as_str() {
                "logs" => Mode::Logs,
                "overview" => Mode::Overview,
                "yaml" => Mode::Yaml,
                "events" => Mode::Events,
                "metrics" => Mode::Metrics,
                "related" => Mode::Related,
                _ => return Err("Unsupported inspection view".into()),
            };
            if previous && mode != Mode::Logs {
                return Err("Previous instances apply only to logs".into());
            }
            Intent::Inspect { mode, previous }
        }
        ModelAction::Reject => {
            return Err("This request is outside the supported read-only navigation. Use the filters or Commands.".into());
        }
    };
    validate_model_request(&intent, query)?;
    if let Some(expected) = parse(query)?
        && expected != intent
    {
        return Err(
            "The model changed a recognized request. Please use the built-in interpretation."
                .into(),
        );
    }
    Ok(intent)
}
// Schema validity does not prove that the model preserved the requested constraints.
fn validate_model_request(intent: &Intent, query: &str) -> Result<(), String> {
    let lower = query.to_ascii_lowercase();
    let words: Vec<_> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|s| !s.is_empty())
        .collect();
    let has = |word: &str| words.contains(&word);
    let selected = has("this") || has("selected");
    let compound = has("then")
        || query.contains(';')
        || (has("and")
            && !(matches!(
                intent,
                Intent::Inspect {
                    mode: Mode::Metrics,
                    ..
                }
            ) && has("cpu")
                && has("memory")
                && selected));
    if compound {
        return Err("Ask for one navigation action at a time.".into());
    }
    if has("except") || has("excluding") || lower.contains("not in ") {
        return Err(
            "Namespace exclusions are not supported by Ask Kube. Use the namespace picker.".into(),
        );
    }
    match intent {
        Intent::Browse { scope, .. } => {
            if [
                "sort",
                "sorted",
                "oldest",
                "newest",
                "older",
                "younger",
                "restarts",
                "cpu",
                "memory",
                "label",
                "labels",
                "named",
                "contains",
                "created",
                "yesterday",
                "exactly",
                "related",
                "manifest",
                "logs",
                "yaml",
                "events",
                "metrics",
            ]
            .iter()
            .any(|word| has(word))
            {
                return Err("The request includes an unsupported list condition or a different view. Use filters, sort, or inspect the selected resource.".into());
            }
            if selected {
                return Err("Select an inspection view for the current resource.".into());
            }
            if let Some(capture) =
                regex::Regex::new(r"\b(?:in|within)\s+(?:namespace\s+)?([a-z0-9][a-z0-9-]*)")
                    .unwrap()
                    .captures(&lower)
            {
                let named = &capture[1];
                if !["all", "every", "each", "the", "current", "this"].contains(&named)
                    && !matches!(scope,Scope::Named(ns) if ns==named)
                {
                    return Err("The model omitted or changed the namespace you named.".into());
                }
            }
        }
        Intent::Inspect { mode, previous } => {
            if !selected
                && [
                    "pod",
                    "pods",
                    "deployment",
                    "deployments",
                    "service",
                    "services",
                    "workload",
                    "container",
                    "for",
                    "of",
                    "from",
                ]
                .iter()
                .any(|word| has(word))
            {
                return Err("Inspection targets the selection. Select a resource first and refer to this or selected resource.".into());
            }
            if *mode == Mode::Logs
                && *previous
                && !["previous", "prior", "before", "last", "earlier"]
                    .iter()
                    .any(|word| has(word))
            {
                return Err("The model requested previous logs without an earlier instance being requested.".into());
            }
        }
        _ => {}
    }
    Ok(())
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
    fn invented_scope_extra_fields_duplicate_fields_and_writes_fail_closed() {
        for (output, query) in [
            (
                r#"{"action":"browse","kind":"Deployment","namespace":"default","attention":false}"#,
                "bring up deployments",
            ),
            (
                r#"{"action":"browse","kind":"Pod","namespace":"all","attention":false}"#,
                "pods in all namespaces",
            ),
            (
                r#"{"action":"inspect","view":"logs","previous":false,"shell":"kubectl delete pods"}"#,
                "logs",
            ),
            (
                r#"{"action":"inspect","view":"logs","view":"yaml","previous":false}"#,
                "logs",
            ),
            (r#"{"action":"delete","name":"web"}"#, "inspect web"),
            (
                r#"{"action":"cluster","context":"invented"}"#,
                "use cluster east",
            ),
            (
                r#"{"action":"inspect","view":"events","previous":true}"#,
                "events",
            ),
            (
                r#"{"action":"inspect","view":"logs","previous":true}"#,
                "show me logs",
            ),
            (r#"{"action":"reject"} {"action":"reject"}"#, "hello"),
        ] {
            assert!(decode(output, query).is_err(), "{query}: {output}");
        }
    }
    #[test]
    fn json_actions_preserve_model_navigation_scope_and_container_instance() {
        assert_eq!(
            decode(
                r#"{"action":"browse","kind":"Service","namespace":"*","attention":false}"#,
                "Please display services across every namespace"
            )
            .unwrap(),
            Intent::Browse {
                kind: "Service".into(),
                scope: Scope::All,
                attention: false
            }
        );
        assert_eq!(
            decode(
                r#"{"action":"inspect","view":"logs","previous":true}"#,
                "I need output from the previous container instance of this pod"
            )
            .unwrap(),
            Intent::Inspect {
                mode: Mode::Logs,
                previous: true
            }
        );
        assert_eq!(
            decode(
                r#"{"action":"cluster","context":"Team-East"}"#,
                "Change context to Team-East"
            )
            .unwrap(),
            Intent::Cluster("Team-East".into())
        );
    }

    #[test]
    fn model_cannot_drop_a_named_target_condition_or_second_action() {
        let logs = r#"{"action":"inspect","view":"logs","previous":false}"#;
        for query in [
            "Show logs for pod payment-abc",
            "Show this pod logs and YAML",
        ] {
            assert!(decode(logs, query).is_err());
        }
        let browse = r#"{"action":"browse","kind":"Pod","namespace":"","attention":false}"#;
        for query in [
            "Show pods in billing",
            "Pods except kube-system",
            "Show pods sorted by age",
            "Pods with 3 restarts",
            "Show pods then show logs",
        ] {
            assert!(decode(browse, query).is_err(), "{query}");
        }
    }
}
