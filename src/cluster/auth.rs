use anyhow::{Context as _, anyhow};
use kube::config::{KubeConfigOptions, Kubeconfig};
use std::path::{Path, PathBuf};

/// How a context authenticates. Surfaced in the UI so that an auth failure
/// points at a cause rather than a generic 401.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    ClientCert,
    Token,
    /// A credential plugin such as kubelogin or aws-iam-authenticator. The
    /// command is captured because "binary not on PATH" is the usual failure.
    Exec {
        command: String,
    },
    /// An auth-provider block, most commonly OIDC/SSO.
    AuthProvider {
        name: String,
    },
    None,
}

/// Determine how a named user authenticates.
///
/// Order matters: exec and auth-provider are checked before the credential
/// fields, because a kubeconfig may carry a stale cached token alongside the
/// plugin that is actually used to refresh it.
pub fn auth_method_for(kc: &Kubeconfig, user_name: &str) -> AuthMethod {
    let Some(named) = kc.auth_infos.iter().find(|u| u.name == user_name) else {
        return AuthMethod::None;
    };
    let Some(ai) = &named.auth_info else {
        return AuthMethod::None;
    };

    if let Some(exec) = &ai.exec {
        return AuthMethod::Exec {
            command: exec.command.clone().unwrap_or_default(),
        };
    }
    if let Some(provider) = &ai.auth_provider {
        return AuthMethod::AuthProvider {
            name: provider.name.clone(),
        };
    }
    if ai.client_certificate_data.is_some() || ai.client_certificate.is_some() {
        return AuthMethod::ClientCert;
    }
    if ai.token.is_some() || ai.token_file.is_some() {
        return AuthMethod::Token;
    }
    AuthMethod::None
}

/// Options controlling how a client is built.
///
/// The derived `Default` gives `kubeconfig_paths: vec![]`, `context: None`
/// (use the kubeconfig's current-context), and `accept_invalid_certs: false`
/// (TLS verification stays on unless explicitly disabled).
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Kubeconfig files to load and merge, in precedence order.
    pub kubeconfig_paths: Vec<PathBuf>,
    /// Context to connect to. `None` uses the kubeconfig's current-context.
    pub context: Option<String>,
    /// Skip TLS verification. Defaults to false and should stay that way
    /// outside deliberate debugging.
    pub accept_invalid_certs: bool,
}

/// Resolve which kubeconfig files to read, following KUBECONFIG semantics:
/// a colon-separated list, falling back to ~/.kube/config.
pub fn kubeconfig_paths_from_env(var: Option<&str>, home: &Path) -> Vec<PathBuf> {
    match var {
        Some(v) if !v.trim().is_empty() => v
            .split(':')
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect(),
        _ => vec![home.join(".kube").join("config")],
    }
}

/// Merge kubeconfigs in precedence order; earlier entries win on conflict.
pub fn merge_kubeconfigs(configs: Vec<Kubeconfig>) -> anyhow::Result<Kubeconfig> {
    let mut iter = configs.into_iter();
    let first = iter
        .next()
        .ok_or_else(|| anyhow!("no kubeconfig files to merge"))?;
    iter.try_fold(first, |acc, next| {
        acc.merge(next).context("merging kubeconfig files")
    })
}

/// Build a client for the requested context, merging every configured
/// kubeconfig first so one unified file (or several) can define many clusters.
pub async fn connect_with(opts: &ConnectOptions) -> anyhow::Result<kube::Client> {
    let mut loaded = Vec::new();
    for path in &opts.kubeconfig_paths {
        loaded.push(
            Kubeconfig::read_from(path)
                .with_context(|| format!("reading kubeconfig {}", path.display()))?,
        );
    }
    let merged = if loaded.is_empty() {
        Kubeconfig::read().context("reading kubeconfig")?
    } else {
        merge_kubeconfigs(loaded)?
    };

    let kco = KubeConfigOptions {
        context: opts.context.clone(),
        cluster: None,
        user: None,
    };
    let mut cfg = kube::Config::from_custom_kubeconfig(merged, &kco)
        .await
        .with_context(|| match &opts.context {
            Some(c) => format!("building config for context '{c}'"),
            None => "building config for the current context".to_string(),
        })?;
    cfg.accept_invalid_certs = opts.accept_invalid_certs;

    kube::Client::try_from(cfg).context("building Kubernetes client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const MULTI: &str = r#"
apiVersion: v1
kind: Config
current-context: prod
clusters:
- name: prod-cluster
  cluster:
    server: https://prod.corp.example.com
contexts:
- name: prod
  context:
    cluster: prod-cluster
    user: cert-user
- name: sso
  context:
    cluster: prod-cluster
    user: oidc-user
- name: cli
  context:
    cluster: prod-cluster
    user: exec-user
users:
- name: cert-user
  user:
    client-certificate-data: Zm9v
    client-key-data: YmFy
- name: oidc-user
  user:
    auth-provider:
      name: oidc
      config:
        idp-issuer-url: https://sso.corp.example.com
- name: exec-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: kubelogin
      args: ["get-token", "--server-id", "abc"]
- name: token-user
  user:
    token: abcdef123456
- name: exec-with-stale-token
  user:
    token: stale-cached-value
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: kubelogin
      args: ["get-token"]
- name: oidc-with-stale-cert
  user:
    client-certificate-data: Zm9v
    client-key-data: YmFy
    auth-provider:
      name: oidc
      config:
        idp-issuer-url: https://sso.corp.example.com
"#;

    fn kc() -> kube::config::Kubeconfig {
        kube::config::Kubeconfig::from_yaml(MULTI).expect("valid kubeconfig")
    }

    #[test]
    fn detects_client_certificate_auth() {
        assert_eq!(auth_method_for(&kc(), "cert-user"), AuthMethod::ClientCert);
    }

    #[test]
    fn detects_oidc_auth_provider_by_name() {
        assert_eq!(
            auth_method_for(&kc(), "oidc-user"),
            AuthMethod::AuthProvider {
                name: "oidc".to_string()
            }
        );
    }

    #[test]
    fn detects_exec_plugin_and_captures_the_command() {
        assert_eq!(
            auth_method_for(&kc(), "exec-user"),
            AuthMethod::Exec {
                command: "kubelogin".to_string()
            },
            "the helper binary name must be surfaced so a missing PATH entry is diagnosable"
        );
    }

    #[test]
    fn detects_static_token_auth() {
        assert_eq!(auth_method_for(&kc(), "token-user"), AuthMethod::Token);
    }

    #[test]
    fn an_exec_plugin_wins_over_a_stale_cached_token() {
        // A corporate kubeconfig routinely carries a cached token beside the
        // plugin that refreshes it. Reporting Token here would send someone
        // chasing an expired credential instead of their exec helper.
        assert_eq!(
            auth_method_for(&kc(), "exec-with-stale-token"),
            AuthMethod::Exec {
                command: "kubelogin".to_string()
            }
        );
    }

    #[test]
    fn an_auth_provider_wins_over_stale_certificate_data() {
        assert_eq!(
            auth_method_for(&kc(), "oidc-with-stale-cert"),
            AuthMethod::AuthProvider {
                name: "oidc".to_string()
            }
        );
    }

    #[test]
    fn an_unknown_user_reports_none_rather_than_panicking() {
        assert_eq!(auth_method_for(&kc(), "nobody"), AuthMethod::None);
    }

    #[test]
    fn kubeconfig_paths_split_on_colon_like_the_kubeconfig_env_var() {
        let home = PathBuf::from("/home/u");
        let paths = kubeconfig_paths_from_env(Some("/a/one.yaml:/b/two.yaml"), &home);
        assert_eq!(
            paths,
            vec![PathBuf::from("/a/one.yaml"), PathBuf::from("/b/two.yaml")]
        );
    }

    #[test]
    fn kubeconfig_paths_default_to_home_when_env_is_unset() {
        let home = PathBuf::from("/home/u");
        let paths = kubeconfig_paths_from_env(None, &home);
        assert_eq!(paths, vec![PathBuf::from("/home/u/.kube/config")]);
    }

    #[test]
    fn empty_segments_in_the_env_var_are_ignored() {
        let home = PathBuf::from("/home/u");
        let paths = kubeconfig_paths_from_env(Some("/a/one.yaml::"), &home);
        assert_eq!(
            paths,
            vec![PathBuf::from("/a/one.yaml")],
            "a trailing colon must not yield an empty path"
        );
    }

    #[test]
    fn merging_keeps_contexts_from_every_file() {
        let a = kube::config::Kubeconfig::from_yaml(
            "apiVersion: v1\nkind: Config\ncontexts:\n- name: alpha\n  context:\n    cluster: c1\n    user: u1\nclusters: []\nusers: []\n",
        )
        .unwrap();
        let b = kube::config::Kubeconfig::from_yaml(
            "apiVersion: v1\nkind: Config\ncontexts:\n- name: beta\n  context:\n    cluster: c2\n    user: u2\nclusters: []\nusers: []\n",
        )
        .unwrap();
        let merged = merge_kubeconfigs(vec![a, b]).unwrap();
        let names: Vec<String> = merged.contexts.iter().map(|c| c.name.clone()).collect();
        assert!(names.contains(&"alpha".to_string()), "got {names:?}");
        assert!(names.contains(&"beta".to_string()), "got {names:?}");
    }

    #[test]
    fn merging_an_empty_list_is_an_error_not_a_panic() {
        assert!(merge_kubeconfigs(vec![]).is_err());
    }

    #[test]
    fn connect_options_default_to_current_context_and_secure_tls() {
        let o = ConnectOptions::default();
        assert_eq!(
            o.context, None,
            "None means: use the kubeconfig's current-context"
        );
        assert!(
            !o.accept_invalid_certs,
            "TLS verification must default to on"
        );
    }
}
