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
        WatchStatus::Initialising => ("loading", Style::default().fg(theme::MIST)),
        WatchStatus::Synced => ("live", Style::default().fg(theme::VIRIDIAN)),
        WatchStatus::Reconnecting => ("reconnecting", Style::default().fg(theme::AMBER)),
        WatchStatus::Failed => ("failed", Style::default().fg(theme::CORAL)),
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
    // Name of the cluster a switch is connecting to, if any — the registry
    // is the only source of truth for this, so it must come from a scan of
    // `ConnectionState::Connecting` entries, not from `context`.
    //
    // While a connect is in flight the active cluster is still the OLD one
    // (teardown only happens on success), so `context` alone shows no sign
    // of the attempt in progress — this is what makes that visible.
    connecting: Option<&str>,
    hits: &mut HitRegistry,
) {
    let (label, style) = status_label(status);

    let mut spans = vec![
        Span::styled(
            format!(" {context} "),
            Style::default().fg(theme::cluster_hue(context)),
        ),
        Span::styled("· ", Style::default().fg(theme::MIST)),
        Span::styled(format!("{namespace} "), Style::default().fg(theme::PAPER)),
        Span::styled("· ", Style::default().fg(theme::MIST)),
        Span::styled(format!("{count} items "), Style::default().fg(theme::PAPER)),
        Span::styled("· ", Style::default().fg(theme::MIST)),
        Span::styled(label, style),
    ];

    if let Some(name) = connecting {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            format!("connecting to {name}…"),
            Style::default().fg(theme::AMBER),
        ));
    }

    if let Some(e) = error {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            e.to_string(),
            Style::default().fg(theme::CORAL),
        ));
    } else if show_all_namespaces_hint {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            "no pods here — try -A for all namespaces",
            Style::default().fg(theme::MIST),
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
                f, area, "prod-eu", "payments", status, count, error, false, None, &mut hits,
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
                None,
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
                None,
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
                None,
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
                None,
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

    // --- Task 9: cluster hue in the status bar and "connecting" visibility ---

    #[test]
    fn the_cluster_name_renders_in_its_own_hue_not_a_fixed_chrome_colour() {
        let text_style_at = |context: &str| {
            let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
            let mut hits = HitRegistry::new();
            term.draw(|f| {
                let area = f.area();
                render_status(
                    f,
                    area,
                    context,
                    "payments",
                    WatchStatus::Synced,
                    1,
                    None,
                    false,
                    None,
                    &mut hits,
                );
            })
            .unwrap();
            term.backend().buffer()[(0, 0)].style().fg
        };
        assert_eq!(
            text_style_at("prod-eu"),
            Some(theme::cluster_hue("prod-eu")),
            "the context name must render in that cluster's own hue"
        );
        assert_ne!(
            text_style_at("prod-eu"),
            Some(theme::PERIWINKLE),
            "PERIWINKLE is chrome, not a cluster hue — the old fixed colour must be gone"
        );
    }

    #[test]
    fn two_different_clusters_show_different_name_colours() {
        let fg_for = |context: &str| {
            let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
            let mut hits = HitRegistry::new();
            term.draw(|f| {
                let area = f.area();
                render_status(
                    f,
                    area,
                    context,
                    "payments",
                    WatchStatus::Synced,
                    1,
                    None,
                    false,
                    None,
                    &mut hits,
                );
            })
            .unwrap();
            term.backend().buffer()[(0, 0)].style().fg
        };
        assert_ne!(fg_for("prod-eu"), fg_for("staging"));
    }

    #[test]
    fn a_connecting_switch_is_visible_even_though_the_active_cluster_is_still_the_old_one() {
        // Task 8's hazard 2: while a connect is in flight the active cluster
        // is still the OLD one, so a status bar driven only by `context` would
        // show no sign of the attempt. The caller is expected to scan the
        // registry for a `Connecting` entry and pass its name here.
        let text = {
            let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
            let mut hits = HitRegistry::new();
            term.draw(|f| {
                let area = f.area();
                render_status(
                    f,
                    area,
                    "prod-eu", // still the OLD active cluster
                    "payments",
                    WatchStatus::Synced,
                    1,
                    None,
                    false,
                    Some("dev"), // switching TO dev
                    &mut hits,
                );
            })
            .unwrap();
            let buf = term.backend().buffer();
            (0..80)
                .map(|x| buf[(x, 0)].symbol().to_string())
                .collect::<String>()
        };
        assert!(
            text.contains("connecting") && text.contains("dev"),
            "connecting state must be visible; got: {text}"
        );
    }

    #[test]
    fn with_no_switch_in_progress_no_connecting_text_appears() {
        let text = render(WatchStatus::Synced, 42, None);
        assert!(
            !text.contains("connecting"),
            "must not claim a connection is in progress when none is; got: {text}"
        );
    }
}
