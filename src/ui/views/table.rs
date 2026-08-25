use crate::store::columns::columns_for;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use kube::api::{DynamicObject, GroupVersionKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table, TableState};
use std::sync::Arc;

pub struct TableView {
    pub state: TableState,
    pub scroll_offset: usize,
}

impl Default for TableView {
    fn default() -> Self {
        Self::new()
    }
}

impl TableView {
    pub fn new() -> Self {
        Self {
            state: TableState::default().with_selected(Some(0)),
            scroll_offset: 0,
        }
    }
}

/// Colour a pod phase by severity so problems are visible without reading.
pub fn phase_style(phase: &str) -> Style {
    let color = match phase {
        "Running" | "Succeeded" => theme::OK,
        "Pending" | "ContainerCreating" => theme::WARN,
        "Failed" | "CrashLoopBackOff" | "Error" | "ImagePullBackOff" => theme::ERR,
        _ => theme::MUTED,
    };
    Style::default().fg(color)
}

/// Render the resource table and register a clickable zone for every visible row.
///
/// Rows are registered against the same geometry ratatui uses to lay them out:
/// the block border takes one line, the header one more, so the first data row
/// begins at `area.y + 2`.
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

    let header = Row::new(columns.iter().map(|c| c.header).collect::<Vec<_>>()).style(
        Style::default()
            .fg(theme::HEADER)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = objects
        .iter()
        .map(|obj| {
            let cells: Vec<String> = columns.iter().map(|c| (c.extract)(obj)).collect();
            // Style the whole row by phase when the kind exposes one.
            let style = columns
                .iter()
                .position(|c| c.header == "STATUS")
                .map(|i| phase_style(&cells[i]))
                .unwrap_or_else(|| Style::default().fg(theme::FG));
            Row::new(cells).style(style)
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .fg(theme::SELECTED)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(gvk.kind.clone()),
        );

    f.render_stateful_widget(table, area, &mut view.state);

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
    for (i, _) in objects.iter().enumerate() {
        let y = first_row_y.saturating_add(i as u16);
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
            HitTarget::TableRow(i),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn failing_phases_are_styled_differently_from_running() {
        assert_ne!(
            phase_style("Running"),
            phase_style("CrashLoopBackOff"),
            "a failing pod must be visually distinct"
        );
        assert_ne!(phase_style("Running"), phase_style("Pending"));
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
}
