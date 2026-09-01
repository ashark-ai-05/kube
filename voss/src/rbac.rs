//! Map HTTP 403 / Forbidden API errors to RBAC failures with resource and verb.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbacDenial {
    pub verb: String,
    pub resource: String,
    pub message: String,
}

impl std::fmt::Display for RbacDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RBAC denied: cannot {verb} {resource} — {message}",
            verb = self.verb,
            resource = self.resource,
            message = self.message
        )
    }
}

/// Kubernetes 403 message shape:
/// `User "x" cannot list resource "pods" in API group "" in the namespace "ns"`
pub fn parse_forbidden_message(message: &str) -> Option<(String, String)> {
    let verb = capture_after(message, " cannot ", " resource ")?;
    let resource = capture_quoted_after(message, "resource ")?;
    Some((verb, resource))
}

fn capture_after<'a>(s: &'a str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let rest = &s[i..];
    let j = rest.find(end)?;
    let verb = rest[..j].trim();
    if verb.is_empty() {
        None
    } else {
        Some(verb.to_string())
    }
}

fn capture_quoted_after(s: &str, marker: &str) -> Option<String> {
    let i = s.find(marker)? + marker.len();
    let rest = s[i..].trim_start();
    if rest.starts_with('"') {
        let inner = rest.trim_start_matches('"');
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else {
        let end = rest.find(' ').unwrap_or(rest.len());
        Some(rest[..end].trim_end_matches('.').to_string())
    }
}

pub fn rbac_from_status(code: u16, reason: &str, message: &str) -> Option<RbacDenial> {
    let forbidden = code == 403 || reason.eq_ignore_ascii_case("Forbidden");
    if !forbidden {
        return None;
    }
    let (verb, resource) = parse_forbidden_message(message)
        .unwrap_or_else(|| ("unknown".into(), "unknown".into()));
    Some(RbacDenial {
        verb,
        resource,
        message: message.to_string(),
    })
}

/// Format any error: 403 becomes an RBAC sentence; others pass through.
pub fn map_error_text(code: Option<u16>, reason: &str, message: &str) -> String {
    if let Some(code) = code {
        if let Some(rbac) = rbac_from_status(code, reason, message) {
            return rbac.to_string();
        }
    }
    if !reason.is_empty() {
        format!("{reason}: {message}")
    } else {
        message.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_list_pods() {
        let msg = r#"pods is forbidden: User "u" cannot list resource "pods" in API group "" in the namespace "default""#;
        let (verb, resource) = parse_forbidden_message(msg).unwrap();
        assert_eq!(verb, "list");
        assert_eq!(resource, "pods");
    }

    #[test]
    fn extracts_get_secrets() {
        let msg = r#"User "bob" cannot get resource "secrets" in API group "" in the namespace "kube-system""#;
        let (verb, resource) = parse_forbidden_message(msg).unwrap();
        assert_eq!(verb, "get");
        assert_eq!(resource, "secrets");
    }

    #[test]
    fn extracts_watch_events() {
        let msg = r#"User "x" cannot watch resource "events" in API group "" at the cluster scope"#;
        let (verb, resource) = parse_forbidden_message(msg).unwrap();
        assert_eq!(verb, "watch");
        assert_eq!(resource, "events");
    }

    #[test]
    fn status_403_maps_to_rbac() {
        let d = rbac_from_status(
            403,
            "Forbidden",
            r#"User "u" cannot create resource "pods/exec" in API group """#,
        )
        .unwrap();
        assert_eq!(d.verb, "create");
        assert_eq!(d.resource, "pods/exec");
        assert!(d.to_string().contains("RBAC denied"));
        assert!(d.to_string().contains("create"));
        assert!(d.to_string().contains("pods/exec"));
    }

    #[test]
    fn status_500_is_not_rbac() {
        assert!(rbac_from_status(500, "InternalError", "etcd timeout").is_none());
    }

    #[test]
    fn map_error_text_prefers_rbac() {
        let t = map_error_text(
            Some(403),
            "Forbidden",
            r#"User "u" cannot list resource "namespaces" in API group """#,
        );
        assert!(t.starts_with("RBAC denied"));
        assert!(t.contains("list"));
        assert!(t.contains("namespaces"));
    }
}
