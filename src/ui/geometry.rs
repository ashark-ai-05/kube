//! Shared measurement helpers for compound widgets that ratatui does not
//! expose post-layout geometry for.
//!
//! `Table` column offsets (Plan 2) and now `Tabs` tab boundaries: neither
//! widget's public API returns where it actually drew its sub-regions, so
//! any caller that needs to hit-test them has to replicate the widget's own
//! layout loop. Centralising that replication here means it happens once,
//! not once per view that grows tabs.

use ratatui::layout::{Constraint, Layout, Rect};
use unicode_width::UnicodeWidthStr;

/// Per-column x-offsets for a ratatui `Table`, reproducing its own two-stage
/// layout rather than the single `Layout::horizontal(widths).split(area)` a
/// caller might reach for first.
///
/// `Table`'s real layout (`ratatui-widgets-0.3.2/src/table.rs`,
/// `get_column_widths`; verified in
/// `docs/superpowers/plan2-api-reference.md` section D14) reserves a
/// selection-symbol column FIRST — `Layout::horizontal([Length(selection_width),
/// Fill(0)])` — and only THEN splits what remains by the caller's widths with
/// `column_spacing`. A naive single-stage split only happens to agree with
/// this when `selection_width` is 0 (no highlight symbol, or
/// `HighlightSpacing::WhenSelected` with nothing currently selected); the
/// moment a row IS selected under the common `WhenSelected` default, or a
/// highlight symbol is set at all, the selection column silently eats width
/// and every column header click lands one column short of where `Table`
/// actually drew it.
pub fn column_offsets(
    widths: &[Constraint],
    area: Rect,
    column_spacing: u16,
    selection_width: u16,
) -> Vec<Rect> {
    // TODO(Task 6 RED): naive single-stage split, ignores `selection_width`
    // entirely — the exact bug this function exists to fix.
    let _ = selection_width;
    if area.width == 0 || area.height == 0 || widths.is_empty() {
        return Vec::new();
    }
    Layout::horizontal(widths.to_vec())
        .spacing(column_spacing)
        .split(area)
        .to_vec()
}

/// Per-tab clickable rects for a manually hit-tested `Tabs`-like row.
///
/// Ratatui's `Tabs` widget (`ratatui-widgets-0.3.2/src/tabs.rs`) exposes no
/// way to ask "where did tab N end up" — its public surface is `new`,
/// `titles`, `select`, `style`, `divider`, and padding, nothing that returns
/// geometry. This replicates its left-to-right layout loop measuring each
/// label with `unicode_width::UnicodeWidthStr` (not byte length or
/// `.chars().count()`, both of which under-measure wide characters and would
/// make every tab after one mis-place its hit zone).
///
/// Tabs that do not fit within `area` are dropped rather than returned with
/// a `Rect` extending past it: `x` only ever increases as tabs are laid out,
/// so once one tab overflows, every tab after it would too.
pub fn tab_spans(labels: &[&str], area: Rect, divider_width: u16) -> Vec<Rect> {
    let mut rects = Vec::new();
    if area.width == 0 || area.height == 0 {
        return rects;
    }

    let right_edge = area.x.saturating_add(area.width);
    let mut x = area.x;

    for label in labels {
        let width = UnicodeWidthStr::width(*label) as u16;
        let end = x.saturating_add(width);
        if end > right_edge {
            // `x` only grows, so every label after this one would overflow
            // too — drop the rest rather than emitting rects past the area.
            break;
        }
        rects.push(Rect {
            x,
            y: area.y,
            width,
            height: 1,
        });
        x = end.saturating_add(divider_width);
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn tab_rects_tile_left_to_right_without_overlap() {
        let rects = tab_spans(&["Overview", "YAML", "Events"], rect(0, 0, 60, 1), 3);
        assert_eq!(rects.len(), 3);
        for pair in rects.windows(2) {
            assert!(
                pair[0].x + pair[0].width <= pair[1].x,
                "tabs overlap: {pair:?}"
            );
        }
    }

    #[test]
    fn tab_widths_account_for_wide_characters() {
        // A CJK label is two cells per character; measuring in chars would
        // make every tab after it clickable at the wrong offset.
        let narrow = tab_spans(&["ab", "cd"], rect(0, 0, 40, 1), 1);
        let wide = tab_spans(&["日本", "cd"], rect(0, 0, 40, 1), 1);
        assert!(wide[1].x > narrow[1].x, "wide label did not widen its tab");
    }

    #[test]
    fn tabs_that_do_not_fit_are_dropped_rather_than_drawn_off_screen() {
        let rects = tab_spans(&["Overview", "YAML", "Events"], rect(0, 0, 10, 1), 3);
        for r in &rects {
            assert!(r.x + r.width <= 10, "tab {r:?} extends past the area");
        }
    }

    #[test]
    fn a_zero_width_area_yields_no_tabs_rather_than_panicking() {
        let rects = tab_spans(&["Overview", "YAML"], rect(0, 0, 0, 1), 1);
        assert!(rects.is_empty());
    }

    #[test]
    fn a_zero_height_area_yields_no_tabs_rather_than_panicking() {
        let rects = tab_spans(&["Overview", "YAML"], rect(0, 0, 40, 0), 1);
        assert!(rects.is_empty());
    }

    #[test]
    fn an_empty_label_list_yields_no_tabs() {
        let rects: Vec<Rect> = tab_spans(&[], rect(0, 0, 40, 1), 1);
        assert!(rects.is_empty());
    }

    // --- column_offsets ---

    #[test]
    fn column_offsets_account_for_the_selection_column_and_spacing() {
        // Plan 2's reference established that Layout::horizontal alone is only
        // correct when the selection symbol is zero-width. A naive split makes
        // every column header click land on the wrong column.
        let widths = [
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Fill(1),
        ];
        let plain = column_offsets(&widths, rect(0, 0, 60, 1), 1, 0);
        let with_sel = column_offsets(&widths, rect(0, 0, 60, 1), 1, 2);
        assert!(with_sel[0].x > plain[0].x, "selection column not reserved");
        for pair in with_sel.windows(2) {
            assert!(
                pair[0].x + pair[0].width <= pair[1].x,
                "columns overlap: {pair:?}"
            );
        }
    }

    #[test]
    fn a_zero_width_area_yields_no_column_offsets_rather_than_panicking() {
        let widths = [Constraint::Length(10), Constraint::Fill(1)];
        assert!(column_offsets(&widths, rect(0, 0, 0, 1), 1, 0).is_empty());
    }

    #[test]
    fn a_zero_height_area_yields_no_column_offsets_rather_than_panicking() {
        let widths = [Constraint::Length(10), Constraint::Fill(1)];
        assert!(column_offsets(&widths, rect(0, 0, 40, 0), 1, 0).is_empty());
    }

    #[test]
    fn an_empty_widths_list_yields_no_column_offsets() {
        assert!(column_offsets(&[], rect(0, 0, 40, 1), 1, 0).is_empty());
    }
}
