use crate::store::columns::columns_for;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use crate::ui::theme::phase_style;
use kube::api::{DynamicObject, GroupVersionKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table, TableState};
use std::sync::Arc;

pub struct TableView {
    /// Absolute index of the selected object, owned by this view rather than
    /// by ratatui. Callers (e.g. `main.rs`, which tracks its own `selected`
    /// in response to `Action::SelectRow`/`Action::ScrollBy`) write the real
    /// selection here directly.
    pub selected: usize,
    /// Scroll offset, owned by this view and advanced by `scroll_offset` each
    /// frame. Never read back out of a ratatui `TableState` after render:
    /// `render_table` always hands `Table` an already-windowed row list with
    /// a window-relative selection, so ratatui has nothing left to scroll and
    /// cannot disagree with this value.
    pub offset: usize,
}

impl Default for TableView {
    fn default() -> Self {
        Self::new()
    }
}

impl TableView {
    pub fn new() -> Self {
        Self {
            selected: 0,
            offset: 0,
        }
    }
}

/// The half-open range of object indices that can actually be drawn.
///
/// Formatting the whole list every frame costs O(objects); this makes it
/// O(viewport). The block border takes one line at the top and one at the
/// bottom, and the header takes one more, leaving `height - 3` data rows.
pub fn visible_window(offset: usize, area_height: u16, total: usize) -> std::ops::Range<usize> {
    let rows = area_height.saturating_sub(3) as usize;
    let start = offset.min(total);
    let end = start.saturating_add(rows).min(total);
    start..end
}

/// Offset that keeps `selected` visible, moving as little as possible.
///
/// `render_table` owns scrolling outright rather than delegating it to
/// ratatui. The straightforward design — hand `Table` a windowed `Vec<Row>`
/// while leaving `TableState::selected` as an absolute object index — was
/// tried first and found empirically unsafe: `ratatui::widgets::Table::render`
/// clamps `state.selected` to `rows.len() - 1` whenever `selected >=
/// rows.len()`, so a windowed row list silently rewrites the real selection
/// (verified with a probe: `selected = 30` against a 7-row list came back as
/// `selected = Some(6)` after render). No choice of offset avoids that clamp,
/// because it fires purely off `rows.len()`. Computing the offset ourselves
/// and handing ratatui a window-relative selection sidesteps it entirely:
/// ratatui always sees exactly the rows it draws, so its own clamp is a
/// no-op and it never scrolls on its own. See task-4-report.md for the full
/// empirical finding.
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

/// Render the resource table and register a clickable zone for every visible row.
///
/// Rows are registered against the same geometry ratatui uses to lay them out:
/// the block border takes one line, the header one more, so the first data row
/// begins at `area.y + 2`.
///
/// Zones map screen rows to the *visible window*, not to absolute object
/// indices: this view owns scrolling (see `scroll_offset`), so the object
/// drawn at `area.y + 2` is `view.offset`, not `0`.
///
/// Only the objects in `visible_window(view.offset, area.height, ...)` are
/// formatted — the whole point of Task 4 — and hit zones are registered
/// against that identical window so the two cannot drift apart.
pub fn render_table(
    f: &mut Frame,
    area: Rect,
    objects: &[Arc<DynamicObject>],
    gvk: &GroupVersionKind,
    view: &mut TableView,
    hits: &mut HitRegistry,
) {
    let columns = columns_for(gvk);
    let widths: Vec<Constraint> = columns.iter().map(|c| c.width).collect();

    let header =
        Row::new(columns.iter().map(|c| c.header).collect::<Vec<_>>()).style(theme::header_style());

    // This view owns scrolling: compute how many data rows fit, advance the
    // offset by the least amount needed to keep the selection visible, then
    // derive the visible window from that offset. Row construction and hit
    // zones below both derive from this one `window`, so they cannot drift
    // apart the way Plan 1's shipped defect did.
    let rows_visible = area.height.saturating_sub(3) as usize;
    view.offset = scroll_offset(view.selected, view.offset, rows_visible);
    let window = visible_window(view.offset, area.height, objects.len());

    let rows: Vec<Row> = objects[window.clone()]
        .iter()
        .map(|obj| {
            let cells: Vec<String> = columns.iter().map(|c| (c.extract)(obj)).collect();
            // Style the whole row by phase when the kind exposes one.
            let style = columns
                .iter()
                .position(|c| c.header == "STATUS")
                .map(|i| phase_style(&cells[i]))
                .unwrap_or_else(|| Style::default().fg(theme::PAPER));
            Row::new(cells).style(style)
        })
        .collect();

    // Window-relative selection: ratatui is given exactly the rows it draws,
    // so its own out-of-bounds clamp on `selected` is a no-op and it has
    // nothing left to scroll. This TableState is freshly built every frame
    // and discarded after the call — nothing persists it.
    let selected_in_window = view
        .selected
        .checked_sub(window.start)
        .filter(|i| *i < rows.len());
    let mut render_state = TableState::default().with_selected(selected_in_window);

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .fg(theme::INDIGO)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_style(true))
                .title(gvk.kind.clone()),
        );

    f.render_stateful_widget(table, area, &mut render_state);

    // Register hit zones matching the geometry above.
    let header_y = area.y.saturating_add(1);
    if header_y < area.y + area.height {
        hits.push(
            Rect {
                x: area.x + 1,
                y: header_y,
                width: area.width.saturating_sub(2),
                height: 1,
            },
            0,
            HitTarget::ColumnHeader(0),
        );
    }

    let first_row_y = area.y.saturating_add(2);
    let last_y = area.y + area.height.saturating_sub(1);
    for (k, _) in objects[window.clone()].iter().enumerate() {
        let y = first_row_y.saturating_add(k as u16);
        if y >= last_y {
            break;
        }
        hits.push(
            Rect {
                x: area.x + 1,
                y,
                width: area.width.saturating_sub(2),
                height: 1,
            },
            0,
            HitTarget::TableRow(window.start + k),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::columns::Column;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn pod(name: &str, phase: &str) -> Arc<DynamicObject> {
        let mut o = DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({
            "status": {
                "phase": phase,
                "containerStatuses": [{"ready": true, "restartCount": 0}]
            }
        });
        Arc::new(o)
    }

    fn render(objects: &[Arc<DynamicObject>], w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut view = TableView::new();
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| {
            let area = f.area();
            render_table(f, area, objects, &gvk, &mut view, &mut hits);
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
    fn renders_the_column_headers() {
        let (text, _) = render(&[pod("a", "Running")], 60, 8);
        assert!(text.contains("NAME"), "expected NAME header in:\n{text}");
        assert!(text.contains("READY"), "expected READY header in:\n{text}");
        assert!(
            text.contains("STATUS"),
            "expected STATUS header in:\n{text}"
        );
    }

    #[test]
    fn renders_object_names_and_derived_cells() {
        let (text, _) = render(&[pod("api-7d9f-x2k", "Running")], 60, 8);
        assert!(
            text.contains("api-7d9f-x2k"),
            "expected pod name in:\n{text}"
        );
        assert!(text.contains("Running"), "expected phase in:\n{text}");
        assert!(text.contains("1/1"), "expected ready ratio in:\n{text}");
    }

    #[test]
    fn registers_one_hit_zone_per_visible_row() {
        let pods = vec![
            pod("a", "Running"),
            pod("b", "Running"),
            pod("c", "Running"),
        ];
        let (_, hits) = render(&pods, 60, 10);
        let mut found = Vec::new();
        for row in 0..10u16 {
            if let Some(HitTarget::TableRow(i)) = hits.hit(5, row) {
                found.push(*i);
            }
        }
        assert_eq!(found, vec![0, 1, 2], "each rendered row must be clickable");
    }

    #[test]
    fn registers_clickable_column_headers() {
        let (_, hits) = render(&[pod("a", "Running")], 60, 8);
        let mut found_header = false;
        for row in 0..8u16 {
            if matches!(hits.hit(2, row), Some(HitTarget::ColumnHeader(_))) {
                found_header = true;
            }
        }
        assert!(found_header, "the header row must be clickable for sorting");
    }

    #[test]
    fn an_empty_table_renders_without_panicking() {
        let (text, _) = render(&[], 60, 8);
        assert!(
            text.contains("NAME"),
            "headers still show when there are no rows"
        );
    }

    #[test]
    fn a_tiny_viewport_renders_exactly_the_available_lines() {
        // A terminal too small for the header plus any row is a real crash
        // source in layout code; this pins both non-panic and correct extent.
        let pods = vec![pod("a", "Running"), pod("b", "Running")];
        let (text, _) = render(&pods, 12, 3);
        assert_eq!(
            text.lines().count(),
            3,
            "must fill exactly the viewport height"
        );
        assert!(
            text.lines().all(|l| l.chars().count() == 12),
            "no line may exceed the width"
        );
    }

    #[test]
    fn hit_zones_align_with_the_rows_ratatui_actually_draws() {
        // The real guarantee: the row you can SEE at screen row y is the row
        // you SELECT by clicking y. Asserting the zone sequence alone does not
        // pin this — the whole block can shift and the sequence still matches.
        let pods = vec![
            pod("row-zero", "Running"),
            pod("row-one", "Running"),
            pod("row-two", "Running"),
        ];
        let (text, hits) = render(&pods, 60, 10);
        let lines: Vec<&str> = text.lines().collect();

        assert!(
            lines[1].contains("NAME"),
            "header expected at y=1, got: {}",
            lines[1]
        );
        assert_eq!(hits.hit(5, 1), Some(&HitTarget::ColumnHeader(0)));

        for (i, name) in ["row-zero", "row-one", "row-two"].iter().enumerate() {
            let y = 2 + i;
            assert!(
                lines[y].contains(name),
                "expected {name} drawn at y={y}, got: {}",
                lines[y]
            );
            assert_eq!(
                hits.hit(5, y as u16),
                Some(&HitTarget::TableRow(i)),
                "clicking the row drawn at y={y} must select index {i}, not another row"
            );
        }
    }

    #[test]
    fn the_selected_row_is_the_one_that_renders_highlighted() {
        // Regression guard: TableView used to carry a vestigial TableState
        // that main.rs wrote the real selection into while render_table read
        // a different, always-zero field — every test passed, but selection
        // was inert in the running binary. This exercises the full path from
        // "app sets a selection" to "that row renders highlighted".
        let pods: Vec<_> = (0..5)
            .map(|i| pod(&format!("pod-{i}"), "Running"))
            .collect();
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let mut view = TableView::new();
        view.selected = 2;
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| render_table(f, f.area(), &pods, &gvk, &mut view, &mut hits))
            .unwrap();

        let buf = term.backend().buffer();
        // pod-2 is the third data row: first_row_y = area.y + 2, so it draws
        // at y = 2 + 2 = 4. pod-0, an unselected row, draws at y = 2. Bold is
        // the discriminator, not "any style difference": phase_style never
        // sets it (verified in ui/theme.rs), only row_highlight_style does,
        // so checking bold pins WHICH row is highlighted, not just that two
        // rows differ — a bug that pins selection to row 0 would still make
        // row 0 (y=2) bold and row 2 (y=4) plain, and a same-row-index check
        // that only asserted inequality would miss that entirely.
        let is_bold = |y: u16| {
            (1..10).any(|x| {
                buf[(x, y)]
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
            })
        };
        assert!(
            is_bold(4),
            "the selected row (pod-2, drawn at y=4) must be bold"
        );
        assert!(
            !is_bold(2),
            "an unselected row (pod-0, drawn at y=2) must not be bold"
        );
    }

    #[test]
    fn hit_zones_follow_the_scrolled_viewport() {
        // This view owns scrolling (scroll_offset) to keep the selection
        // visible. Registering zones by absolute object index makes every
        // row past the first screenful select the wrong pod.
        let pods: Vec<_> = (0..40)
            .map(|i| pod(&format!("pod-{i:02}"), "Running"))
            .collect();
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        let mut view = TableView::new();
        view.selected = 30;
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| render_table(f, f.area(), &pods, &gvk, &mut view, &mut hits))
            .unwrap();

        let offset = view.offset;
        assert!(
            offset > 0,
            "expected the viewport to have scrolled, got offset {offset}"
        );

        // The pod drawn on the first data row must be the one a click there selects.
        let buf = term.backend().buffer();
        let first_row: String = (0..60).map(|x| buf[(x, 2)].symbol()).collect();
        let expected = format!("pod-{offset:02}");
        assert!(
            first_row.contains(&expected),
            "expected {expected} at y=2, got: {first_row}"
        );
        assert_eq!(
            hits.hit(5, 2),
            Some(&HitTarget::TableRow(offset)),
            "clicking the first visible row must select the pod drawn there"
        );
    }

    #[test]
    fn zones_are_not_registered_past_the_visible_area() {
        // 20 rows into a viewport with room for a handful: registration must
        // stop at the bottom border rather than running past the frame.
        let pods: Vec<_> = (0..20).map(|i| pod(&format!("p{i}"), "Running")).collect();
        let (_, hits) = render(&pods, 60, 8);

        for y in 0..40u16 {
            if let Some(HitTarget::TableRow(idx)) = hits.hit(5, y) {
                assert!(
                    y < 7,
                    "row zone {idx} registered at y={y}, at or past the bottom border of an 8-row viewport"
                );
            }
        }
    }

    #[test]
    fn the_visible_window_covers_only_rows_that_fit() {
        // A 10-row area spends 1 line on the top border, 1 on the header and
        // 1 on the bottom border, leaving 7 data rows.
        assert_eq!(visible_window(0, 10, 100), 0..7);
    }

    #[test]
    fn the_visible_window_follows_the_scroll_offset() {
        assert_eq!(visible_window(24, 10, 100), 24..31);
    }

    #[test]
    fn the_visible_window_is_clamped_to_the_object_count() {
        assert_eq!(
            visible_window(0, 10, 3),
            0..3,
            "must not run past the end of the list"
        );
        assert_eq!(visible_window(98, 10, 100), 98..100);
    }

    #[test]
    fn a_viewport_with_no_room_for_rows_yields_an_empty_window() {
        for h in [0u16, 1, 2, 3] {
            let w = visible_window(0, h, 100);
            assert!(
                w.start >= w.end || w.len() <= 1,
                "height {h} produced {w:?}"
            );
        }
        assert!(visible_window(0, 0, 100).is_empty());
    }

    #[test]
    fn an_offset_past_the_end_yields_an_empty_window_rather_than_panicking() {
        assert!(visible_window(500, 10, 100).is_empty());
    }

    #[test]
    fn scrolling_does_not_move_while_the_selection_is_visible() {
        assert_eq!(scroll_offset(5, 0, 10), 0);
        assert_eq!(
            scroll_offset(9, 0, 10),
            0,
            "the last visible row must not scroll"
        );
    }

    #[test]
    fn scrolling_follows_the_selection_downward_by_the_minimum() {
        assert_eq!(
            scroll_offset(10, 0, 10),
            1,
            "one past the window scrolls exactly one row"
        );
        assert_eq!(scroll_offset(50, 0, 10), 41);
    }

    #[test]
    fn scrolling_follows_the_selection_upward() {
        assert_eq!(scroll_offset(3, 20, 10), 3);
        assert_eq!(scroll_offset(19, 20, 10), 19);
    }

    #[test]
    fn a_zero_row_viewport_yields_offset_zero_rather_than_underflowing() {
        assert_eq!(scroll_offset(0, 0, 0), 0);
        assert_eq!(scroll_offset(100, 50, 0), 0);
    }

    #[test]
    fn only_visible_rows_are_formatted() {
        // The guarantee this task exists for: a 5000-object list in a small
        // viewport must format tens of rows, not thousands.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FORMATS: AtomicUsize = AtomicUsize::new(0);

        fn counting_extract(_o: &DynamicObject) -> String {
            FORMATS.fetch_add(1, Ordering::SeqCst);
            "x".to_string()
        }

        let pods: Vec<_> = (0..5000)
            .map(|i| pod(&format!("p{i}"), "Running"))
            .collect();
        let cols = vec![Column {
            header: "NAME",
            width: Constraint::Fill(1),
            extract: counting_extract,
        }];

        FORMATS.store(0, Ordering::SeqCst);
        let window = visible_window(0, 20, pods.len());
        for obj in &pods[window] {
            for c in &cols {
                let _ = (c.extract)(obj);
            }
        }

        let n = FORMATS.load(Ordering::SeqCst);
        assert!(
            n <= 20,
            "formatted {n} rows for a 20-row viewport; expected at most 20"
        );
    }
}
