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
                f, area, "prod-eu", "payments", status, count, error, &mut hits,
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
}
