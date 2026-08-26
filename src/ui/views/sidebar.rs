//! Sidebar rendering: the flattened kind tree drawn as a scrollable list.
//!
//! Mirrors `views::table` and `views::picker`: the flattened rows
//! (`tree::flatten`) are the single source of what is on screen, scrolling is
//! delegated to `ui::scroll` rather than reimplemented, and hit zones are
//! registered against the exact same window used to draw — never against
//! independently-derived geometry, which is how Plan 1's table shipped a row
//! that could be seen but not clicked.

use crate::store::multi::KindAvailability;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::scroll;
use crate::ui::theme;
use crate::ui::tree::{KindTree, TreeRow, flatten};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Render the sidebar tree and register a clickable zone for every visible row.
///
/// This view owns scrolling exactly as `render_table` and `render_picker` do:
/// it advances `tree.scroll` by the least amount that keeps `tree.selected`
/// on screen, then draws and registers only that window. The row drawn at
/// screen row `y` and the row a click at `y` resolves to are always the same
/// flattened index — both are read from the identical `window` slice below.
pub fn render_sidebar(f: &mut Frame, area: Rect, tree: &mut KindTree, hits: &mut HitRegistry) {
    // The tree reshapes underneath an open sidebar (a group toggles,
    // discovery adds a kind) — clamp before touching `selected` for anything
    // below, the same defensive clamp `render_table`/`render_picker` perform.
    tree.clamp_selected();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .title("Kinds");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // This view owns scrolling exactly as `render_table` does: advance the
    // offset by the least amount that keeps `selected` visible, then derive
    // the visible window from that offset. Rendering and hit-registration
    // below both walk this one `window`, so they cannot drift apart.
    let rows_visible = inner.height as usize;
    tree.scroll = scroll::scroll_offset(tree.selected, tree.scroll, rows_visible);

    let rows = flatten(tree);
    let window = scroll::window(tree.scroll, rows_visible, rows.len());

    for (offset, row) in rows[window.clone()].iter().enumerate() {
        let absolute_row = window.start + offset;
        let y = inner.y.saturating_add(offset as u16);

        let line = match row {
            TreeRow::Group { group, .. } => {
                let marker = if group.expanded { "▾" } else { "▸" };
                Line::from(Span::styled(
                    format!("{marker} {}", group.label),
                    theme::label_style(),
                ))
            }
            TreeRow::Kind { kind, .. } => {
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(kind.label.clone(), theme::text_style()),
                ];
                match &kind.availability {
                    // A watch that hit a permanent failure must say why —
                    // rendering a blank or a "0" here would read as "this
                    // kind is empty" rather than "you can't see this kind",
                    // and on a corporate cluster lacking RBAC on some kinds
                    // is the normal case, not the exception.
                    KindAvailability::Unavailable { reason } => {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(reason.clone(), theme::muted_style()));
                    }
                    KindAvailability::NotWatched => {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled("not watched", theme::muted_style()));
                    }
                    KindAvailability::Watching => {
                        if let Some(count) = kind.count {
                            spans.push(Span::raw("  "));
                            spans.push(Span::styled(count.to_string(), theme::count_style()));
                        }
                    }
                }
                Line::from(spans)
            }
        };

        let row_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };

        // `tree.selected` already drives the scroll offset above; it must
        // also change what is drawn, or moving the selection scrolls the
        // pane while showing the user nothing distinguishable — the same
        // written-but-never-drawn shape that shipped in Plan 2's table
        // (`view.state.select(...)` written, `view.selected` read, every
        // test passed because each one used only one side). Same convention
        // `render_picker` uses for its selected row: a background fill plus
        // bold, chrome family only.
        let row_style = if absolute_row == tree.selected {
            Style::default()
                .bg(theme::DUSK)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        f.render_widget(Paragraph::new(line).style(row_style), row_area);
        hits.push(row_area, 0, HitTarget::TreeRow(absolute_row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::{TreeGroup, TreeKind};
    use kube::api::GroupVersionKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Test helper: construct a `KindTree` from simplified test data, mirroring
    /// `tree::tests::tree` — every kind defaults to `Watching` with no count.
    fn tree(specs: &[(&str, bool, &[&str])]) -> KindTree {
        let groups = specs
            .iter()
            .map(|(label, expanded, kinds)| TreeGroup {
                label: label.to_string(),
                expanded: *expanded,
                kinds: kinds
                    .iter()
                    .map(|k| TreeKind {
                        gvk: GroupVersionKind::gvk(label, "v1", k),
                        label: k.to_string(),
                        count: None,
                        availability: KindAvailability::Watching,
                    })
                    .collect(),
            })
            .collect();

        KindTree {
            groups,
            selected: 0,
            scroll: 0,
        }
    }

    /// A single expanded group holding one kind whose watch failed.
    fn tree_with_unavailable(kind_label: &str, reason: &str) -> KindTree {
        KindTree {
            groups: vec![TreeGroup {
                label: "core".to_string(),
                expanded: true,
                kinds: vec![TreeKind {
                    gvk: GroupVersionKind::gvk("", "v1", kind_label),
                    label: kind_label.to_string(),
                    count: None,
                    availability: KindAvailability::Unavailable {
                        reason: reason.to_string(),
                    },
                }],
            }],
            selected: 0,
            scroll: 0,
        }
    }

    /// One group with `n` kinds, all watched with a count — enough to exceed
    /// any reasonably small pane so scrolling actually engages. A fixture
    /// that fits entirely on screen cannot distinguish a correct scrolled
    /// hit-test from one that silently registers zones by absolute index
    /// while the drawn rows scroll underneath.
    fn many_kinds(n: usize) -> KindTree {
        let kinds = (0..n)
            .map(|i| TreeKind {
                gvk: GroupVersionKind::gvk("apps", "v1", &format!("Kind{i:02}")),
                label: format!("Kind{i:02}"),
                count: Some(i),
                availability: KindAvailability::Watching,
            })
            .collect();

        KindTree {
            groups: vec![TreeGroup {
                label: "apps".to_string(),
                expanded: true,
                kinds,
            }],
            selected: 0,
            scroll: 0,
        }
    }

    /// Like `render_to_string`, but also returns the `Style` painted at each
    /// screen row's label column (x=3: inner.x=1 plus the two-cell indent
    /// shared by group markers and kind labels alike), so a test can tell
    /// whether the selection is visually distinct from the rows around it.
    /// x=3 and the row-to-y mapping below were confirmed against a real
    /// buffer dump (`buf[(3, y)].style()` for every y) rather than assumed —
    /// this project has a history of fixtures that guessed at coordinates
    /// and were vacuous as a result.
    fn render_with_styles(
        tree: &mut KindTree,
        w: u16,
        h: u16,
    ) -> (Vec<String>, Vec<Style>, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_sidebar(f, area, tree, &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let lines: Vec<String> = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let styles: Vec<Style> = (0..h).map(|y| buf[(3, y)].style()).collect();
        (lines, styles, hits)
    }

    fn render_to_string(tree: &mut KindTree, w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_sidebar(f, area, tree, &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    #[test]
    fn each_visible_sidebar_row_registers_a_hit_zone_at_its_own_index() {
        let mut t = tree(&[("core", true, &["Pod", "Service"])]);
        let (text, hits) = render_to_string(&mut t, 24, 10);
        for (row, expected) in [(0usize, "core"), (1, "Pod"), (2, "Service")] {
            let y = (row + 1) as u16; // border
            assert!(text.lines().nth(y as usize).unwrap().contains(expected));
            assert_eq!(hits.hit(2, y), Some(&HitTarget::TreeRow(row)));
        }
    }

    #[test]
    fn an_unavailable_kind_says_so_instead_of_showing_a_count() {
        // On a corporate cluster the user lacks RBAC on some kinds. Showing a
        // perpetual blank or zero would read as "this kind is empty".
        let mut t = tree_with_unavailable("Secret", "forbidden");
        let (text, _) = render_to_string(&mut t, 30, 10);
        assert!(text.contains("Secret"));
        assert!(
            text.to_lowercase().contains("forbidden") || text.contains("—"),
            "unavailable kind rendered as if it were merely empty:\n{text}"
        );
    }

    #[test]
    fn hit_zones_follow_the_scrolled_sidebar() {
        // The real guarantee, re-checked for the tree the same way task 4's
        // report re-checked it for the table: the row you can SEE at screen
        // row y is the row you SELECT by clicking y, even once scrolled.
        // With 40 kinds in a 10-row pane, most of the list only exists below
        // the fold — a fixture that fit entirely on screen could not tell a
        // correct implementation from one that registers zones at the
        // absolute row number regardless of scroll.
        let mut t = many_kinds(40);
        t.selected = 35;
        let (text, hits) = render_to_string(&mut t, 30, 10);
        let lines: Vec<&str> = text.lines().collect();

        // A 10-row area spends one line on each border, leaving 8 data rows.
        // selected=35 is 28 rows past what an offset of 0 could show, so the
        // window must have scrolled by the minimum that brings it on screen:
        // 35 + 1 - 8 = 28, not further, not "just enough plus some margin".
        assert_eq!(
            t.scroll, 28,
            "the offset must move by the minimum that keeps row 35 visible \
             in an 8-row window"
        );

        for (k, y) in (1u16..=8).enumerate() {
            let absolute_row = 28 + k;
            // Row 0 of the flattened tree is the "apps" group header; kind
            // index = absolute_row - 1.
            let expected_label = format!("Kind{:02}", absolute_row - 1);
            assert!(
                lines[y as usize].contains(&expected_label),
                "expected {expected_label} drawn at y={y}; got: {}",
                lines[y as usize]
            );
            assert_eq!(
                hits.hit(2, y),
                Some(&HitTarget::TreeRow(absolute_row)),
                "clicking the row drawn at y={y} must resolve to the kind \
                 shown there (absolute row {absolute_row}), not another row"
            );
        }
    }

    #[test]
    fn a_pane_too_narrow_to_hold_anything_does_not_panic() {
        let mut t = tree(&[("core", true, &["Pod"])]);
        let (_text, _hits) = render_to_string(&mut t, 0, 10);
        let (_text, _hits) = render_to_string(&mut t, 10, 0);
    }

    #[test]
    fn the_selected_row_is_visually_distinct_from_the_others() {
        // `tree.selected` already drives scrolling (see
        // `hit_zones_follow_the_scrolled_sidebar` above). If it does not also
        // change what is drawn, moving the selection scrolls the pane while
        // showing the user nothing — and a test that only sets and reads
        // `tree.selected` through the same field can never see that gap.
        // Plan 2 shipped exactly this shape in its table view.
        let mut t = tree(&[("core", true, &["Pod", "Service"])]);
        t.selected = 2; // "Service": flattened row 2, drawn at y=3 (1 border + 2 rows above)
        let (lines, styles, _hits) = render_with_styles(&mut t, 30, 10);
        assert!(
            lines[3].contains("Service"),
            "expected Service drawn at y=3; got: {}",
            lines[3]
        );
        assert_ne!(
            styles[3], styles[2],
            "the selected row renders identically to an unselected one"
        );
    }

    #[test]
    fn the_highlight_follows_the_selection_when_the_sidebar_scrolls() {
        // The test above alone is not enough: a highlight painted at a fixed
        // screen row would still pass it, because in that small fixture the
        // selected row and a hardcoded row happen to coincide. This is the
        // test that actually distinguishes "highlights the selection" from
        // "highlights whatever happens to be at row 3" — the highlight must
        // land on the row the selection occupies AFTER scrolling, not at a
        // fixed screen position.
        let mut t = many_kinds(40);
        t.selected = 30;
        let (lines, styles, _hits) = render_with_styles(&mut t, 30, 10);

        // Same arithmetic as hit_zones_follow_the_scrolled_sidebar's sibling
        // fixture: an 8-row window, selected=30, scrolls to offset 23, so
        // the selection (kind index 29) draws on the LAST visible row, y=8 —
        // not y=3, which is where the small fixture's selection happened to
        // land.
        assert!(
            lines[8].contains("Kind29"),
            "expected the selected kind (absolute row 30) at y=8; got: {}",
            lines[8]
        );
        assert_eq!(
            styles[8].bg,
            Some(theme::DUSK),
            "the row the selection actually occupies after scrolling must \
             carry the highlight"
        );
        assert_ne!(
            styles[1].bg,
            Some(theme::DUSK),
            "an unselected row (Kind22, the first row drawn) must not be \
             highlighted"
        );
    }
}
