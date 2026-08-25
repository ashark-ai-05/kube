pub mod event;
pub mod input;
pub mod session;

use crate::ui::views::picker::Picker;

/// A modal overlay drawn on top of the table. At most one is ever open —
/// opening one replaces whatever was open before.
///
/// `action_for` only needs to know THAT a picker is open (`is_open`), not
/// which: deciding what a keystroke means while a picker has focus (Esc
/// closes it, everything else is filter text) doesn't depend on whether
/// it's clusters or namespaces. What DOES depend on which picker is open is
/// what a confirmed selection actually does — connect to a cluster, or
/// restart the watch in a different namespace — and that dispatch belongs
/// to the caller (`main.rs`), the only place that knows how to do either.
#[derive(Default)]
pub enum Overlay {
    #[default]
    None,
    ClusterPicker(Picker),
    NamespacePicker(Picker),
}

impl Overlay {
    /// Whether a picker currently has input focus.
    pub fn is_open(&self) -> bool {
        !matches!(self, Overlay::None)
    }

    /// The picker underneath, regardless of which kind — for code that reads
    /// picker state (filter, selection, items) without caring what a
    /// confirmed choice will do.
    pub fn picker(&self) -> Option<&Picker> {
        match self {
            Overlay::None => None,
            Overlay::ClusterPicker(p) | Overlay::NamespacePicker(p) => Some(p),
        }
    }

    /// As `picker`, but mutable — for routing filter keystrokes and
    /// navigation into whichever picker is open.
    pub fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self {
            Overlay::None => None,
            Overlay::ClusterPicker(p) | Overlay::NamespacePicker(p) => Some(p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::views::picker::{Picker, PickerItem};

    fn picker() -> Picker {
        Picker {
            title: "T".into(),
            items: vec![PickerItem {
                label: "a".into(),
                detail: String::new(),
                accent: None,
            }],
            filter: String::new(),
            selected: 0,
        }
    }

    #[test]
    fn no_overlay_is_not_open() {
        assert!(!Overlay::None.is_open());
    }

    #[test]
    fn a_cluster_picker_overlay_is_open() {
        assert!(Overlay::ClusterPicker(picker()).is_open());
    }

    #[test]
    fn a_namespace_picker_overlay_is_open() {
        assert!(Overlay::NamespacePicker(picker()).is_open());
    }

    #[test]
    fn picker_mut_reaches_into_either_variant() {
        let mut o = Overlay::NamespacePicker(picker());
        o.picker_mut()
            .expect("a namespace picker overlay has a picker")
            .filter
            .push('x');
        assert_eq!(o.picker().expect("still open").filter, "x");
    }

    #[test]
    fn no_overlay_has_no_picker() {
        assert!(Overlay::None.picker().is_none());
        assert!(Overlay::None.picker_mut().is_none());
    }

    #[test]
    fn overlay_defaults_to_none() {
        assert!(!Overlay::default().is_open());
    }
}
