use crate::app::event::WatchStatus;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// A short label and colour per watch state. Freshness is always visible:
/// presenting stale data as live is the worst failure mode for an ops tool.
pub fn status_label(status: WatchStatus) -> (&'static str, Style) {
    match status {
        WatchStatus::Initialising => ("loading", Style::default().fg(theme::MUTED)),
        WatchStatus::Synced => ("live", Style::default().fg(theme::OK)),
        WatchStatus::Reconnecting => ("reconnecting", Style::default().fg(theme::WARN)),
        WatchStatus::Failed => ("failed", Style::default().fg(theme::ERR)),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_status(
    f: &mut Frame,
    area: Rect,
    context: &str,
    namespace: &str,
    status: WatchStatus,
    count: usize,
    error: Option<&str>,
    show_all_namespaces_hint: bool,
    hits: &mut HitRegistry,
) {
    let (label, style) = status_label(status);

    let mut spans = vec![
        Span::styled(format!(" {context} "), Style::default().fg(theme::HEADER)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(format!("{namespace} "), Style::default().fg(theme::FG)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(format!("{count} items "), Style::default().fg(theme::FG)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(label, style),
    ];

    if let Some(e) = error {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(e.to_string(), Style::default().fg(theme::ERR)));
    } else if show_all_namespaces_hint {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            "no pods here — try -A for all namespaces",
            Style::default().fg(theme::MUTED),
        ));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
    hits.push(area, 0, HitTarget::StatusBar);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(status: WatchStatus, count: usize, error: Option<&str>) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(
                f, area, "prod-eu", "payments", status, count, error, false, &mut hits,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    #[test]
    fn shows_context_and_namespace() {
        let text = render(WatchStatus::Synced, 42, None);
        assert!(text.contains("prod-eu"), "got: {text}");
        assert!(text.contains("payments"), "got: {text}");
    }

    #[test]
    fn shows_the_object_count() {
        let text = render(WatchStatus::Synced, 42, None);
        assert!(text.contains("42"), "got: {text}");
    }

    #[test]
    fn reconnecting_is_visible_so_stale_data_is_never_shown_as_live() {
        let text = render(WatchStatus::Reconnecting, 42, None);
        assert!(
            text.contains("reconnect"),
            "reconnect state must be visible; got: {text}"
        );
    }

    #[test]
    fn an_error_is_surfaced_rather_than_swallowed() {
        let text = render(WatchStatus::Failed, 0, Some("forbidden: pods is denied"));
        assert!(
            text.contains("forbidden"),
            "errors must reach the user; got: {text}"
        );
    }

    #[test]
    fn each_status_has_a_distinct_label() {
        let labels = [
            status_label(WatchStatus::Initialising).0,
            status_label(WatchStatus::Synced).0,
            status_label(WatchStatus::Reconnecting).0,
            status_label(WatchStatus::Failed).0,
        ];
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), 4, "statuses must be distinguishable");
    }

    #[test]
    fn shows_all_namespaces_when_watching_all() {
        // When watching all namespaces, the status bar should show "all namespaces"
        // This test verifies the display behavior when the namespace field is set to "all namespaces"
        let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(
                f,
                area,
                "prod-eu",
                "all namespaces",
                WatchStatus::Synced,
                42,
                None,
                false,
                &mut hits,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(text.contains("all namespaces"), "got: {text}");
    }

    #[test]
    fn shows_hint_when_fallback_namespace_is_empty() {
        // When watching default namespace (fallback) with zero items, show hint
        let mut term = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(
                f,
                area,
                "prod-eu",
                "default",
                WatchStatus::Synced,
                0,
                None,
                true,
                &mut hits,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..120).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            text.contains("try -A"),
            "hint should appear when default namespace is empty; got: {text}"
        );
    }

    #[test]
    fn hides_hint_when_namespace_has_items() {
        // No hint when there are items in the namespace, even if was_fallback
        let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(
                f,
                area,
                "prod-eu",
                "default",
                WatchStatus::Synced,
                5,
                None,
                false,
                &mut hits,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(
            !text.contains("try -A"),
            "hint should not appear when there are items; got: {text}"
        );
    }

    #[test]
    fn hides_hint_when_error_is_present() {
        // Error message takes precedence over hint
        let mut term = Terminal::new(TestBackend::new(120, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(
                f,
                area,
                "prod-eu",
                "default",
                WatchStatus::Failed,
                0,
                Some("forbidden"),
                true,
                &mut hits,
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..120).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(text.contains("forbidden"), "error should appear");
        assert!(
            !text.contains("try -A"),
            "hint should not appear when there's an error; got: {text}"
        );
    }
}
