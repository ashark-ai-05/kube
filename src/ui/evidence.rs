use crate::{
    dashboard::{evidence, pod::Identity},
    ui::{theme, workspace::Workspace},
};
use kube::ResourceExt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};

pub fn render(f: &mut Frame, area: Rect, workspace: &mut Workspace) {
    let dashboard = &mut workspace.dashboard;
    let Some(row) = dashboard.rows.get(dashboard.selected) else {
        f.render_widget(
            Paragraph::new("No selected pod · Esc returns to Pod monitor")
                .style(theme::muted_style()),
            area,
        );
        return;
    };
    let previous = dashboard
        .findings
        .get(dashboard.evidence_selected)
        .map(|finding| {
            (
                finding.source.clone(),
                finding.timestamp.clone(),
                finding.title.clone(),
            )
        });
    let findings = evidence::collect(row, &dashboard.events);
    dashboard.evidence_selected = previous
        .and_then(|key| {
            findings
                .iter()
                .position(|f| (f.source.clone(), f.timestamp.clone(), f.title.clone()) == key)
        })
        .unwrap_or(dashboard.evidence_selected)
        .min(findings.len().saturating_sub(1));
    dashboard.findings = findings;
    let regions = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Length(2),
    ])
    .spacing(1)
    .split(area);
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!(
                    "TROUBLESHOOT  {}/{}",
                    row.object.namespace().unwrap_or_default(),
                    row.object.name_any()
                ),
                theme::header_style(),
            ),
            Line::styled(
                "Observed Kubernetes evidence · warnings first · current selected pod",
                theme::muted_style(),
            ),
        ]),
        regions[0],
    );
    let panes = if area.width >= 95 {
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
            .spacing(2)
            .split(regions[1])
    } else {
        Layout::vertical([
            Constraint::Length((regions[1].height / 3).max(3)),
            Constraint::Min(3),
        ])
        .spacing(1)
        .split(regions[1])
    };
    let available = panes[0].height.saturating_sub(2).max(2) as usize / 2;
    let start = dashboard
        .evidence_selected
        .saturating_sub(available.saturating_sub(1));
    let rows: Vec<_> = dashboard
        .findings
        .iter()
        .enumerate()
        .skip(start)
        .take(available)
        .map(|(i, finding)| {
            let style = if finding.warning {
                Style::default().fg(theme::AMBER)
            } else {
                theme::muted_style()
            };
            workspace.buttons.push((
                Rect::new(
                    panes[0].x,
                    panes[0].y + 1 + ((i - start) * 2) as u16,
                    panes[0].width,
                    2,
                ),
                format!("evidence-select {i}"),
            ));
            Row::new([Cell::from(vec![
                Line::styled(&finding.title, style),
                Line::styled(
                    finding.timestamp.as_deref().unwrap_or("time not recorded"),
                    theme::muted_style(),
                ),
            ])])
            .height(2)
        })
        .collect();
    let mut state = TableState::default().with_selected(Some(dashboard.evidence_selected - start));
    f.render_stateful_widget(
        Table::new(rows, [Constraint::Fill(1)])
            .block(
                Block::bordered()
                    .title(format!(
                        " Evidence {} / {} ",
                        dashboard.evidence_selected + 1,
                        dashboard.findings.len()
                    ))
                    .border_style(theme::border_style()),
            )
            .row_highlight_style(theme::text_style().bg(theme::SURFACE))
            .highlight_symbol("› "),
        panes[0],
        &mut state,
    );
    if let Some(finding) = dashboard.findings.get(dashboard.evidence_selected) {
        let mut lines = vec![
            Line::styled(&finding.title, theme::label_style()),
            Line::default(),
            Line::from(vec![
                Span::styled("Source: ", theme::muted_style()),
                Span::raw(&finding.source),
            ]),
            Line::from(vec![
                Span::styled("Recorded: ", theme::muted_style()),
                Span::raw(
                    finding
                        .timestamp
                        .as_deref()
                        .unwrap_or("not supplied by Kubernetes"),
                ),
            ]),
            Line::default(),
        ];
        lines.extend(
            finding
                .detail
                .lines()
                .map(|line| Line::raw(line.to_string())),
        );
        lines.push(Line::default());
        lines.push(Line::styled(
            if let Some(target) = &finding.logs {
                format!(
                    "Enter: {} logs for {} (if retained)",
                    if target.previous {
                        "previous instance"
                    } else {
                        "current instance"
                    },
                    target.container
                )
            } else {
                "Enter: full event history for this pod".into()
            },
            theme::label_style(),
        ));
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Details · PgUp/PgDn scroll ")
            .border_style(theme::border_style());
        let inner = block.inner(panes[1]);
        let text: Vec<_> = lines.iter().map(ToString::to_string).collect();
        let total = crate::ui::views::detail::total_wrapped_rows(
            text.iter().map(String::as_str),
            inner.width,
        );
        let paragraph = Paragraph::new(lines)
            .style(theme::text_style())
            .wrap(Wrap { trim: false });
        dashboard.evidence_scroll = dashboard
            .evidence_scroll
            .min(total.saturating_sub(inner.height));
        f.render_widget(
            paragraph
                .block(block)
                .scroll((dashboard.evidence_scroll, 0)),
            panes[1],
        );
    }
    f.render_widget(
        Paragraph::new(dashboard.events.note_for(&Identity::of(&row.object)))
            .style(theme::muted_style()),
        regions[2],
    );
    let controls = [
        ("Esc Back", "troubleshoot-close"),
        ("Enter Open", "evidence-open"),
        ("l Logs", "logs"),
        ("e Events", "events"),
    ];
    let mut x = regions[3].x;
    for (label, command) in controls {
        let width = label.len() as u16 + 2;
        if x + width > regions[3].right() {
            break;
        }
        let rect = Rect::new(x, regions[3].y, width, 1);
        f.render_widget(
            Paragraph::new(label).style(theme::label_style().bg(theme::SURFACE)),
            rect,
        );
        workspace.buttons.push((rect, command.into()));
        x += width + 1;
    }
    f.render_widget(
        Paragraph::new("↑↓ select evidence · observations can describe resolved conditions")
            .style(theme::muted_style()),
        Rect::new(regions[3].x, regions[3].y + 1, regions[3].width, 1),
    );
}
