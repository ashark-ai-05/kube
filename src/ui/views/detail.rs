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
//! YAML tab renders serialized kubectl-style YAML via `serde_norway` (Task 8).
//! Events tab renders `store::events::EventRow`s (Task 9): a scrollable list
//! when the fetch succeeded (even with zero rows — a healthy object with
//! nothing to report), or an explanation when it failed. The two must never
//! render the same way: an empty Events tab reads as "nothing wrong here,"
//! the most dangerous possible lie for the exact tab someone opens to find
//! out what's wrong.

use crate::store::columns::format_age;
use crate::store::events::EventRow;
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
///
/// The YAML cache stores the serialized YAML string keyed by `resourceVersion`
/// (from the object's metadata). When the object changes, the resourceVersion
/// changes, so the cache never goes stale. Serialization (especially in debug
/// builds with managedFields) is expensive; caching it prevents re-serializing
/// an unchanged object on every frame.
pub struct DetailPane {
    pub tab: DetailTab,
    pub yaml_scroll: u16,
    pub events_scroll: u16,
    yaml_cache: Option<(String, String)>, // (resourceVersion, serialized YAML)
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
            yaml_cache: None,
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
/// `events`/`events_error` are supplied by the caller, never fetched here:
/// this function stays synchronous and does no I/O, exactly like every
/// other render function in this module (`fetch_events` is a request, see
/// `store::events`). `events_error` takes priority over `events` being
/// empty — see the module doc comment for why the two must render
/// differently.
pub fn render_detail(
    f: &mut Frame,
    area: Rect,
    obj: &DynamicObject,
    pane: &mut DetailPane,
    hits: &mut HitRegistry,
    events: &[EventRow],
    events_error: Option<&str>,
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
        DetailTab::Yaml => render_yaml(f, content_area, obj, pane),
        DetailTab::Events => render_events(f, content_area, events, events_error, pane),
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

/// Render the YAML tab content: a scrollable view of the object serialized to YAML.
/// The scroll position is clamped to valid bounds to prevent scrolling past the end.
fn render_yaml(f: &mut Frame, area: Rect, obj: &DynamicObject, pane: &mut DetailPane) {
    use ratatui::widgets::Wrap;

    let yaml = get_or_cache_yaml(obj, pane);
    // `total_wrapped_rows`, not a `\n`-count: the Paragraph below wraps, so a
    // single long line (a base64 secret value, a long annotation) can occupy
    // several screen rows. Clamping on the newline count alone would make
    // that line's tail — and anything after it — unreachable by scrolling,
    // the same bug Finding 1 fixed for the Events tab. See
    // `wrapped_row_count`'s doc comment for why this is hand-rolled instead
    // of using `Paragraph::line_count`.
    let total_lines = total_wrapped_rows(yaml.lines(), area.width);

    // Clamp scroll to valid bounds: the viewport height is the render area height
    pane.yaml_scroll = clamp_scroll(pane.yaml_scroll, total_lines, area.height);

    let paragraph = Paragraph::new(yaml.clone())
        .wrap(Wrap { trim: false })
        .scroll((pane.yaml_scroll, 0));

    f.render_widget(paragraph, area);
}

/// Get cached YAML for an object, or compute and cache it if the object changed.
/// The cache is keyed on `resourceVersion` — if it changes, the object changed,
/// so the cache is invalid. This prevents re-serializing an unchanged object on
/// every frame.
fn get_or_cache_yaml(obj: &DynamicObject, pane: &mut DetailPane) -> String {
    let resource_version = obj.metadata.resource_version.as_deref().unwrap_or("");

    // Check cache: if resourceVersion matches, reuse it
    if let Some((cached_rv, cached_yaml)) = &pane.yaml_cache
        && cached_rv == resource_version
    {
        return cached_yaml.clone();
    }

    // Cache miss or stale: serialize and update cache
    let yaml = object_to_yaml(obj);
    pane.yaml_cache = Some((resource_version.to_string(), yaml.clone()));
    yaml
}

/// Render the Events tab: a scrollable list of the object's events
/// (pre-scoped and pre-formatted upstream by `store::events::fetch_events`/
/// `event_rows`), or an explanation when the fetch failed.
///
/// `events_error` is checked FIRST and unconditionally wins over `events`
/// being empty — see the module doc comment for why a forbidden listing
/// must never render like a healthy, uneventful object. Warnings render in
/// `theme::event_kind_style`'s signal colour, matching how the table already
/// colours failing pod phases (`theme::phase_style`), so a problem is
/// visible without reading every row.
fn render_events(
    f: &mut Frame,
    area: Rect,
    events: &[EventRow],
    events_error: Option<&str>,
    pane: &mut DetailPane,
) {
    use ratatui::widgets::Wrap;

    if let Some(err) = events_error {
        let paragraph = Paragraph::new(format!("Events unavailable: {err}"))
            .style(Style::default().fg(theme::CORAL))
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, area);
        return;
    }

    if events.is_empty() {
        f.render_widget(
            Paragraph::new("No events.").style(theme::muted_style()),
            area,
        );
        return;
    }

    let lines: Vec<Line> = events.iter().map(event_line).collect();
    // `events_wrapped_line_count`, not `events.len()`: `len()` is the ROW
    // count, but the rendered content is wrapped text — a handful of events
    // with long messages can occupy far more screen rows than there are
    // events. Clamping against the row count made later events permanently
    // unreachable at any scroll offset whenever the row count alone still
    // fit the viewport even though the true wrapped height did not (see
    // `events_whose_messages_wrap_can_still_all_be_reached`).
    let total_lines = events_wrapped_line_count(events, area.width);
    pane.events_scroll = clamp_scroll(pane.events_scroll, total_lines, area.height);

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((pane.events_scroll, 0));
    f.render_widget(paragraph, area);
}

/// One Events-tab display line: age, type and reason lead (mirroring
/// `kubectl describe`'s Events table), then the message. No OBJECT column —
/// every row already belongs to the one object this pane is open on — and
/// no FROM column, since `EventRow` (deliberately) does not carry the
/// reporting source.
fn event_line(row: &EventRow) -> Line<'static> {
    let style = theme::event_kind_style(&row.kind);
    Line::styled(event_line_text(row), style)
}

/// The plain text `event_line` renders, factored out so
/// `events_wrapped_line_count` measures the exact same string that ends up
/// on screen rather than a second, hand-maintained approximation of it.
fn event_line_text(row: &EventRow) -> String {
    let count = if row.count > 1 {
        format!(" (x{})", row.count)
    } else {
        String::new()
    };
    format!(
        "{age:<6} {kind:<7} {reason:<20} {message}{count}",
        age = row.age,
        kind = row.kind,
        reason = row.reason,
        message = row.message,
    )
}

/// Serialize a DynamicObject to YAML string using serde_norway.
/// The output leads with apiVersion, kind, metadata (kubectl convention) and
/// requires no post-processing for readability.
fn object_to_yaml(obj: &DynamicObject) -> String {
    serde_norway::to_string(obj).unwrap_or_else(|_| "Failed to serialize YAML".to_string())
}

/// Number of screen rows one logical (pre-wrap) line of `text` occupies once
/// wrapped to `width` columns under `Wrap { trim: false }` — the setting
/// both the YAML and Events tabs always render with.
///
/// This hand-rolls ratatui's own wrapper (`ratatui-widgets` 0.3.2's
/// `WordWrapper`, `src/reflow.rs`) closely enough to clamp scrolling safely:
/// greedy word-wrap on whitespace boundaries, and a single token wider than
/// `width` forced onto `ceil(token_width / width)` rows of its own — the
/// same direction of error a real wrapper produces — rather than silently
/// counted as one (undersized) row, which would recreate the exact
/// under-counting bug this function exists to fix.
///
/// `Paragraph::line_count` (ratatui 0.30's own exact wrapped-line count)
/// would be preferable to hand-rolling this, and was checked first: it
/// exists (`ratatui-widgets-0.3.2/src/paragraph.rs`), but is gated behind
/// the `unstable-rendered-line-info` Cargo feature, which is OFF by default
/// and not enabled by this project's `Cargo.toml` (`ratatui = "0.30"`, no
/// features listed) — turning it on would be a dependency-shape change this
/// task's brief forbids. The method is also explicitly marked
/// `#[instability::unstable(...)]` with the doc note "the design for text
/// wrapping is not stable and might affect this API," so it would be a bet
/// on a surface ratatui itself does not consider settled. Verified
/// empirically instead: a scratch harness with the feature enabled
/// (`/tmp/.../scratchpad/wraptest`, not part of this project) compared this
/// function's output against `Paragraph::line_count` across representative
/// texts (short/empty/whitespace-only lines, normal sentences, a token wider
/// than the viewport, width down to 1) — every case matched exactly.
///
/// Known divergence risk: this project's own value can still drift from
/// ratatui's real wrapping on inputs the scratch check didn't cover — wide
/// (double-width) or zero-width grapheme clusters, combining characters, or
/// future changes to ratatui's (self-described unstable) wrap algorithm.
/// That risk is acceptable here because this value only bounds a scroll
/// position; it never decides what glyphs are drawn.
fn wrapped_row_count(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    let width = u32::from(width);

    let mut rows: u32 = 1;
    let mut current: u32 = 0;
    for token in text.split_inclusive(char::is_whitespace) {
        let token_width = UnicodeWidthStr::width(token) as u32;
        if token_width == 0 {
            continue;
        }
        if token_width > width {
            // A single token wider than the viewport cannot fit on any row,
            // so it force-wraps across ceil(token_width / width) rows —
            // mirroring how a real word-wrapper breaks an overlong token
            // mid-token instead of letting it overflow.
            if current > 0 {
                rows += 1;
                current = 0;
            }
            rows += token_width.div_ceil(width) - 1;
            continue;
        }
        if current + token_width > width {
            rows += 1;
            current = token_width;
        } else {
            current += token_width;
        }
    }
    rows.try_into().unwrap_or(u16::MAX)
}

/// Sum of `wrapped_row_count` across every logical (pre-wrap) line — what a
/// wrapped `Paragraph`'s scroll clamp must bound against instead of a raw
/// line/row count. `lines` is whatever already-split logical lines the
/// caller has (YAML's `\n`-delimited lines; Events' one-line-per-event
/// text), since `Paragraph` wraps each of those independently.
fn total_wrapped_rows<'a>(lines: impl Iterator<Item = &'a str>, width: u16) -> u16 {
    let mut total: u32 = 0;
    for line in lines {
        total = total.saturating_add(u32::from(wrapped_row_count(line, width)));
    }
    total.try_into().unwrap_or(u16::MAX)
}

/// `total_wrapped_rows` specialised to `EventRow`s: each event is one
/// logical line (`event_line_text`), so this is what `render_events`'
/// scroll clamp bounds against — see that function's call site for why
/// `events.len()` (a row count) was wrong.
fn events_wrapped_line_count(events: &[EventRow], width: u16) -> u16 {
    let mut total: u32 = 0;
    for row in events {
        total = total.saturating_add(u32::from(wrapped_row_count(&event_line_text(row), width)));
    }
    total.try_into().unwrap_or(u16::MAX)
}

/// Clamp a scroll position to valid bounds for a document.
/// Returns a scroll value that, when used with Paragraph::scroll((y, x)),
/// ensures the viewport never scrolls past the end of the document.
///
/// If total_lines <= viewport, returns 0 (document fits entirely).
/// Otherwise, clamps scroll to the range [0, total_lines - viewport].
fn clamp_scroll(scroll: u16, total_lines: u16, viewport: u16) -> u16 {
    if total_lines <= viewport {
        0
    } else {
        scroll.min(total_lines - viewport)
    }
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
        render_to_string_with_events(active, w, h, &[], None)
    }

    /// As `render_to_string`, but with the Events tab's data under caller
    /// control — needed by every test that checks how `events`/
    /// `events_error` actually render, rather than always exercising the
    /// "nothing fetched yet" default.
    fn render_to_string_with_events(
        active: DetailTab,
        w: u16,
        h: u16,
        events: &[EventRow],
        events_error: Option<&str>,
    ) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: active,
            yaml_scroll: 0,
            events_scroll: 0,
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, events, events_error);
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
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, &[], None);
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
    fn the_yaml_tab_shows_serialized_yaml_not_overview_content() {
        let (yaml_text, _) = render_to_string(DetailTab::Yaml, 60, 20);
        // The YAML tab shows actual serialized YAML (not a placeholder), so
        // it should contain kubectl-style content with proper YAML structure.
        assert!(
            yaml_text.contains("apiVersion:"),
            "YAML tab must show serialized YAML content:\n{yaml_text}"
        );
        assert!(
            yaml_text.contains("kind:"),
            "YAML tab must show kind field:\n{yaml_text}"
        );
        // The key assertion: YAML tab must NOT show the Overview layout
        // (Overview shows "Name" / "Namespace" / "Node" / "Status" rows with aligned labels).
        assert!(
            !yaml_text.contains("Name      "),
            "YAML tab must not show Overview-formatted field labels:\n{yaml_text}"
        );
    }

    // --- Events tab ---

    fn normal_row() -> EventRow {
        EventRow {
            kind: "Normal".to_string(),
            reason: "Scheduled".to_string(),
            message: "pod scheduled onto node-7".to_string(),
            age: "5m".to_string(),
            count: 1,
        }
    }

    fn warning_row() -> EventRow {
        EventRow {
            kind: "Warning".to_string(),
            reason: "BackOff".to_string(),
            message: "back-off restarting failed container".to_string(),
            age: "1m".to_string(),
            count: 5,
        }
    }

    #[test]
    fn the_events_tab_shows_reason_message_and_repeat_count() {
        let (text, _) =
            render_to_string_with_events(DetailTab::Events, 60, 20, &[warning_row()], None);
        assert!(text.contains("BackOff"), "expected the reason:\n{text}");
        assert!(
            text.contains("back-off restarting"),
            "expected the message:\n{text}"
        );
        assert!(
            text.contains("x5"),
            "a repeating event's count must be visible:\n{text}"
        );
    }

    #[test]
    fn an_empty_but_permitted_events_list_does_not_claim_to_be_forbidden() {
        // A healthy object legitimately has zero events. This must read as
        // "nothing to report," never as an error.
        let (text, _) = render_to_string_with_events(DetailTab::Events, 60, 20, &[], None);
        assert!(
            !text.to_lowercase().contains("forbidden"),
            "a healthy empty state must not scare the user:\n{text}"
        );
        assert!(
            text.to_lowercase().contains("events"),
            "the empty state should still say what it is:\n{text}"
        );
    }

    #[test]
    fn a_forbidden_events_fetch_renders_an_explanation_not_an_empty_list() {
        // Mutation check: a wrong implementation that ignores events_error
        // and renders based solely on events.is_empty() must fail this.
        let (text, _) = render_to_string_with_events(
            DetailTab::Events,
            60,
            20,
            &[],
            Some("events forbidden for web-1: pods is forbidden"),
        );
        assert!(
            text.to_lowercase().contains("forbidden"),
            "a forbidden listing must say so, not render as an empty list:\n{text}"
        );
    }

    #[test]
    fn events_error_wins_even_when_stale_events_are_also_passed() {
        // A refresh that failed after a previous successful fetch must not
        // silently keep showing the stale rows as if nothing were wrong.
        let (text, _) = render_to_string_with_events(
            DetailTab::Events,
            60,
            20,
            &[normal_row()],
            Some("events forbidden for web-1: pods is forbidden"),
        );
        assert!(
            text.to_lowercase().contains("forbidden"),
            "an error must win over stale rows:\n{text}"
        );
    }

    #[test]
    fn warning_events_are_visually_distinct_from_normal_events() {
        let rows = vec![normal_row(), warning_row()];
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: DetailTab::Events,
            yaml_scroll: 0,
            events_scroll: 0,
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, &rows, None);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();

        let mut text = String::new();
        for y in 0..20u16 {
            for x in 0..60u16 {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        let lines: Vec<&str> = text.lines().collect();
        let normal_y = lines
            .iter()
            .position(|l| l.contains("Scheduled"))
            .expect("normal row drawn") as u16;
        let warning_y = lines
            .iter()
            .position(|l| l.contains("BackOff"))
            .expect("warning row drawn") as u16;
        let normal_x = lines[normal_y as usize].find("Scheduled").unwrap() as u16;
        let warning_x = lines[warning_y as usize].find("BackOff").unwrap() as u16;

        let normal_style = buf[(normal_x, normal_y)].style();
        let warning_style = buf[(warning_x, warning_y)].style();
        assert_ne!(
            normal_style.fg, warning_style.fg,
            "warning events must look different from normal ones"
        );
    }

    #[test]
    fn events_scroll_is_clamped_to_the_actual_content_length() {
        // Reuses `clamp_scroll` — must not write a second scroll clamp.
        let rows: Vec<EventRow> = (0..50)
            .map(|i| EventRow {
                kind: "Normal".to_string(),
                reason: format!("Reason{i}"),
                message: "m".to_string(),
                age: "1m".to_string(),
                count: 1,
            })
            .collect();
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: DetailTab::Events,
            yaml_scroll: 0,
            events_scroll: 9999,
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, &rows, None);
        })
        .unwrap();
        assert!(
            pane.events_scroll < 9999,
            "scroll must be clamped to the actual content length, got {}",
            pane.events_scroll
        );
    }

    /// Render the Events tab with `events_scroll` set far past any
    /// reasonable bound and return the rendered buffer as text. Exercises
    /// `clamp_scroll` itself (a huge scroll value must get pulled back to
    /// the true last screenful) rather than bypassing it — a test that set
    /// `events_scroll` directly to some pre-computed "correct" value could
    /// pass by construction even if the clamp were wrong.
    fn render_events_scrolled_to_end(events: &[EventRow], w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        let obj = pod_with_status();
        let mut pane = DetailPane {
            tab: DetailTab::Events,
            yaml_scroll: 0,
            events_scroll: 9999,
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, events, None);
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
        text
    }

    #[test]
    fn events_whose_messages_wrap_can_still_all_be_reached() {
        // Clamping on the ROW count rather than the WRAPPED LINE count makes
        // later events unreachable at any scroll offset: if the row count
        // alone fits the viewport, the clamp returns 0 even though the real
        // (wrapped) content is much taller. Whole events vanish from the one
        // tab that explains why a pod will not start.
        //
        // 10-word messages, not the ~40-word message a first draft of this
        // test used: at 40 words a single event's own wrapped block is
        // taller than the 7-row viewport, so no scroll position — not even
        // a perfectly correct one — can show a *later* event's reason label
        // at the same time as scrolling to the very end (confirmed against
        // ratatui's own `Paragraph::line_count` via a throwaway harness
        // before picking these numbers). 10 words keeps each event's block
        // shorter than the viewport, so the last event's reason line is
        // fully within the final screenful once the clamp is correct.
        let events: Vec<EventRow> = (0..3)
            .map(|i| EventRow {
                kind: "Warning".to_string(),
                reason: format!("Reason{i}"),
                message: "word ".repeat(10),
                age: "1m".to_string(),
                count: 1,
            })
            .collect();
        let text = render_events_scrolled_to_end(&events, 30, 10);
        assert!(
            text.contains("Reason2"),
            "the last event could not be reached by scrolling:\n{text}"
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
            render_detail(f, area, &obj, &mut pane, &mut hits, &[], None);
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
            render_detail(f, area, &obj, &mut pane, &mut hits, &[], None);
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
            render_detail(f, area, &obj, &mut pane, &mut hits, &[], None);
        })
        .unwrap();
    }

    /// Build an object with a custom annotation. Used to test multi-line
    /// annotation rendering.
    /// Sets annotations on the typed metadata, leaving only spec/status in data
    /// to match the shape of real apiserver responses (no duplicate metadata keys).
    fn object_with_annotation(key: &str, value: &str) -> DynamicObject {
        let mut o =
            DynamicObject::new("test-obj", &ApiResource::erase::<Pod>(&())).within("default");
        // Add annotation to the typed ObjectMeta field (BTreeMap)
        let mut annotations = std::collections::BTreeMap::new();
        annotations.insert(key.to_string(), value.to_string());
        o.metadata.annotations = Some(annotations);
        // Keep only spec/status in data to avoid duplicate metadata key in YAML
        o.data = serde_json::json!({
            "spec": {},
            "status": {}
        });
        o
    }

    #[test]
    fn yaml_leads_with_apiversion_kind_and_metadata() {
        // kubectl's own convention: apiVersion, kind, metadata lead.
        // This test uses a realistic Pod which happens to have alphabetically-ordered
        // top-level keys (apiVersion, kind, metadata, spec, status), so it cannot
        // distinguish "declaration order" from "full alphabetisation". See the separate
        // key-order test below for the discriminating case.
        let y = object_to_yaml(&pod_with_status());
        let lines: Vec<&str> = y.lines().collect();
        assert!(
            lines[0].starts_with("apiVersion:"),
            "first line was {:?}",
            lines[0]
        );
        assert!(lines.iter().any(|l| l.starts_with("kind:")));
        assert!(lines.iter().any(|l| l.starts_with("metadata:")));
    }

    #[test]
    fn the_document_leads_with_apiversion_rather_than_sorting_it_among_the_rest() {
        // Deliberately artificial: real Kubernetes objects happen to have
        // top-level keys that are already alphabetical (apiVersion, kind,
        // metadata, spec, status), so no realistic fixture can distinguish
        // "apiVersion first" from "everything alphabetised". The `aaa` key
        // sorts before `apiVersion` and makes the difference observable, so a
        // future serde_norway change that started sorting the top level would
        // fail here rather than silently reordering every object we display.
        let mut obj = DynamicObject::new("x", &ApiResource::erase::<Pod>(&())).within("default");
        obj.data = serde_json::json!({ "aaa": 1, "spec": { "nodeName": "n1" } });
        let y = object_to_yaml(&obj);
        let lines: Vec<&str> = y.lines().collect();
        assert!(lines[0].starts_with("apiVersion:"), "got: {:?}", lines[0]);
        assert!(
            lines.iter().position(|l| l.starts_with("aaa:"))
                > lines.iter().position(|l| l.starts_with("kind:")),
            "`aaa` sorted ahead of the leading keys, so the top level is being alphabetised:\n{y}"
        );
    }

    #[test]
    fn yaml_renders_multiline_annotations_readably() {
        let obj = object_with_annotation("desc", "line one\nline two\nline three");
        let y = object_to_yaml(&obj);
        assert!(y.contains("line one"));
        assert!(y.contains("line three"));
        assert!(
            !y.contains("\\n"),
            "multi-line value was escaped rather than blocked:\n{y}"
        );
        // Catch invalid YAML: duplicate top-level metadata key would indicate
        // the fixture is not shaped like a real object (apiserver never does this).
        assert_eq!(
            y.lines().filter(|l| l.starts_with("metadata:")).count(),
            1,
            "duplicate top-level metadata key — the fixture is not shaped like a real object:\n{y}"
        );
    }

    #[test]
    fn a_long_unbroken_yaml_value_does_not_hide_the_rest_of_the_document() {
        // A single very long value with no whitespace to wrap on (a base64
        // secret, an unbroken annotation) is ONE `\n`-delimited line in the
        // source text but many WRAPPED screen rows once rendered — the same
        // gap between "row count" and "wrapped row count" Finding 1 fixed
        // for the Events tab. `yaml_line_count`-style clamping (counting
        // `\n`s) would under-clamp the scroll and make the tail of the
        // document — everything serialized after this value — unreachable.
        //
        // Confirmed empirically (scratch harness against
        // `Paragraph::line_count`, not guessed): this document's 9
        // `\n`-delimited lines wrap to 24 real screen rows at width 28, so a
        // `\n`-count-based clamp caps scrolling at `9-7=2`, far short of the
        // `24-7=17` actually needed to reach the final "status:" line.
        let obj = object_with_annotation("blob", &"x".repeat(400));
        let mut term = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let mut hits = HitRegistry::new();
        let mut pane = DetailPane {
            tab: DetailTab::Yaml,
            yaml_scroll: 9999,
            events_scroll: 0,
            yaml_cache: None,
        };
        term.draw(|f| {
            let area = f.area();
            render_detail(f, area, &obj, &mut pane, &mut hits, &[], None);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..10u16 {
            for x in 0..30u16 {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("status:"),
            "the tail of the document (after the long value) could not be reached by scrolling:\n{text}"
        );
    }

    #[test]
    fn scroll_clamps_to_the_document_and_never_underflows() {
        assert_eq!(clamp_scroll(0, 100, 20), 0);
        assert_eq!(clamp_scroll(50, 100, 20), 50);
        assert_eq!(
            clamp_scroll(200, 100, 20),
            80,
            "cannot scroll past the last screenful"
        );
        assert_eq!(
            clamp_scroll(10, 5, 20),
            0,
            "a document shorter than the viewport does not scroll"
        );
    }

    #[test]
    fn yaml_cache_is_reused_for_unchanged_objects() {
        // Verify that the YAML cache prevents re-serialization for unchanged objects.
        // We create an object, serialize it once (populating the cache), then
        // verify that a second call reuses the cached value. The cache is keyed
        // on resourceVersion, so an unchanged object with the same resourceVersion
        // must return the same YAML without re-serializing.
        let mut pane = DetailPane::new();
        let obj = pod_with_status();

        // First call: populates cache
        let yaml1 = get_or_cache_yaml(&obj, &mut pane);
        let cache_after_first = pane.yaml_cache.clone();

        // Second call with identical object: should reuse cache
        let yaml2 = get_or_cache_yaml(&obj, &mut pane);

        // Verify: same YAML, and cache was not recomputed
        assert_eq!(yaml1, yaml2, "YAML must be identical for unchanged object");
        assert_eq!(
            pane.yaml_cache, cache_after_first,
            "cache must not be recomputed for unchanged object"
        );
    }

    #[test]
    fn yaml_cache_invalidates_when_resourceversion_changes() {
        // Verify that the YAML cache invalidates when the object's resourceVersion changes.
        let mut pane = DetailPane::new();
        let mut obj1 = pod_with_status();
        obj1.metadata.resource_version = Some("rv1".to_string());

        // Populate cache with first object
        let yaml1 = get_or_cache_yaml(&obj1, &mut pane);
        let cache_after_first = pane.yaml_cache.clone();

        // Create a new object with different resourceVersion
        let mut obj2 = pod_with_status();
        obj2.metadata.resource_version = Some("rv2".to_string());

        // Get YAML for second object: should recompute, not use stale cache
        let yaml2 = get_or_cache_yaml(&obj2, &mut pane);

        // Verify: YAML differs (because resourceVersion is serialized),
        // and cache was updated to reflect the new resourceVersion
        assert_ne!(
            yaml1, yaml2,
            "YAML must differ when resourceVersion changes"
        );
        assert_ne!(
            pane.yaml_cache, cache_after_first,
            "cache must be recomputed for changed object"
        );
        assert_eq!(
            pane.yaml_cache.as_ref().map(|(rv, _)| rv.as_str()),
            Some("rv2"),
            "cache must be updated to new resourceVersion"
        );
    }
}
