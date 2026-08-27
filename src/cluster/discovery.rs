//! Discovering watchable resource kinds in a cluster.
//!
//! A "watchable" kind is one that supports both `list` and `watch` operations.
//! Without `list`, the initial population is empty; without `watch`, counts never
//! update. Either alone makes the sidebar show incorrect information.

use anyhow::Result;
use kube::Client;
use kube::api::ApiResource;
use kube::discovery::{ApiCapabilities, Discovery, Scope, verbs};

/// A resource kind that can be watched for changes in the sidebar.
#[derive(Debug, Clone)]
pub struct KindInfo {
    /// The group, version, and kind as a single composite.
    pub gvk: kube::api::GroupVersionKind,
    /// The resource's API details (group, version, plural name, etc).
    pub resource: ApiResource,
    /// Whether this kind is scoped to a namespace (true) or cluster-wide (false).
    pub namespaced: bool,
    /// A human-readable label for the group (e.g. "core" for the empty group).
    pub group_label: String,
}

/// A human-readable label for a resource group.
///
/// The core Kubernetes API group has an empty name — displaying a blank sidebar
/// heading would be worse than useless. This function maps `""` to `"core"`.
pub fn group_label_for(group: &str) -> String {
    if group.is_empty() {
        "core".to_string()
    } else {
        group.to_string()
    }
}

/// Whether a resource kind can be watched and listed in the sidebar.
///
/// A kind needs both `list` and `watch` to appear:
///
/// - Without `list`, the initial population is empty.
/// - Without `watch`, the count never updates.
///
/// Either alone makes the sidebar show incorrect information.
pub fn is_browsable(caps: &ApiCapabilities) -> bool {
    caps.supports_operation(verbs::LIST) && caps.supports_operation(verbs::WATCH)
}

/// Sort kinds by group label, then by kind name within each group.
///
/// `Discovery::groups()` iterates an unordered HashMap, so groups appear in
/// arbitrary order. Without explicit sorting, the sidebar would reorder itself
/// between application restarts. This function enforces a stable order.
pub fn sort_kinds(kinds: &mut [KindInfo]) {
    kinds.sort_by(|a, b| {
        a.group_label
            .cmp(&b.group_label)
            .then_with(|| a.gvk.kind.cmp(&b.gvk.kind))
    });
}

/// Discover all watchable resource kinds in a cluster.
///
/// Iterates every API group and each group's recommended resource version, filters
/// to kinds that support both `list` and `watch`, and returns them sorted by
/// group and kind name for consistent sidebar ordering.
///
/// This is an I/O-bound async operation — never call this from a render path.
pub async fn discover_kinds(client: &Client) -> Result<Vec<KindInfo>> {
    let discovery = Discovery::new(client.clone()).run().await?;
    let mut kinds = Vec::new();

    for group in discovery.groups() {
        let group_name = group.name().to_string();
        let group_label = group_label_for(&group_name);

        for (api_resource, caps) in group.recommended_resources() {
            if is_browsable(&caps) {
                let gvk = kube::api::GroupVersionKind::gvk(
                    &group_name,
                    &api_resource.version,
                    &api_resource.kind,
                );
                let namespaced = matches!(caps.scope, Scope::Namespaced);

                kinds.push(KindInfo {
                    gvk,
                    resource: api_resource,
                    namespaced,
                    group_label: group_label.clone(),
                });
            }
        }
    }

    sort_kinds(&mut kinds);
    Ok(kinds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct an ApiCapabilities from a list of operation names.
    ///
    /// This helper is used by tests that check `is_browsable` without needing
    /// a live Kubernetes cluster or a full discovery response.
    fn caps(operations: &[&str]) -> ApiCapabilities {
        ApiCapabilities {
            scope: Scope::Namespaced,
            subresources: vec![],
            operations: operations.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn the_core_group_gets_a_readable_label() {
        // The core group's name is the empty string; showing a blank sidebar
        // heading would be worse than useless.
        assert_eq!(group_label_for(""), "core");
        assert_eq!(group_label_for("apps"), "apps");
        assert_eq!(group_label_for("networking.k8s.io"), "networking.k8s.io");
    }

    #[test]
    fn a_kind_needs_both_list_and_watch_to_be_browsable() {
        assert!(is_browsable(&caps(&["list", "watch", "get"])));
        assert!(
            !is_browsable(&caps(&["list", "get"])),
            "no watch: counts would never update"
        );
        assert!(
            !is_browsable(&caps(&["watch"])),
            "no list: the initial population would be empty"
        );
        assert!(!is_browsable(&caps(&[])));
    }

    #[test]
    fn subresources_and_write_only_verbs_do_not_make_a_kind_browsable() {
        assert!(!is_browsable(&caps(&["create", "delete", "patch"])));
    }

    /// Construct a KindInfo for testing sorting.
    ///
    /// Uses a dummy resource to isolate the test of sort logic from discovery.
    fn kind_info(group_label: &str, kind: &str) -> KindInfo {
        KindInfo {
            gvk: kube::api::GroupVersionKind::gvk(group_label, "v1", kind),
            resource: ApiResource {
                group: group_label.to_string(),
                api_version: "v1".to_string(),
                kind: kind.to_string(),
                version: "v1".to_string(),
                plural: kind.to_lowercase(),
            },
            namespaced: true,
            group_label: group_label.to_string(),
        }
    }

    #[test]
    fn kinds_come_back_in_a_stable_order_regardless_of_discovery_order() {
        // Discovery::groups() iterates a HashMap, so input order is arbitrary
        // and varies between runs. Without an explicit sort the sidebar would
        // reorder itself every restart.
        //
        // This fixture is carefully chosen to distinguish correct sorting from
        // common mistakes:
        // - Input is deliberately neither fully sorted nor reverse sorted
        // - Groups interleave: core kind appears before all apps kinds
        // - Sorting by kind only: Daemon, Pod, Service, Web (wrong!)
        // - Sorting by group then kind: apps/Daemon, apps/Web, core/Pod, core/Service (right!)
        // These differ, so a wrong implementation fails.
        let mut kinds = vec![
            kind_info("apps", "Web"),
            kind_info("core", "Pod"),
            kind_info("apps", "Daemon"),
            kind_info("core", "Service"),
        ];
        sort_kinds(&mut kinds);

        let got: Vec<String> = kinds
            .iter()
            .map(|k| format!("{}/{}", k.group_label, k.gvk.kind))
            .collect();

        // Expected: sorted by group_label first (alphabetical), then by kind
        // within each group. "apps" comes before "core", and within each group
        // kinds are alphabetical.
        assert_eq!(
            got,
            vec!["apps/Daemon", "apps/Web", "core/Pod", "core/Service"]
        );
    }
}
