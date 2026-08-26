use ratatui::layout::Rect;

/// What a screen region means when clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    TableRow(usize),
    ColumnHeader(usize),
    StatusBar,
    Background,
    Ribbon,
    PickerRow(usize),
    /// A row in the sidebar's flattened kind tree — see `ui::tree::flatten`.
    /// The index is into that flattened list, exactly like `TableRow` is an
    /// index into the object list: absolute, not screen-relative, so a
    /// scrolled sidebar still resolves clicks to the right row.
    TreeRow(usize),
}

/// Maps screen coordinates back to meaning.
///
/// Ratatui draws into a buffer and keeps no widget tree, so this registry is
/// rebuilt every frame as widgets draw. Resolution walks in reverse so that
/// higher z-index — and, at equal z, later-drawn — zones win.
pub struct HitRegistry {
    zones: Vec<(Rect, u8, HitTarget)>,
}

impl Default for HitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HitRegistry {
    pub fn new() -> Self {
        Self { zones: Vec::new() }
    }

    /// Drop all zones. Called at the start of every frame.
    pub fn clear(&mut self) {
        self.zones.clear();
    }

    pub fn push(&mut self, area: Rect, z: u8, target: HitTarget) {
        self.zones.push((area, z, target));
    }

    pub fn hit(&self, col: u16, row: u16) -> Option<&HitTarget> {
        self.zones
            .iter()
            .filter(|(area, _, _)| {
                area.width > 0
                    && area.height > 0
                    && col >= area.x
                    && col < area.x.saturating_add(area.width)
                    && row >= area.y
                    && row < area.y.saturating_add(area.height)
            })
            .max_by_key(|(_, z, _)| *z)
            .map(|(_, _, target)| target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn a_click_inside_a_zone_hits_it() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        assert_eq!(r.hit(5, 2), Some(&HitTarget::TableRow(3)));
    }

    #[test]
    fn a_click_outside_every_zone_misses() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        assert_eq!(r.hit(50, 50), None);
    }

    #[test]
    fn zone_boundaries_are_inclusive_at_the_start_exclusive_at_the_end() {
        let mut r = HitRegistry::new();
        r.push(rect(2, 2, 3, 3), 0, HitTarget::TableRow(0));
        assert!(r.hit(2, 2).is_some(), "top-left corner is inside");
        assert!(r.hit(4, 4).is_some(), "bottom-right cell is inside");
        assert!(r.hit(5, 4).is_none(), "one past the right edge is outside");
        assert!(r.hit(4, 5).is_none(), "one past the bottom edge is outside");
        assert!(r.hit(1, 2).is_none(), "one before the left edge is outside");
    }

    #[test]
    fn a_higher_z_zone_wins_when_zones_overlap() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 20, 10), 0, HitTarget::Background);
        r.push(rect(5, 5, 5, 3), 1, HitTarget::TableRow(7));
        assert_eq!(
            r.hit(6, 6),
            Some(&HitTarget::TableRow(7)),
            "an overlay must capture clicks over the pane beneath it"
        );
    }

    #[test]
    fn later_registration_wins_at_equal_z() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 10), 0, HitTarget::TableRow(1));
        r.push(rect(0, 0, 10, 10), 0, HitTarget::TableRow(2));
        assert_eq!(
            r.hit(1, 1),
            Some(&HitTarget::TableRow(2)),
            "drawn later means drawn on top"
        );
    }

    #[test]
    fn clear_empties_the_registry_for_the_next_frame() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        r.clear();
        assert_eq!(
            r.hit(5, 2),
            None,
            "stale zones must not survive a re-render"
        );
    }

    #[test]
    fn zero_sized_zones_never_hit() {
        let mut r = HitRegistry::new();
        r.push(rect(3, 3, 0, 0), 0, HitTarget::TableRow(1));
        assert_eq!(r.hit(3, 3), None);
    }

    #[test]
    fn a_zone_near_the_coordinate_limit_does_not_overflow() {
        // Rect's fields are public, so a zone can be built by struct literal
        // without Rect::new()'s clamping. Unchecked addition panics in debug
        // and — worse — wraps silently in release, mis-targeting the click.
        let mut r = HitRegistry::new();
        r.push(rect(u16::MAX - 5, 0, 100, 1), 0, HitTarget::TableRow(0));
        assert_eq!(
            r.hit(u16::MAX - 3, 0),
            Some(&HitTarget::TableRow(0)),
            "inside the zone"
        );
        assert_eq!(r.hit(0, 0), None, "far outside must not wrap into a hit");
    }
}
