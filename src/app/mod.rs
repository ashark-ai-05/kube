pub mod event;
pub mod input;
pub mod session;

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
