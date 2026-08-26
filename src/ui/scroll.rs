//! Viewport scrolling shared by every scrollable list.
//!
//! Both functions are pure and geometry-agnostic: they speak in *rows that
//! fit*, leaving each view to work out how many of its own lines the chrome
//! takes (the table spends three on borders and its header, the picker one on
//! its filter line). They live here rather than in `views::table` so the
//! picker can reuse them without depending on the table — the picker shipped
//! without scrolling at all, which left every cluster past the first
//! screenful undrawn, unclickable and unreachable on a kubeconfig with 20+
//! contexts.

/// The half-open range of item indices that can actually be drawn.
pub fn window(offset: usize, rows: usize, total: usize) -> std::ops::Range<usize> {
    let start = offset.min(total);
    let end = start.saturating_add(rows).min(total);
    start..end
}

/// Offset that keeps `selected` visible, moving as little as possible.
///
/// Views own scrolling outright rather than delegating it to ratatui. The
/// straightforward design — hand `Table` a windowed `Vec<Row>` while leaving
/// `TableState::selected` as an absolute object index — was tried first and
/// found empirically unsafe: `ratatui::widgets::Table::render` clamps
/// `state.selected` to `rows.len() - 1` whenever `selected >= rows.len()`, so
/// a windowed row list silently rewrites the real selection (verified with a
/// probe: `selected = 30` against a 7-row list came back as `selected =
/// Some(6)` after render). No choice of offset avoids that clamp, because it
/// fires purely off `rows.len()`. Computing the offset ourselves and handing
/// ratatui a window-relative selection sidesteps it entirely: ratatui always
/// sees exactly the rows it draws, so its own clamp is a no-op and it never
/// scrolls on its own. See task-4-report.md for the full empirical finding.
pub fn scroll_offset(selected: usize, current_offset: usize, rows: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    if selected < current_offset {
        selected
    } else if selected >= current_offset + rows {
        selected + 1 - rows
    } else {
        current_offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_covers_only_the_rows_that_fit() {
        assert_eq!(window(0, 7, 100), 0..7);
    }

    #[test]
    fn a_window_follows_the_offset() {
        assert_eq!(window(24, 7, 100), 24..31);
    }

    #[test]
    fn a_window_is_clamped_to_the_item_count() {
        assert_eq!(window(0, 7, 3), 0..3);
        assert_eq!(window(98, 7, 100), 98..100);
    }

    #[test]
    fn an_offset_past_the_end_yields_an_empty_window_rather_than_panicking() {
        assert!(window(500, 7, 100).is_empty());
    }

    #[test]
    fn a_zero_row_window_is_empty() {
        assert!(window(0, 0, 100).is_empty());
    }
}
