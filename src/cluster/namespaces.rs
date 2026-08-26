//! Listing cluster namespaces via the API, for the namespace picker.
//!
//! Kept apart from `store::watch`: this is a one-shot GET for the picker to
//! render when it opens, not a live watch. It reuses
//! `store::rbac::classify_kube_error` for "is this 403 permanent" rather than
//! re-deriving that check — a second copy of "unwrap into
//! `kube::Error::Api(Status)` and look at the code" is exactly the kind of
//! duplicated classification earlier reviews of this project flagged.

use crate::store::rbac::{WatchFailure, classify_kube_error};
use k8s_openapi::api::core::v1::Namespace;
use kube::api::{ListParams, ObjectList};
use kube::{Api, Client};

/// Why `list_namespaces` could not return a list.
///
/// Carries the error's *display text*, not the `kube::Error` itself, so this
/// type stays `Clone`/`PartialEq`/`Eq` and can travel through `AppEvent`
/// (which derives `Clone`) unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceListError {
    /// 403 — this identity cannot list namespaces cluster-wide. Routine on a
    /// corporate cluster whose RBAC is scoped to individual namespaces —
    /// exactly the cluster where the picker is needed most, since the same
    /// RBAC usually forbids listing pods at cluster scope too.
    Forbidden(String),
    /// Anything else — a blip, a dead connection, a 500.
    Other(String),
}

impl NamespaceListError {
    /// A short line for the picker: what happened, and the one thing that
    /// still works regardless of which of these this is — typing a name.
    pub fn explanation(&self) -> String {
        let cause = match self {
            NamespaceListError::Forbidden(_) => "forbidden".to_string(),
            NamespaceListError::Other(detail) => detail.clone(),
        };
        format!("namespaces could not be listed ({cause}) — type a name and press Enter")
    }
}

/// Fetch every namespace's name from the API, sorted alphabetically.
///
/// Async and I/O-bound — never call this from a render path. `main.rs` spawns
/// it on a task when the namespace picker opens and delivers the result back
/// through the `AppEvent` channel, exactly as `store::watch::spawn_watch`
/// does for a live watch.
pub async fn list_namespaces(client: &Client) -> Result<Vec<String>, NamespaceListError> {
    let api: Api<Namespace> = Api::all(client.clone());
    classify_list_result(api.list(&ListParams::default()).await)
}

/// The classification `list_namespaces` runs its result through, pulled out
/// so it can be tested without a cluster: an `ObjectList` is a plain struct
/// constructible by hand, and a `kube::Error::Api` is built the same way
/// `store::rbac`'s own tests build one.
fn classify_list_result(
    result: kube::Result<ObjectList<Namespace>>,
) -> Result<Vec<String>, NamespaceListError> {
    match result {
        Ok(list) => {
            let mut names: Vec<String> = list
                .items
                .into_iter()
                .filter_map(|ns| ns.metadata.name)
                .collect();
            names.sort();
            Ok(names)
        }
        Err(e) => match classify_kube_error(&e) {
            WatchFailure::Forbidden { detail } => Err(NamespaceListError::Forbidden(detail)),
            _ => Err(NamespaceListError::Other(e.to_string())),
        },
    }
}

/// Whether `name` could be a valid Kubernetes namespace name: a DNS-1123
/// label — lowercase alphanumerics and `-`, 1-63 characters, never starting
/// or ending with `-`.
///
/// Checked before the type-to-enter path in the namespace picker acts on
/// anything typed, so a name that could never be valid is rejected locally
/// rather than spent on a request that is certain to fail with an apiserver
/// validation error.
pub fn is_valid_namespace_name(name: &str) -> bool {
    let len = name.chars().count();
    if len == 0 || len > 63 {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::core::Status;

    fn ns(name: &str) -> Namespace {
        Namespace {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn list_of(names: &[&str]) -> kube::Result<ObjectList<Namespace>> {
        Ok(ObjectList {
            types: Default::default(),
            metadata: Default::default(),
            items: names.iter().map(|n| ns(n)).collect(),
        })
    }

    fn api_error(code: u16, reason: &str, message: &str) -> kube::Error {
        kube::Error::Api(Box::new(Status {
            code,
            message: message.to_string(),
            reason: reason.to_string(),
            ..Default::default()
        }))
    }

    #[test]
    fn a_successful_list_is_sorted_alphabetically() {
        // Out of alphabetical order on the wire, as a real apiserver response
        // is (insertion/creation order) — sorting is what's under test.
        let result = classify_list_result(list_of(&["zeta", "alpha", "mu"]));
        assert_eq!(
            result,
            Ok(vec![
                "alpha".to_string(),
                "mu".to_string(),
                "zeta".to_string()
            ])
        );
    }

    #[test]
    fn a_forbidden_list_is_distinguished_from_any_other_failure() {
        let result = classify_list_result(Err(api_error(
            403,
            "Forbidden",
            "namespaces is forbidden: User \"u\" cannot list resource \"namespaces\" at the cluster scope",
        )));
        match result {
            Err(NamespaceListError::Forbidden(detail)) => {
                assert!(detail.contains("namespaces"), "got {detail}");
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    #[test]
    fn a_500_is_not_mistaken_for_forbidden() {
        let result = classify_list_result(Err(api_error(500, "InternalError", "etcd unavailable")));
        assert!(matches!(result, Err(NamespaceListError::Other(_))));
    }

    // --- is_valid_namespace_name ---

    #[test]
    fn ordinary_namespace_names_are_valid() {
        let long = "a".repeat(63);
        for name in ["default", "kube-system", "a", "prod-eu-1", long.as_str()] {
            assert!(is_valid_namespace_name(name), "{name} should be valid");
        }
    }

    #[test]
    fn an_empty_name_is_invalid() {
        assert!(!is_valid_namespace_name(""));
    }

    #[test]
    fn a_name_over_63_characters_is_invalid() {
        assert!(!is_valid_namespace_name(&"a".repeat(64)));
    }

    #[test]
    fn a_leading_or_trailing_hyphen_is_invalid() {
        assert!(!is_valid_namespace_name("-abc"));
        assert!(!is_valid_namespace_name("abc-"));
    }

    #[test]
    fn uppercase_is_invalid() {
        assert!(!is_valid_namespace_name("Default"));
        assert!(!is_valid_namespace_name("PROD"));
    }

    #[test]
    fn a_slash_or_space_is_invalid() {
        assert!(!is_valid_namespace_name("kube/system"));
        assert!(!is_valid_namespace_name("kube system"));
    }
}
