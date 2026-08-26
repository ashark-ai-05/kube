//! Sidebar tree model for kinds grouped by API group.
//!
//! The tree flattens to a single list of rows for rendering and hit-testing,
//! ensuring both layers see exactly the same structure without duplication or
//! drift.

use kube::api::GroupVersionKind;

/// A single Kubernetes kind to display in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeKind {
    pub gvk: GroupVersionKind,
    pub label: String,
    pub count: Option<usize>,
    pub availability: crate::store::multi::KindAvailability,
}

/// A group of kinds (same API group) that can be collapsed/expanded.
#[derive(Debug, Clone)]
pub struct TreeGroup {
    pub label: String,
    pub expanded: bool,
    pub kinds: Vec<TreeKind>,
}

/// The complete tree of kinds, indexed by selected row.
#[derive(Debug, Clone)]
pub struct KindTree {
    pub groups: Vec<TreeGroup>,
    pub selected: usize,
}

/// A row in the flattened tree — either a group header or a kind.
#[derive(Debug)]
pub enum TreeRow<'a> {
    Group {
        index: usize,
        group: &'a TreeGroup,
    },
    Kind {
        group_index: usize,
        kind: &'a TreeKind,
    },
}

/// Flatten the tree into a list of rows for rendering and hit-testing.
///
/// Each expanded group contributes one row for itself plus one per kind.
/// Each collapsed group contributes only one row for itself.
pub fn flatten(_tree: &KindTree) -> Vec<TreeRow<'_>> {
    // TODO: implement
    Vec::new()
}

impl KindTree {
    /// Toggle the expanded state of a group at the given flattened row index.
    ///
    /// If the row is a kind (not a group), does nothing.
    /// If the row index is out of bounds, does nothing.
    pub fn toggle(&mut self, row: usize) {
        // TODO: implement
        let _ = row;
    }

    /// Get the kind at the current selection, or None if the selection is a group.
    pub fn selected_kind(&self) -> Option<&TreeKind> {
        // TODO: implement
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: construct a KindTree from simplified test data.
    ///
    /// Each group is specified as (label, expanded, &[kind_labels]).
    fn tree(specs: &[(&str, bool, &[&str])]) -> KindTree {
        let groups = specs
            .iter()
            .map(|(label, expanded, kinds)| TreeGroup {
                label: label.to_string(),
                expanded: *expanded,
                kinds: kinds
                    .iter()
                    .map(|k| TreeKind {
                        gvk: GroupVersionKind::gvk(*label, "v1", k),
                        label: k.to_string(),
                        count: None,
                        availability: crate::store::multi::KindAvailability::Watching,
                    })
                    .collect(),
            })
            .collect();

        KindTree {
            groups,
            selected: 0,
        }
    }

    #[test]
    fn a_collapsed_group_contributes_only_its_own_row() {
        let t = tree(&[("core", false, &["Pod", "Service"])]);
        assert_eq!(flatten(&t).len(), 1);
    }

    #[test]
    fn an_expanded_group_contributes_its_kinds_in_order() {
        let t = tree(&[("core", true, &["Pod", "Service"])]);
        let rows = flatten(&t);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], TreeRow::Group { .. }));
        assert!(matches!(&rows[1], TreeRow::Kind { kind, .. } if kind.label == "Pod"));
        assert!(matches!(&rows[2], TreeRow::Kind { kind, .. } if kind.label == "Service"));
    }

    #[test]
    fn flattening_interleaves_groups_correctly() {
        // Two expanded groups and one collapsed between them — the classic
        // off-by-one shape. Row indices must map back to the right kinds.
        let t = tree(&[
            ("core", true, &["Pod"]),
            ("apps", false, &["Deployment"]),
            ("batch", true, &["Job", "CronJob"]),
        ]);
        let rows = flatten(&t);
        assert_eq!(rows.len(), 6);
        assert!(matches!(&rows[3], TreeRow::Group { group, .. } if group.label == "batch"));
        assert!(matches!(&rows[5], TreeRow::Kind { kind, .. } if kind.label == "CronJob"));
    }

    #[test]
    fn toggling_a_group_row_changes_only_that_group() {
        let mut t = tree(&[("core", true, &["Pod"]), ("apps", true, &["Deployment"])]);
        t.toggle(0);
        assert!(!t.groups[0].expanded);
        assert!(t.groups[1].expanded, "collapsing one group must not collapse another");
    }

    #[test]
    fn toggling_a_kind_row_does_nothing_rather_than_collapsing_its_parent() {
        let mut t = tree(&[("core", true, &["Pod"])]);
        t.toggle(1);
        assert!(t.groups[0].expanded);
    }

    #[test]
    fn toggling_a_row_past_the_end_is_a_no_op() {
        let mut t = tree(&[("core", true, &["Pod"])]);
        t.toggle(99);
        assert_eq!(flatten(&t).len(), 2);
    }

    #[test]
    fn the_selected_kind_follows_the_flattened_row() {
        let mut t = tree(&[("core", true, &["Pod", "Service"])]);
        t.selected = 2;
        assert_eq!(t.selected_kind().map(|k| k.label.as_str()), Some("Service"));
        t.selected = 0;
        assert_eq!(t.selected_kind(), None, "a group row selects no kind");
    }
}
