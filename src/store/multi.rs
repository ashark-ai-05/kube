//! Watching many kinds under an explicit cap.
//!
//! There is no client-side reason to watch lazily — `Client::clone()` is a
//! cheap handle over a shared tower `Buffer`, and the underlying connection
//! pool is unbounded (see `docs/superpowers/plan3-api-reference.md` section
//! B4). So this watches every discovered kind eagerly, with a cap purely as
//! a guard against pathological clusters (hundreds of CRDs) — a
//! configuration value, not a second code path.
//!
//! Two things matter once many kinds are in play:
//!
//! - **Which kinds survive the cap.** If Pods get cut while some operator's
//!   CRD survives, the tool is useless on exactly the clusters where the cap
//!   engages. `prioritise` puts the kinds people actually look at first.
//! - **Silent truncation is a lie.** `kinds_to_watch` reports how many kinds
//!   were skipped so the caller can tell the user, rather than presenting a
//!   40-of-300-kind cluster as if it only had 40 kinds.

use crate::cluster::discovery::KindInfo;
use crate::store::rbac::WatchFailure;

/// The only guard against watching hundreds of kinds on a pathological
/// cluster (e.g. one with hundreds of installed CRDs). Not a throttle for
/// normal clusters — see the module doc comment for why eager watching is
/// safe up to this point.
pub const DEFAULT_MAX_EAGER_WATCHES: usize = 40;

/// What the sidebar should show for a single kind.
///
/// This is deliberately a coarser axis than `store::watch::WatchStatus`:
/// `WatchStatus` (Initialising/Synced/Reconnecting/Failed) describes the
/// live health of a watch that IS running. `KindAvailability` additionally
/// covers the case where a kind was never watched at all (cut by the cap)
/// and, for a kind that was watched, collapses `WatchStatus::Failed` into a
/// user-facing reason via `availability_of` below — without a second
/// per-kind map duplicating `ResourceStore`'s bookkeeping. See this task's
/// report for the full "where does this live" reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindAvailability {
    /// Actively watched, no permanent failure seen (may still be
    /// initialising, synced, or transiently reconnecting).
    Watching,
    /// A watch for this kind hit a permanent (RBAC or gone) failure and
    /// stopped retrying. `reason` is the apiserver's own detail text so the
    /// sidebar can say *why*, not just *that*.
    Unavailable { reason: String },
    /// Discovered but never watched — cut by `kinds_to_watch`'s cap.
    NotWatched,
}

/// Kind names ranked in the order people actually look at them, most
/// important first. Anything not listed here sorts after all of these,
/// keeping its discovery order relative to other unranked kinds.
const PRIORITY: &[&str] = &[
    "Pod",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Service",
    "Ingress",
    "ConfigMap",
    "Secret",
    "Job",
    "CronJob",
    "Node",
    "Namespace",
    "PersistentVolumeClaim",
];

/// Rank of a kind for `prioritise`: its index in `PRIORITY`, or
/// `PRIORITY.len()` (i.e. "after everything ranked") if it isn't listed.
fn rank(kind: &str) -> usize {
    PRIORITY
        .iter()
        .position(|&p| p == kind)
        .unwrap_or(PRIORITY.len())
}

/// Sort kinds so the ones people actually look at (Pods, Deployments, ...)
/// come first. Uses a **stable** sort so kinds that share a rank — which
/// includes every unranked kind, since they all share the same "unranked"
/// rank — keep their discovery order rather than being shuffled.
///
/// This must run before `kinds_to_watch` applies its cap: which kinds
/// survive the cap is only correct if the important ones are first.
///
/// Takes `&mut [KindInfo]` rather than `&mut Vec<KindInfo>` (clippy's
/// `ptr_arg`: a slice is all sorting needs) — callers holding a
/// `Vec<KindInfo>` still call this as `prioritise(&mut kinds)` unchanged,
/// via the usual `&mut Vec<T>` -> `&mut [T]` deref coercion.
pub fn prioritise(kinds: &mut [KindInfo]) {
    // `sort_by_key` is documented stable (unlike `sort_unstable_by_key`),
    // which is what keeps equal-rank (in particular, all-unranked) kinds in
    // their original relative order.
    kinds.sort_by_key(|k| rank(&k.gvk.kind));
}

/// Split `kinds` into those to watch and how many were dropped by the cap.
///
/// Callers should `prioritise` first: this function takes the first `cap`
/// kinds as given, in order, and does not reorder or rank them itself — it
/// is purely the "how many fit" guard, kept separate from "which ones
/// matter most" so each is independently testable.
///
/// Returns the skipped count explicitly rather than swallowing it: a
/// sidebar that silently shows 40 of 300 kinds reads as "this cluster has
/// 40 kinds", and the user has no way to tell the difference from the
/// truth.
pub fn kinds_to_watch(kinds: &[KindInfo], cap: usize) -> (Vec<&KindInfo>, usize) {
    let take = kinds.len().min(cap);
    let watched: Vec<&KindInfo> = kinds.iter().take(take).collect();
    let skipped = kinds.len() - take;
    (watched, skipped)
}

/// Map a classified watch failure onto the per-kind availability the
/// sidebar shows.
///
/// This does not re-derive what counts as forbidden/not-found/retryable —
/// that classification is `rbac::classify`'s job alone, per the
/// two-sources-of-truth rule this project has been burned by before. This
/// function only projects `classify`'s already-computed answer onto the
/// coarser Watching/Unavailable axis: `Forbidden`/`NotFound` are permanent,
/// so the kind is `Unavailable` with the apiserver's own detail; `Retryable`
/// is not a failure of availability at all (the watch is still trying), so
/// it stays `Watching`.
pub fn availability_of(failure: &WatchFailure) -> KindAvailability {
    match failure {
        WatchFailure::Forbidden { detail } | WatchFailure::NotFound { detail } => {
            KindAvailability::Unavailable {
                reason: detail.clone(),
            }
        }
        WatchFailure::Retryable => KindAvailability::Watching,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::{ApiResource, GroupVersionKind};

    /// Build minimal `KindInfo`s for kinds with the given names, all in the
    /// core group. Mirrors `cluster::discovery`'s own test helper so these
    /// tests don't need a live cluster or real discovery output.
    fn make_kinds<S: AsRef<str>>(names: &[S]) -> Vec<KindInfo> {
        names
            .iter()
            .map(|name| {
                let kind = name.as_ref();
                KindInfo {
                    gvk: GroupVersionKind::gvk("", "v1", kind),
                    resource: ApiResource {
                        group: String::new(),
                        api_version: "v1".to_string(),
                        kind: kind.to_string(),
                        version: "v1".to_string(),
                        plural: kind.to_lowercase(),
                    },
                    namespaced: true,
                    group_label: "core".to_string(),
                }
            })
            .collect()
    }

    // --- kinds_to_watch ---

    #[test]
    fn every_kind_is_watched_when_under_the_cap() {
        let kinds = make_kinds(&["Pod", "Deployment", "Service"]);
        let (watched, skipped) = kinds_to_watch(&kinds, 40);
        assert_eq!(watched.len(), 3);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn the_cap_bounds_the_watch_count_and_reports_what_was_dropped() {
        // Silent truncation would read as "this cluster has 40 kinds".
        let kinds = make_kinds(&(0..100).map(|i| format!("Kind{i}")).collect::<Vec<_>>());
        let (watched, skipped) = kinds_to_watch(&kinds, 40);
        assert_eq!(watched.len(), 40);
        assert_eq!(
            skipped, 60,
            "the count dropped must be reportable to the user"
        );
    }

    #[test]
    fn a_cap_of_zero_watches_nothing_rather_than_panicking() {
        let kinds = make_kinds(&["Pod"]);
        let (watched, skipped) = kinds_to_watch(&kinds, 0);
        assert!(watched.is_empty());
        assert_eq!(skipped, 1);
    }

    // --- prioritise ---

    #[test]
    fn the_kinds_people_actually_look_at_survive_the_cap() {
        // With a cap, which kinds get dropped matters. Pods being cut while
        // some operator's CRD survives would make the tool useless on
        // exactly the clusters where the cap engages.
        let mut kinds = make_kinds(&["Widget", "Gizmo", "Pod", "Doodad", "Deployment"]);
        prioritise(&mut kinds);
        let names: Vec<&str> = kinds.iter().map(|k| k.gvk.kind.as_str()).collect();
        assert_eq!(&names[..2], &["Pod", "Deployment"]);
    }

    #[test]
    fn prioritising_is_stable_for_kinds_of_equal_rank() {
        let mut kinds = make_kinds(&["Zebra", "Apple", "Pod"]);
        prioritise(&mut kinds);
        let names: Vec<&str> = kinds.iter().map(|k| k.gvk.kind.as_str()).collect();
        assert_eq!(
            names,
            vec!["Pod", "Zebra", "Apple"],
            "unranked kinds keep discovery order"
        );
    }

    #[test]
    fn prioritising_is_stable_across_many_unranked_kinds() {
        // A fixture of ONE ranked kind plus a long run of unranked kinds is
        // not adversarial enough: to a comparator, an all-equal-key run
        // already satisfies "sorted", so pdqsort's adaptive run-detection
        // can skip real partitioning and happen to leave it untouched —
        // "passing" even under `sort_unstable_by_key`. Genuine partitioning
        // only kicks in when the array isn't already sorted by the
        // comparator, so this fixture interleaves several *different* ranks
        // throughout a long run of same-rank (unranked) kinds, forcing real
        // quicksort partition/swap work across the whole slice.
        let mut names: Vec<String> = Vec::new();
        for (i, ranked) in PRIORITY.iter().enumerate() {
            names.push(ranked.to_string());
            for j in 0..15 {
                names.push(format!("Unranked{i:02}_{j:02}"));
            }
        }
        for j in 0..100 {
            names.push(format!("Tail{j:03}"));
        }

        let expected_unranked_order: Vec<String> = names
            .iter()
            .filter(|n| !PRIORITY.contains(&n.as_str()))
            .cloned()
            .collect();

        let mut kinds = make_kinds(&names);
        prioritise(&mut kinds);

        let got: Vec<&str> = kinds.iter().map(|k| k.gvk.kind.as_str()).collect();
        assert_eq!(
            &got[..PRIORITY.len()],
            PRIORITY,
            "ranked kinds must lead, in rank order"
        );
        assert_eq!(
            &got[PRIORITY.len()..],
            expected_unranked_order
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .as_slice(),
            "unranked kinds (all sharing one rank) must keep their original discovery order"
        );
    }

    // --- availability_of ---

    #[test]
    fn a_forbidden_failure_marks_the_kind_unavailable_with_the_apiservers_detail() {
        let failure = WatchFailure::Forbidden {
            detail: "pods is forbidden".to_string(),
        };
        assert_eq!(
            availability_of(&failure),
            KindAvailability::Unavailable {
                reason: "pods is forbidden".to_string()
            }
        );
    }

    #[test]
    fn a_not_found_failure_also_marks_the_kind_unavailable() {
        let failure = WatchFailure::NotFound {
            detail: "widgets not found".to_string(),
        };
        assert_eq!(
            availability_of(&failure),
            KindAvailability::Unavailable {
                reason: "widgets not found".to_string()
            }
        );
    }

    #[test]
    fn a_retryable_failure_keeps_the_kind_watching_not_unavailable() {
        // Negative case for the two tests above: a same-shaped classify
        // result that is NOT permanent must not collapse to Unavailable. If
        // `availability_of` ever treated every failure as permanent, this
        // fails while the Forbidden/NotFound tests keep passing.
        assert_eq!(
            availability_of(&WatchFailure::Retryable),
            KindAvailability::Watching
        );
    }
}
