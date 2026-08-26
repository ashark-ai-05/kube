//! The detail pane: a modal overlay showing one object across three tabs
//! (Overview, YAML, Events).
//!
//! Follows the same modal convention `views::picker` established — `Clear`
//! first (so the pane beneath does not bleed through wherever this overlay
//! doesn't paint a glyph), a rounded border, `theme::border_style()` — rather
//! than inventing a second convention for the same idea. The active tab
//! reuses the picker's/sidebar's own selection convention (`theme::DUSK`
//! background plus `Modifier::BOLD`) for the same reason: highlighting is one
//! idea in this app, not one idea per widget.
//!
//! `Tabs` (ratatui) exposes no per-tab geometry (see
//! `docs/superpowers/plan3-api-reference.md` D11), so tab hit zones are
//! computed with `geometry::tab_spans`, exactly as that module's own doc
//! comment prescribes, instead of a second, locally-invented layout loop.
//!
//! YAML and Events tab *content* are out of scope for this task (Task 8 and
//! Task 9 respectively) — both render a clearly-marked placeholder here.

use crate::store::columns::format_age;
use crate::ui::geometry::tab_spans;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use chrono::Utc;
use kube::api::{DynamicObject, ResourceExt};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Which of the three tabs is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Overview,
    Yaml,
    Events,
}

/// Fixed left-to-right order the tab bar renders in, and the order
/// `HitTarget::DetailTab`'s index refers to. Kept as one array so the drawn
/// order and the hit-tested order can never drift apart from each other.
const TAB_ORDER: [DetailTab; 3] = [DetailTab::Overview, DetailTab::Yaml, DetailTab::Events];

impl DetailTab {
    fn label(self) -> &'static str {
        match self {
            DetailTab::Overview => "Overview",
            DetailTab::Yaml => "YAML",
            DetailTab::Events => "Events",
        }
    }

    fn index(self) -> usize {
        TAB_ORDER.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// State for a single open detail pane: which tab is active, and the
/// independent scroll position each of the scrollable tabs remembers across
/// re-renders (Overview has nothing to scroll, so it carries none).
///
/// `yaml_scroll`/`events_scroll` are written by nothing in this task —
/// Task 8 and Task 9 own reading and advancing them for their own tab's
/// content — but they live on this shared struct rather than two separate
/// ones so switching tabs and reopening the pane does not lose either
/// position.
pub struct DetailPane {
    pub tab: DetailTab,
    pub yaml_scroll: u16,
    pub events_scroll: u16,
}

impl Default for DetailPane {
    fn default() -> Self {
        Self::new()
    }
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            tab: DetailTab::Overview,
            yaml_scroll: 0,
            events_scroll: 0,
        }
    }
}

/// The label/value rows the Overview tab shows for a given object.
///
/// Built from whatever the object actually has rather than a fixed
/// template: `Namespace`, `Node` and `Status` are each included only when the
/// underlying field is present, because cluster-scoped objects have no
/// namespace and non-Pod kinds (ConfigMaps, Secrets — anything without a
/// `status` block at all) have neither a node nor a phase. `Name` and `Age`
/// are the only rows guaranteed to exist for every object.
pub fn overview_rows(obj: &DynamicObject) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    rows.push(("Name".to_string(), obj.name_any()));

    if let Some(ns) = obj.metadata.namespace.as_ref() {
        rows.push(("Namespace".to_string(), ns.clone()));
    }

    if let Some(node) = obj
        .data
        .get("spec")
        .and_then(|s| s.get("nodeName"))
        .and_then(|n| n.as_str())
    {
        rows.push(("Node".to_string(), node.to_string()));
    }

    if let Some(phase) = obj
        .data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
    {
        rows.push(("Status".to_string(), phase.to_string()));
    }

    rows.push(("Age".to_string(), age_row(obj)));
    rows
}

/// `metadata.creationTimestamp` formatted the same compact way the table
/// does (`store::columns::format_age`) — an object with no recorded creation
/// time (unusual, but not impossible for a hand-built `DynamicObject` in a
/// test, or a partially-populated watch event) renders "?" rather than
/// panicking on an `unwrap`.
fn age_row(obj: &DynamicObject) -> String {
    match obj.metadata.creation_timestamp.as_ref() {
        Some(t) => format_age(&format!("{}", t.0), Utc::now()),
        None => "?".to_string(),
    }
}

/// Draw the detail pane's frame (border, tab bar, close affordance) and the
/// active tab's content, registering clickable zones for every tab and for
/// closing the pane.
///
/// `events` is deliberately not a parameter here: Task 9 owns the Events
/// tab's content and the `EventRow` type it will read from, neither of which
/// exist yet. Adding a placeholder type now would force Task 9 to either
/// match a guessed shape or immediately break this signature again — see
/// the task report for the full reasoning. Task 9 extends this signature
/// (and the `DetailTab::Events` arm below) when it lands.
pub fn render_detail(
    f: &mut Frame,
    area: Rect,
    obj: &DynamicObject,
    pane: &mut DetailPane,
    hits: &mut HitRegistry,
) {
    // Without `Clear`, whatever was drawn earlier in this same frame (the
    // table beneath) shows through wherever this overlay doesn't explicitly
    // paint a glyph — the same reasoning `render_picker` documents.
    f.render_widget(Clear, area);

    let title = format!(" {} ", obj.name_any());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_style())
        .title(Span::styled(title, theme::header_style()))
        .style(Style::default().bg(theme::ABYSS));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // A pane dragged to nothing must not panic; there is also nothing left
    // to draw a tab bar or content into.
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    render_tab_bar(f, inner, pane, hits);

    let content_area = Rect {
        x: inner.x,
        y: inner.y.saturating_add(1),
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    if content_area.height == 0 {
        return;
    }

    match pane.tab {
        DetailTab::Overview => render_overview(f, content_area, obj),
        DetailTab::Yaml => {
            render_placeholder(f, content_area, "YAML view — implemented in Task 8.")
        }
        DetailTab::Events => render_placeholder(f, content_area, "Events — implemented in Task 9."),
    }
}

/// The top row of `inner`: tab labels on the left (via `geometry::tab_spans`,
/// the shared measurement this module must not reimplement locally — see
/// D11), a `[x]` close affordance reserved on the right so the pane is
/// closeable by mouse and not only by `Esc`.
fn render_tab_bar(f: &mut Frame, inner: Rect, pane: &DetailPane, hits: &mut HitRegistry) {
    let close_label = "[x]";
    let close_width = UnicodeWidthStr::width(close_label) as u16;
    let gap: u16 = 1;

    let tabs_width = inner.width.saturating_sub(close_width.saturating_add(gap));
    let tabs_area = Rect {
        x: inner.x,
        y: inner.y,
        width: tabs_width,
        height: 1,
    };

    let labels: Vec<&str> = TAB_ORDER.iter().map(|t| t.label()).collect();
    let spans = tab_spans(&labels, tabs_area, 2);
    for (i, rect) in spans.iter().enumerate() {
        let active = pane.tab.index() == i;
        let style = if active {
            Style::default()
                .bg(theme::DUSK)
                .fg(theme::PAPER)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::muted_style()
        };
        f.render_widget(Paragraph::new(labels[i]).style(style), *rect);
        hits.push(*rect, 1, HitTarget::DetailTab(i));
    }

    if inner.width >= close_width {
        let close_rect = Rect {
            x: inner.x + inner.width - close_width,
            y: inner.y,
            width: close_width,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(close_label).style(theme::muted_style()),
            close_rect,
        );
        hits.push(close_rect, 1, HitTarget::DetailClose);
    }
}

/// Render `overview_rows(obj)` as aligned label/value lines, label column
/// width matching the widest label actually present so alignment holds
/// whether or not `Node`/`Status`/`Namespace` are shown.
fn render_overview(f: &mut Frame, area: Rect, obj: &DynamicObject) {
    let rows = overview_rows(obj);
    let label_width = rows
        .iter()
        .map(|(k, _)| UnicodeWidthStr::width(k.as_str()))
        .max()
        .unwrap_or(0);

    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!("{k:<label_width$}  "), theme::label_style()),
                Span::styled(v.clone(), theme::text_style()),
            ])
        })
        .collect();

    f.render_widget(Paragraph::new(lines), area);
}

/// A clearly-marked stand-in for tab content this task does not own.
fn render_placeholder(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(Paragraph::new(text).style(theme::muted_style()), area);
}

/// Serialize a DynamicObject to YAML string using serde_norway.
/// The output leads with apiVersion, kind, metadata (kubectl convention) and
/// requires no post-processing for readability.
fn object_to_yaml(obj: &DynamicObject) -> String {
    unimplemented!()
}

/// Count the number of lines in a YAML string.
/// Returns the count as u16, saturating if the count exceeds u16::MAX.
fn yaml_line_count(yaml: &str) -> u16 {
    unimplemented!()
}

/// Clamp a scroll position to valid bounds for a document.
/// Returns a scroll value that, when used with Paragraph::scroll((y, x)),
/// ensures the viewport never scrolls past the end of the document.
///
/// If total_lines <= viewport, returns 0 (document fits entirely).
/// Otherwise, clamps scroll to the range [0, total_lines - viewport].
fn clamp_scroll(scroll: u16, total_lines: u16, viewport: u16) -> u16 {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{ConfigMap, Pod};
    use kube::api::ApiResource;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Style;

    /// A Pod with the fields the Overview tab exists to surface: a
    /// namespace, a node it was scheduled to, and a phase — everything a
    /// vacuous fixture (an object with every field, or a template-shaped
    /// implementation) could not be told apart on.
    fn pod_with_status() -> DynamicObject {
        let mut o = DynamicObject::new("web-1", &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({
            "spec": { "nodeName": "node-7" },
            "status": { "phase": "Running" },
        });
        o
    }

    /// A cluster-scoped-looking, status-less object — ConfigMaps and
    /// Secrets have no `status` block at all, and this one also has no
    /// namespace set, so a wrong implementation that assumes either exists
    /// would panic rather than merely render blank fields.
    fn bare_object(name: &str) -> DynamicObject {
        DynamicObject::new(name, &ApiResource::erase::<ConfigMap>(&()))
    }

    fn render_to_string(active: DetailTab, w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: active,
            yaml_scroll: 0,
            events_scroll: 0,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits);
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

    /// Renders with the given tab active and returns the `Style` painted at
    /// the fixed screen coordinate the "Overview" tab label always starts
    /// at (x=1, y=1: one cell in from the left/top border — confirmed
    /// against a real buffer dump, not assumed), plus the `HitRegistry` for
    /// callers that also want to check hit zones.
    fn render_styles(active: DetailTab) -> (Style, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: active,
            yaml_scroll: 0,
            events_scroll: 0,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits);
        })
        .unwrap();
        let buf = term.backend().buffer();
        (buf[(1, 1)].style(), hits)
    }

    #[test]
    fn overview_shows_the_fields_you_open_a_pod_to_check() {
        let obj = pod_with_status();
        let rows = overview_rows(&obj);
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        for expected in ["Name", "Namespace", "Node", "Status", "Age"] {
            assert!(
                keys.contains(&expected),
                "overview missing {expected}: {keys:?}"
            );
        }
    }

    #[test]
    fn overview_of_an_object_with_no_status_still_renders_its_metadata() {
        // ConfigMaps and Secrets have no status block at all.
        let rows = overview_rows(&bare_object("my-config"));
        assert!(rows.iter().any(|(k, v)| k == "Name" && v == "my-config"));
    }

    #[test]
    fn overview_of_a_status_less_object_omits_node_and_status_rows() {
        // The stronger half of the fixture above: a template-shaped
        // implementation that always emits "Node"/"Status" (blank or not)
        // would pass the assertion above yet still not be "renders what
        // exists" — this pins that those keys are actually ABSENT, not
        // merely blank.
        let rows = overview_rows(&bare_object("my-config"));
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!keys.contains(&"Node"), "a ConfigMap has no node: {keys:?}");
        assert!(
            !keys.contains(&"Status"),
            "a ConfigMap has no status: {keys:?}"
        );
        assert!(
            !keys.contains(&"Namespace"),
            "this fixture set no namespace: {keys:?}"
        );
    }

    #[test]
    fn each_tab_is_clickable_and_maps_to_its_own_index() {
        let (_, hits) = render_to_string(DetailTab::Overview, 60, 20);
        let mut found = Vec::new();
        for y in 0..20u16 {
            for x in 0..60u16 {
                if let Some(HitTarget::DetailTab(i)) = hits.hit(x, y)
                    && !found.contains(i)
                {
                    found.push(*i);
                }
            }
        }
        assert_eq!(found, vec![0, 1, 2], "all three tabs must be clickable");
    }

    #[test]
    fn tab_hit_zones_align_with_the_labels_actually_drawn() {
        // The test above proves all three indices exist somewhere; it does
        // not prove WHICH pixel maps to WHICH tab. A tab bar hit-tested with
        // `.chars().count()` instead of `geometry::tab_spans` (or one that
        // registers a constant index) would still pass it while clicking
        // "YAML" selected "Overview" instead. This asserts the label drawn
        // at a coordinate and the target registered at that SAME coordinate
        // agree, for every tab, using positions read back from the actual
        // rendered buffer rather than recomputed independently.
        let (text, hits) = render_to_string(DetailTab::Overview, 60, 20);
        let line = text.lines().nth(1).expect("tab bar drawn at y=1");

        for (label, expected_index) in [("Overview", 0), ("YAML", 1), ("Events", 2)] {
            let x = line
                .find(label)
                .unwrap_or_else(|| panic!("{label} not drawn on the tab bar row: {line}"))
                as u16;
            assert_eq!(
                hits.hit(x, 1),
                Some(&HitTarget::DetailTab(expected_index)),
                "clicking where {label} is drawn must select tab {expected_index}"
            );
        }
    }

    #[test]
    fn the_divider_between_tabs_is_not_swallowed_into_the_next_tabs_zone() {
        // `tab_hit_zones_align_with_the_labels_actually_drawn` above checks
        // that whatever is DRAWN at a coordinate matches whatever is
        // REGISTERED there — but a local reimplementation using
        // `.chars().count()` and dropping the divider entirely (`x += w`
        // with no gap) draws and registers from the very same wrong
        // coordinates, so draw and hit-zone stay internally consistent with
        // EACH OTHER while both silently disagree with
        // `geometry::tab_spans`'s real layout — that alignment test alone
        // cannot see this. This test instead computes the tab bar's
        // geometry independently, via the actual shared `tab_spans`
        // function the module is required to use, and confirms the pixel
        // immediately after a tab ends (the two-column divider) belongs to
        // NEITHER tab, not that it silently belongs to the one that follows.
        let (_, hits) = render_to_string(DetailTab::Overview, 60, 20);

        // `render_detail`'s block border leaves `inner` at (1, 1, 58, 18)
        // for a 60x20 area (one cell of border on every side — confirmed
        // against the buffer dump in the task report). `render_tab_bar`
        // then reserves a 3-wide "[x]" close affordance plus a 1-wide gap
        // on the right before laying out tabs into what remains.
        let inner = Rect {
            x: 1,
            y: 1,
            width: 58,
            height: 18,
        };
        let close_width = 3u16;
        let gap = 1u16;
        let tabs_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width - close_width - gap,
            height: 1,
        };
        let expected = tab_spans(&["Overview", "YAML", "Events"], tabs_area, 2);
        assert_eq!(expected.len(), 3, "sanity: all three tabs must fit");

        for pair in expected.windows(2) {
            let divider_x = pair[0].x + pair[0].width;
            assert_eq!(
                hits.hit(divider_x, inner.y),
                None,
                "the divider column right after a tab (x={divider_x}) must not \
                 belong to the tab that follows it"
            );
        }
    }

    #[test]
    fn the_active_tab_is_visually_distinct() {
        let (a, _) = render_styles(DetailTab::Overview);
        let (b, _) = render_styles(DetailTab::Yaml);
        assert_ne!(a, b, "switching tabs changed nothing on screen");
    }

    #[test]
    fn the_pane_has_a_close_affordance() {
        let (_, hits) = render_to_string(DetailTab::Overview, 60, 20);
        let mut found = false;
        for y in 0..20u16 {
            for x in 0..60u16 {
                if matches!(hits.hit(x, y), Some(HitTarget::DetailClose)) {
                    found = true;
                }
            }
        }
        assert!(found, "no way to close the pane by mouse");
    }

    #[test]
    fn the_overview_tab_shows_the_active_objects_fields() {
        let (text, _) = render_to_string(DetailTab::Overview, 60, 20);
        assert!(
            text.contains("web-1"),
            "expected the object's name:\n{text}"
        );
        assert!(text.contains("node-7"), "expected the node row:\n{text}");
        assert!(text.contains("Running"), "expected the status row:\n{text}");
    }

    #[test]
    fn the_yaml_and_events_tabs_render_a_marked_placeholder_not_overview_content() {
        let (yaml_text, _) = render_to_string(DetailTab::Yaml, 60, 20);
        assert!(
            !yaml_text.contains("node-7"),
            "YAML tab must not silently show Overview content:\n{yaml_text}"
        );
        assert!(
            yaml_text.to_lowercase().contains("yaml"),
            "YAML placeholder must say what it is:\n{yaml_text}"
        );

        let (events_text, _) = render_to_string(DetailTab::Events, 60, 20);
        assert!(
            !events_text.contains("node-7"),
            "Events tab must not silently show Overview content:\n{events_text}"
        );
        assert!(
            events_text.to_lowercase().contains("events"),
            "Events placeholder must say what it is:\n{events_text}"
        );
    }

    #[test]
    fn a_zero_width_pane_does_not_panic() {
        let obj = pod_with_status();
        let mut pane = DetailPane::new();
        let mut term = Terminal::new(TestBackend::new(10, 10)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 10,
            };
            render_detail(f, area, &obj, &mut pane, &mut hits);
        })
        .unwrap();
    }

    #[test]
    fn a_zero_height_pane_does_not_panic() {
        let obj = pod_with_status();
        let mut pane = DetailPane::new();
        let mut term = Terminal::new(TestBackend::new(10, 10)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 10,
                height: 0,
            };
            render_detail(f, area, &obj, &mut pane, &mut hits);
        })
        .unwrap();
    }

    #[test]
    fn a_one_row_pane_does_not_panic_and_draws_only_the_tab_bar() {
        // Just tall enough for the border, leaving zero content rows: the
        // frame (tabs, close) must still render without a content panel.
        let obj = pod_with_status();
        let mut pane = DetailPane::new();
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits);
        })
        .unwrap();
    }

    /// Build an object with a custom annotation. Used to test multi-line
    /// annotation rendering.
    fn object_with_annotation(key: &str, value: &str) -> DynamicObject {
        let mut o = DynamicObject::new("test-obj", &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({
            "metadata": {
                "annotations": {
                    key: value
                }
            }
        });
        o
    }

    #[test]
    fn yaml_leads_with_apiversion_kind_and_metadata() {
        // kubectl's own convention. A serialiser that alphabetised the top
        // level would put `apiVersion` after nothing but still bury `kind`.
        let y = object_to_yaml(&pod_with_status());
        let lines: Vec<&str> = y.lines().collect();
        assert!(lines[0].starts_with("apiVersion:"), "first line was {:?}", lines[0]);
        assert!(lines.iter().any(|l| l.starts_with("kind:")));
        assert!(lines.iter().any(|l| l.starts_with("metadata:")));
    }

    #[test]
    fn yaml_renders_multiline_annotations_readably() {
        let obj = object_with_annotation("desc", "line one\nline two\nline three");
        let y = object_to_yaml(&obj);
        assert!(y.contains("line one"));
        assert!(y.contains("line three"));
        assert!(!y.contains("\\n"), "multi-line value was escaped rather than blocked:\n{y}");
    }

    #[test]
    fn scroll_clamps_to_the_document_and_never_underflows() {
        assert_eq!(clamp_scroll(0, 100, 20), 0);
        assert_eq!(clamp_scroll(50, 100, 20), 50);
        assert_eq!(clamp_scroll(200, 100, 20), 80, "cannot scroll past the last screenful");
        assert_eq!(clamp_scroll(10, 5, 20), 0, "a document shorter than the viewport does not scroll");
    }
}
