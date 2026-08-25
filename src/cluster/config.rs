use anyhow::Context as _;
use kube::config::Kubeconfig;

/// A selectable kubeconfig context, flattened for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub namespace: Option<String>,
    pub is_current: bool,
}

fn flatten(kc: Kubeconfig) -> Vec<ContextInfo> {
    let current = kc.current_context.clone().unwrap_or_default();
    kc.contexts
        .into_iter()
        .map(|named| {
            let ctx = named.context;
            ContextInfo {
                is_current: named.name == current,
                name: named.name,
                cluster: ctx.as_ref().map(|c| c.cluster.clone()).unwrap_or_default(),
                namespace: ctx.and_then(|c| c.namespace).filter(|n| !n.is_empty()),
            }
        })
        .collect()
}

/// Parse contexts from a kubeconfig string. Separated from file loading so
/// context handling is testable without touching the filesystem.
pub fn contexts_from_yaml(yaml: &str) -> anyhow::Result<Vec<ContextInfo>> {
    let kc = Kubeconfig::from_yaml(yaml).context("parsing kubeconfig")?;
    Ok(flatten(kc))
}

/// Load contexts from the standard kubeconfig location(s).
pub fn load_contexts() -> anyhow::Result<Vec<ContextInfo>> {
    let kc = Kubeconfig::read().context("reading kubeconfig")?;
    Ok(flatten(kc))
}

/// Build a client from the current context.
pub async fn connect() -> anyhow::Result<kube::Client> {
    let cfg = kube::Config::infer()
        .await
        .context("inferring cluster config — is a kubeconfig present?")?;
    kube::Client::try_from(cfg).context("building Kubernetes client")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
current-context: prod-eu
clusters:
- name: prod-cluster
  cluster:
    server: https://prod.example.com
- name: dev-cluster
  cluster:
    server: https://dev.example.com
contexts:
- name: prod-eu
  context:
    cluster: prod-cluster
    user: prod-user
    namespace: payments
- name: dev
  context:
    cluster: dev-cluster
    user: dev-user
- name: empty-ns
  context:
    cluster: dev-cluster
    user: dev-user
    namespace: ""
users: []
"#;

    #[test]
    fn parses_all_contexts() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert_eq!(ctxs.len(), 3);
        assert_eq!(ctxs[0].name, "prod-eu");
        assert_eq!(ctxs[1].name, "dev");
        assert_eq!(ctxs[2].name, "empty-ns");
    }

    #[test]
    fn marks_the_current_context() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert!(ctxs[0].is_current, "prod-eu is current-context");
        assert!(!ctxs[1].is_current);
    }

    #[test]
    fn an_explicitly_empty_namespace_becomes_none() {
        // An explicit `namespace: ""` deserializes to Some("") — the filter in
        // flatten() is what normalises it. Without this case that filter is
        // unguarded and can be deleted without failing any test.
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        let empty = ctxs
            .iter()
            .find(|c| c.name == "empty-ns")
            .expect("empty-ns context");
        assert_eq!(
            empty.namespace, None,
            "empty string must normalise to None, not Some(\"\")"
        );
    }

    #[test]
    fn captures_cluster_and_default_namespace() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert_eq!(ctxs[0].cluster, "prod-cluster");
        assert_eq!(ctxs[0].namespace.as_deref(), Some("payments"));
        assert_eq!(
            ctxs[1].namespace, None,
            "absent namespace stays None, not empty string"
        );
    }

    #[test]
    fn malformed_yaml_is_an_error_not_a_panic() {
        assert!(contexts_from_yaml("this: is: not: valid: kubeconfig").is_err());
    }
}
