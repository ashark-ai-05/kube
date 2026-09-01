//! Resolve the user kubeconfig path. Never invent a cluster.
use std::env;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KubeconfigError {
    #[error("KUBECONFIG is set but empty")]
    EmptyEnv,
    #[error("kubeconfig not found at {0}")]
    Missing(PathBuf),
    #[error("could not determine home directory for ~/.kube/config")]
    NoHome,
}

/// First path in `KUBECONFIG` (colon-separated on Unix) or `$HOME/.kube/config`.
pub fn resolve_kubeconfig_path() -> Result<PathBuf, KubeconfigError> {
    resolve_kubeconfig_path_with(env::var_os("KUBECONFIG"), dirs::home_dir(), Path::new(":"))
}

/// Testable resolver. `path_sep` is `:` on Unix.
pub fn resolve_kubeconfig_path_with(
    kubeconfig_env: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    _path_sep: &Path,
) -> Result<PathBuf, KubeconfigError> {
    if let Some(raw) = kubeconfig_env {
        let s = raw.to_string_lossy();
        if s.trim().is_empty() {
            return Err(KubeconfigError::EmptyEnv);
        }
        let first = s
            .split(':')
            .map(str::trim)
            .find(|p| !p.is_empty())
            .ok_or(KubeconfigError::EmptyEnv)?;
        Ok(PathBuf::from(first))
    } else {
        let home = home.ok_or(KubeconfigError::NoHome)?;
        Ok(home.join(".kube").join("config"))
    }
}

/// Require the resolved path to exist (used at connect time).
pub fn require_kubeconfig(path: &Path) -> Result<(), KubeconfigError> {
    if path.exists() {
        Ok(())
    } else {
        Err(KubeconfigError::Missing(path.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn kubeconfig_env_takes_first_path() {
        let p = resolve_kubeconfig_path_with(
            Some(OsString::from("/tmp/a:/tmp/b")),
            Some(PathBuf::from("/home/u")),
            Path::new(":"),
        )
        .unwrap();
        assert_eq!(p, PathBuf::from("/tmp/a"));
    }

    #[test]
    fn empty_kubeconfig_env_is_error() {
        let err = resolve_kubeconfig_path_with(
            Some(OsString::from("   ")),
            Some(PathBuf::from("/home/u")),
            Path::new(":"),
        )
        .unwrap_err();
        assert_eq!(err, KubeconfigError::EmptyEnv);
    }

    #[test]
    fn falls_back_to_home_dot_kube_config() {
        let p = resolve_kubeconfig_path_with(None, Some(PathBuf::from("/home/ashark")), Path::new(":"))
            .unwrap();
        assert_eq!(p, PathBuf::from("/home/ashark/.kube/config"));
    }

    #[test]
    fn missing_home_is_error() {
        let err = resolve_kubeconfig_path_with(None, None, Path::new(":")).unwrap_err();
        assert_eq!(err, KubeconfigError::NoHome);
    }

    #[test]
    fn require_missing_file() {
        let p = PathBuf::from("/no/such/kubeconfig-voss-test");
        match require_kubeconfig(&p) {
            Err(KubeconfigError::Missing(got)) => assert_eq!(got, p),
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn require_existing_file() {
        let dir = std::env::temp_dir().join("voss-kubeconfig-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("config");
        std::fs::write(&f, "apiVersion: v1\nkind: Config\n").unwrap();
        require_kubeconfig(&f).unwrap();
    }
}
