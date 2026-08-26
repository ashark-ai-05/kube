use crate::store::columns::{ColumnSource, column_source};
use crate::store::table::{SortState, TableData, sort_rows, sort_table_rows};
use crate::ui::geometry::column_offsets;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::scroll;
use crate::ui::theme;
// Re-exported so this view's existing call sites and tests keep naming it
// here; the implementation now lives in `ui::scroll`, shared with the picker.
pub use crate::ui::scroll::scroll_offset;
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
    /// Requested sort, set by `toggle_sort` in response to a column header
    /// click (`HitTarget::ColumnHeader`, resolved via `column_offsets`).
    /// `None` means "whatever order the source returned" — object-store
    /// insertion order for `ColumnSource::Builtin`, or the fetched
    /// `TableData`'s own row order for `ColumnSource::Server`.
    pub sort: Option<SortState>,
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
            sort: None,
        }
    }

    /// Click-to-sort: a click on a NEW column sorts it ascending; a second
    /// click on the SAME column reverses direction. There is no third click
    /// back to "unsorted" — nothing currently represents that as a
    /// selectable state, matching the common spreadsheet/`kubectl -o wide`
    /// two-state convention rather than inventing a third.
    pub fn toggle_sort(&mut self, column: usize) {
        self.sort = Some(match self.sort {
            Some(s) if s.column == column => SortState {
                column,
                descending: !s.descending,
            },
            _ => SortState {
                column,
                descending: false,
            },
        });
    }
}

/// The half-open range of object indices that can actually be drawn.
///
/// Formatting the whole list every frame costs O(objects); this makes it
/// O(viewport). The block border takes one line at the top and one at the
/// bottom, and the header takes one more, leaving `height - 3` data rows —
/// this view's own chrome, which is all this wrapper adds over the shared
/// `scroll::window`.
pub fn visible_window(offset: usize, area_height: u16, total: usize) -> std::ops::Range<usize> {
    scroll::window(offset, data_rows(area_height), total)
}

/// How many data rows fit in an area of this height: everything but the two
/// borders and the header.
fn data_rows(area_height: u16) -> usize {
    area_height.saturating_sub(3) as usize
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
    render_table_with_data(f, area, objects, gvk, None, view, hits);
}

/// As `render_table`, but sources columns and rows from a fetched
/// `TableData` when one is available for `gvk` — kubectl's own columns,
/// including a CRD's declared printer columns — falling back to the
/// builtin registry otherwise (`store::columns::column_source`). Also
/// applies `view.sort`, set by `TableView::toggle_sort` in response to a
/// column header click.
///
/// `table_data` is consumed by value: the caller reads it out of a store
/// snapshot (`ResourceStore::table_data`, which already clones), so there
/// is nothing left to borrow from by the time it arrives here, and
/// `ColumnSource::Server` owns its `TableData` per its own definition.
///
/// Performs no I/O. Fetching a `TableData` is a per-kind-change REQUEST
/// issued elsewhere (see `store::table::fetch_table`'s doc comment) — this
/// function, like `render_table`, only ever reads what has already
/// arrived, because both run inside the draw closure, once per frame.
pub fn render_table_with_data(
    f: &mut Frame,
    area: Rect,
    objects: &[Arc<DynamicObject>],
    gvk: &GroupVersionKind,
    table_data: Option<TableData>,
    view: &mut TableView,
    hits: &mut HitRegistry,
) {
    let source = column_source(gvk, table_data);

    let headers: Vec<String> = match &source {
        ColumnSource::Builtin(cols) => cols.iter().map(|c| c.header.to_string()).collect(),
        ColumnSource::Server(t) => t.columns.iter().map(|c| c.name.clone()).collect(),
    };
    let widths: Vec<Constraint> = match &source {
        ColumnSource::Builtin(cols) => cols.iter().map(|c| c.width).collect(),
        ColumnSource::Server(t) => vec![Constraint::Fill(1); t.columns.len()],
    };
    // Row styling by phase works the same way regardless of source: find
    // whichever column is named "status" and look up its value there.
    // Server column names are kubectl's own Title Case ("Status"); the
    // builtin registry's are upper case ("STATUS") — compared
    // case-insensitively so neither source has to match the other's
    // convention.
    let status_idx = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("status"));

    // Objects/rows can shrink between frames (a pod is deleted, or a fresh
    // fetch lands with fewer rows), leaving `selected` past the end. Clamp
    // here so no caller has to remember to.
    let total = match &source {
        ColumnSource::Builtin(_) => objects.len(),
        ColumnSource::Server(t) => t.rows.len(),
    };
    view.selected = view.selected.min(total.saturating_sub(1));

    let header = Row::new(headers.clone()).style(theme::header_style());

    // This view owns scrolling: compute how many data rows fit, advance the
    // offset by the least amount needed to keep the selection visible, then
    // derive the visible window from that offset. Row construction and hit
    // zones below both derive from this one `window`, so they cannot drift
    // apart the way Plan 1's shipped defect did.
    let rows_visible = data_rows(area.height);
    view.offset = scroll_offset(view.selected, view.offset, rows_visible);
    let window = visible_window(view.offset, area.height, total);

    // Sorting needs a full ordering before "the visible window" means
    // anything, so it is the one case allowed to cost O(total) rather than
    // O(viewport) — unavoidable for any sort, not a regression of Task 4's
    // guarantee, which only ever covered the unsorted path (still exercised
    // by `render_table`/`only_visible_rows_are_formatted` below, where
    // `view.sort` stays `None`).
    //
    // `Server` rows sort through `sort_table_rows`, not `sort_rows`: a
    // `TableRow` bundles its cells with the identity of the object it
    // displays (`store::table::TableRow`), and `sort_table_rows` reorders
    // that whole bundle so identity always moves with its cells. `Builtin`
    // rows have no separate identity to carry — the cells ARE extracted
    // from `objects` in this exact call, in this exact order — so they stay
    // on the plain `sort_rows`/`Vec<Vec<String>>` path.
    let rows: Vec<Row> = match (&source, &view.sort) {
        (ColumnSource::Builtin(cols), Some(sort)) => {
            let mut all_rows: Vec<Vec<String>> = objects
                .iter()
                .map(|obj| cols.iter().map(|c| (c.extract)(obj)).collect())
                .collect();
            sort_rows(&mut all_rows, sort);
            all_rows[window.clone()]
                .iter()
                .map(|cells| styled_row(cells, status_idx))
                .collect()
        }
        (ColumnSource::Builtin(cols), None) => objects[window.clone()]
            .iter()
            .map(|obj| {
                let cells: Vec<String> = cols.iter().map(|c| (c.extract)(obj)).collect();
                styled_row(&cells, status_idx)
            })
            .collect(),
        (ColumnSource::Server(t), Some(sort)) => {
            let mut all_rows = t.rows.clone();
            sort_table_rows(&mut all_rows, sort);
            all_rows[window.clone()]
                .iter()
                .map(|row| styled_row(&row.cells, status_idx))
                .collect()
        }
        (ColumnSource::Server(t), None) => t.rows[window.clone()]
            .iter()
            .map(|row| styled_row(&row.cells, status_idx))
            .collect(),
    };
    let row_count = rows.len();

    // Window-relative selection: ratatui is given exactly the rows it draws,
    // so its own out-of-bounds clamp on `selected` is a no-op and it has
    // nothing left to scroll. This TableState is freshly built every frame
    // and discarded after the call — nothing persists it.
    let selected_in_window = view
        .selected
        .checked_sub(window.start)
        .filter(|i| *i < row_count);
    let mut render_state = TableState::default().with_selected(selected_in_window);

    let table = Table::new(rows, widths.clone())
        .header(header)
        .row_highlight_style(
            Style::default()
                .fg(theme::INDIGO)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::border_style())
                .title(gvk.kind.clone()),
        );

    f.render_stateful_widget(table, area, &mut render_state);

    // Column header hit zones: one per column, at the geometry `Table`
    // ACTUALLY drew them at (`column_offsets` reproduces its two-stage
    // layout — see that function's doc comment). A single whole-row zone
    // (this view's pre-Task-6 behaviour) cannot distinguish which column
    // was clicked, which click-to-sort needs. `selection_width` is 0: this
    // `Table` sets no `highlight_symbol`, so that is what `Table` itself
    // uses internally too.
    let header_y = area.y.saturating_add(1);
    if header_y < area.y + area.height {
        let header_area = Rect {
            x: area.x + 1,
            y: header_y,
            width: area.width.saturating_sub(2),
            height: 1,
        };
        for (i, rect) in column_offsets(&widths, header_area, 1, 0)
            .into_iter()
            .enumerate()
        {
            hits.push(rect, 0, HitTarget::ColumnHeader(i));
        }
    }

    let first_row_y = area.y.saturating_add(2);
    let last_y = area.y + area.height.saturating_sub(1);
    for k in 0..row_count {
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

/// Style one already-extracted row by its STATUS/Status cell, if it has
/// one, falling back to plain body text otherwise.
fn styled_row(cells: &[String], status_idx: Option<usize>) -> Row<'static> {
    let style = status_idx
        .and_then(|i| cells.get(i))
        .map(|s| phase_style(s))
        .unwrap_or_else(|| Style::default().fg(theme::PAPER));
    Row::new(cells.to_vec()).style(style)
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
    fn a_selection_past_the_end_is_clamped_when_the_list_shrinks() {
        // A watch delete can drop objects out from under the selection.
        let pods: Vec<_> = (0..3)
            .map(|i| pod(&format!("pod-{i}"), "Running"))
            .collect();
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let mut view = TableView::new();
        view.selected = 99;
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| render_table(f, f.area(), &pods, &gvk, &mut view, &mut hits))
            .unwrap();
        assert_eq!(
            view.selected, 2,
            "selection must clamp to the last remaining object"
        );
    }

    #[test]
    fn rendering_an_empty_list_leaves_the_selection_at_zero() {
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        let mut view = TableView::new();
        view.selected = 5;
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| render_table(f, f.area(), &[], &gvk, &mut view, &mut hits))
            .unwrap();
        assert_eq!(
            view.selected, 0,
            "an empty list must not leave a dangling selection"
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

    // --- TableView::toggle_sort ---

    #[test]
    fn toggle_sort_starts_a_new_column_ascending() {
        let mut view = TableView::new();
        view.toggle_sort(2);
        assert_eq!(
            view.sort,
            Some(SortState {
                column: 2,
                descending: false
            })
        );
    }

    #[test]
    fn toggle_sort_reverses_on_a_second_click_of_the_same_column() {
        let mut view = TableView::new();
        view.toggle_sort(1);
        view.toggle_sort(1);
        assert_eq!(
            view.sort,
            Some(SortState {
                column: 1,
                descending: true
            })
        );
    }

    #[test]
    fn toggle_sort_resets_to_ascending_on_a_different_column() {
        let mut view = TableView::new();
        view.toggle_sort(1);
        view.toggle_sort(1); // now descending
        view.toggle_sort(3); // a different column must reset, not carry descending over
        assert_eq!(
            view.sort,
            Some(SortState {
                column: 3,
                descending: false
            })
        );
    }

    // --- render_table_with_data: ColumnSource preference and sort wiring ---

    fn sample_table_data() -> TableData {
        use crate::store::table::{TableColumn, TableRow};
        TableData {
            columns: vec![
                TableColumn {
                    name: "Custom".to_string(),
                    priority: 0,
                },
                TableColumn {
                    name: "Status".to_string(),
                    priority: 0,
                },
            ],
            rows: vec![
                TableRow {
                    cells: vec!["b-thing".to_string(), "Ready".to_string()],
                    identity: None,
                },
                TableRow {
                    cells: vec!["a-thing".to_string(), "NotReady".to_string()],
                    identity: None,
                },
            ],
        }
    }

    fn dump(term: &Terminal<TestBackend>, w: u16, h: u16) -> String {
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn server_columns_render_when_a_table_has_been_fetched() {
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let mut view = TableView::new();
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("example.com", "v1", "Widget");
        term.draw(|f| {
            render_table_with_data(
                f,
                f.area(),
                &[],
                &gvk,
                Some(sample_table_data()),
                &mut view,
                &mut hits,
            );
        })
        .unwrap();

        let text = dump(&term, 60, 8);
        assert!(
            text.contains("Custom"),
            "expected the server's own column header, got:\n{text}"
        );
        assert!(
            text.contains("b-thing"),
            "expected a server row's cell value, got:\n{text}"
        );
        assert!(
            !text.contains("NAME"),
            "the builtin registry's headers must not appear once a table was fetched, got:\n{text}"
        );
    }

    #[test]
    fn without_a_fetched_table_the_builtin_registry_still_renders() {
        // A kind with no TableData yet — fetch in flight, or failed — must
        // still show something rather than going blank.
        let pods = vec![pod("a", "Running")];
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let mut view = TableView::new();
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| {
            render_table_with_data(f, f.area(), &pods, &gvk, None, &mut view, &mut hits);
        })
        .unwrap();

        let text = dump(&term, 60, 8);
        assert!(
            text.contains("READY"),
            "must fall back to the builtin registry, got:\n{text}"
        );
    }

    #[test]
    fn the_views_sort_state_reorders_server_rows() {
        let mut term = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let mut view = TableView::new();
        view.sort = Some(SortState {
            column: 0,
            descending: false,
        });
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("example.com", "v1", "Widget");
        term.draw(|f| {
            render_table_with_data(
                f,
                f.area(),
                &[],
                &gvk,
                Some(sample_table_data()),
                &mut view,
                &mut hits,
            );
        })
        .unwrap();

        let buf = term.backend().buffer();
        let row2: String = (0..60u16)
            .map(|x| buf[(x, 2)].symbol().to_string())
            .collect();
        assert!(
            row2.contains("a-thing"),
            "sorted ascending by column 0, 'a-thing' must be the first data row, got:\n{row2}"
        );
    }

    #[test]
    fn column_headers_register_per_column_hit_zones_not_one_zone_for_the_whole_row() {
        let pods = vec![pod("a", "Running")];
        let (_, hits) = render(&pods, 60, 8);
        let mut seen = std::collections::HashSet::new();
        for x in 1..59u16 {
            if let Some(HitTarget::ColumnHeader(i)) = hits.hit(x, 1) {
                seen.insert(*i);
            }
        }
        assert!(
            seen.len() >= 2,
            "expected multiple distinct column-header hit zones across the row, got {seen:?}"
        );
    }
}
