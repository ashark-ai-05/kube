use crate::{
    app::event::WatchStatus,
    dashboard::{GroupSummary, metrics::Usage},
    ui::{
        log_view::ellipsis,
        theme,
        workspace::{Health, Workspace, health, reason},
    },
};
use kube::{ResourceExt, api::DynamicObject};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::{collections::BTreeMap, sync::Arc};

pub fn render(
    f: &mut Frame,
    area: Rect,
    objects: &[Arc<DynamicObject>],
    status: WatchStatus,
    workspace: &mut Workspace,
) {
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(Style::default().bg(theme::INK)),
        area,
    );
    let inner = Rect::new(
        area.x.saturating_add(2),
        area.y.saturating_add(1),
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    let compact = inner.height < 25;
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(if compact { 2 } else { 3 }),
        Constraint::Length(if compact { 3 } else { 5 }),
        Constraint::Fill(1),
        Constraint::Length(if compact { 2 } else { 3 }),
    ])
    .spacing(if compact { 0 } else { 1 })
    .split(inner);
    let dash = &mut workspace.dashboard;
    let previous = dash.groups.get(dash.selected).map(|g| g.key.clone());
    let mut groups = BTreeMap::new();
    let mut counts = [0usize; 4];
    let mut issues = vec![];
    let mut total = Usage::default();
    let mut measured = 0;
    for (index, pod) in objects.iter().enumerate() {
        let value = health(pod);
        counts[match value {
            Health::Ready => 0,
            Health::Attention => 1,
            Health::Completed => 2,
            Health::Unknown => 3,
        }] += 1;
        if value == Health::Attention {
            issues.push((index, pod));
        }
        let key = dash.config.key(pod);
        let group = groups
            .entry(key.clone())
            .or_insert_with(|| GroupSummary::new(key));
        group.pods += 1;
        group.ready += usize::from(value == Health::Ready);
        group.issues += usize::from(value == Health::Attention);
        for field in ["containerStatuses", "initContainerStatuses"] {
            if let Some(containers) = pod.data["status"][field].as_array() {
                group.restarts = group.restarts.saturating_add(
                    containers
                        .iter()
                        .filter_map(|c| c["restartCount"].as_u64())
                        .fold(0u64, u64::saturating_add),
                );
            }
        }
        if let Some(node) = pod.data["spec"]["nodeName"].as_str() {
            group.nodes.insert(node.into());
        }
        if let Some(usage) = dash.metrics.usage(pod) {
            group.usage.add(usage);
            group.measured += 1;
            total.add(usage);
            measured += 1;
        }
    }
    dash.groups = groups.into_values().collect();
    dash.selected = previous
        .and_then(|key| dash.groups.iter().position(|g| g.key == key))
        .unwrap_or(dash.selected)
        .min(dash.groups.len().saturating_sub(1));
    if dash.sampled_revision != dash.metrics.revision {
        dash.history.push_back((measured > 0).then_some(total));
        while dash.history.len() > 60 {
            dash.history.pop_front();
        }
        dash.sampled_revision = dash.metrics.revision;
    }
    f.render_widget(
        Paragraph::new(vec![
            Line::styled("Fleet radar", theme::header_style()),
            Line::styled(
                format!(
                    "{} pod groups · {} · {}",
                    dash.groups.len(),
                    if dash.config.has_rules() {
                        "config → labels → owner"
                    } else {
                        "app labels → workload owner"
                    },
                    if status == WatchStatus::Synced {
                        "live watch"
                    } else {
                        "watch incomplete"
                    }
                ),
                theme::muted_style(),
            ),
        ]),
        rows[0],
    );
    let cards = Layout::horizontal([Constraint::Fill(1); 4])
        .spacing(1)
        .split(rows[1]);
    for (i, (label, color)) in [
        ("READY", theme::TEAL),
        ("ATTENTION", theme::CORAL),
        ("COMPLETED", theme::MIST),
        ("UNKNOWN", theme::AMBER),
    ]
    .iter()
    .enumerate()
    {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" {} ", counts[i]),
                    Style::default().fg(*color).bold(),
                ),
                Span::styled(
                    if cards[i].width < 15 && i == 1 {
                        "ATTN"
                    } else {
                        *label
                    },
                    theme::muted_style(),
                ),
            ]))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(*color))
                    .style(Style::default().bg(theme::ABYSS)),
            ),
            cards[i],
        );
    }
    let charts = Layout::horizontal([Constraint::Fill(1); 2])
        .spacing(2)
        .split(rows[2]);
    for (i, label) in ["CPU · millicores", "MEMORY · MiB"].iter().enumerate() {
        let color = if i == 0 { theme::TEAL } else { theme::VIOLET };
        let value = if measured == 0 {
            "Unavailable".into()
        } else if i == 0 {
            format!("{:.1} mCPU", total.cpu_milli)
        } else {
            format!("{:.1} MiB", total.memory_bytes / 1048576.)
        };
        let values: Vec<_> = dash
            .history
            .iter()
            .map(|p| {
                p.map(|u| {
                    if i == 0 {
                        u.cpu_milli
                    } else {
                        u.memory_bytes / 1048576.
                    }
                })
            })
            .collect();
        let note = if measured > 0 {
            format!(
                "{measured}/{} pods measured · recent samples",
                objects.len()
            )
        } else {
            dash.metrics
                .error
                .clone()
                .unwrap_or_else(|| "Waiting for metrics-server samples…".into())
        };
        let mut lines = vec![Line::styled(
            format!(" {value}"),
            Style::default().fg(color).bold(),
        )];
        if charts[i].height >= 4 {
            lines.push(Line::styled(
                trend(&values, charts[i].width.saturating_sub(2) as usize),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::styled(
            format!(
                " {}",
                ellipsis(&note, charts[i].width.saturating_sub(2) as usize)
            ),
            theme::muted_style(),
        ));
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .title(format!(" {label} "))
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(color))
                    .style(Style::default().bg(theme::ABYSS)),
            ),
            charts[i],
        );
    }
    let wide = rows[3].width >= 100;
    let columns = Layout::horizontal(if wide {
        vec![Constraint::Percentage(66), Constraint::Percentage(34)]
    } else if dash.issues_focus {
        vec![Constraint::Length(0), Constraint::Fill(1)]
    } else {
        vec![Constraint::Fill(1), Constraint::Length(0)]
    })
    .spacing(if wide { 2 } else { 0 })
    .split(rows[3]);
    if columns[0].width > 0 {
        let mut lines = vec![Line::styled(
            "POD GROUPS · g",
            if !dash.issues_focus {
                theme::label_style()
            } else {
                theme::muted_style()
            },
        )];
        workspace.buttons.push((
            Rect {
                height: 1,
                ..columns[0]
            },
            "focus-groups".into(),
        ));
        let count = (columns[0].height.saturating_sub(1) / 3).max(1) as usize;
        let start = dash.selected.saturating_sub(count - 1);
        for (i, g) in dash.groups.iter().enumerate().skip(start).take(count) {
            let y = columns[0].y + lines.len() as u16;
            let width = columns[0].width as usize;
            let name = ellipsis(&g.key.name, width.saturating_sub(21));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "{} {name}",
                        if i == dash.selected && !dash.issues_focus {
                            "›"
                        } else {
                            " "
                        }
                    ),
                    if i == dash.selected && !dash.issues_focus {
                        theme::text_style().bg(theme::SURFACE)
                    } else {
                        theme::text_style()
                    },
                ),
                Span::styled(
                    format!("  {}/{} ready", g.ready, g.pods),
                    if g.issues > 0 {
                        Style::default().fg(theme::AMBER)
                    } else {
                        theme::label_style()
                    },
                ),
            ]));
            let usage = if g.measured == 0 {
                "CPU — · RAM —".into()
            } else {
                format!(
                    "{:.0}m · {:.0}MiB{}",
                    g.usage.cpu_milli,
                    g.usage.memory_bytes / 1048576.,
                    if g.measured < g.pods { " partial" } else { "" }
                )
            };
            lines.push(Line::styled(
                ellipsis(
                    &format!(
                        "  {} · ↻ {} · {} nodes · {usage}",
                        g.key.namespace,
                        g.restarts,
                        g.nodes.len()
                    ),
                    width,
                ),
                theme::muted_style(),
            ));
            lines.push(Line::from(""));
            workspace.buttons.push((
                Rect::new(columns[0].x, y, columns[0].width, 2),
                format!("group {i}"),
            ));
        }
        if dash.groups.is_empty() {
            lines.push(Line::styled(
                "Waiting for pods in this scope…",
                theme::muted_style(),
            ));
        }
        f.render_widget(Paragraph::new(lines), columns[0]);
    }
    if columns[1].width > 0 {
        workspace.issue = workspace.issue.min(issues.len().saturating_sub(1));
        let mut lines = vec![Line::styled(
            "ATTENTION · a",
            if dash.issues_focus {
                theme::label_style()
            } else {
                theme::muted_style()
            },
        )];
        workspace.buttons.push((
            Rect {
                height: 1,
                ..columns[1]
            },
            "focus-issues".into(),
        ));
        let count = (columns[1].height.saturating_sub(1) / 3).max(1) as usize;
        let start = workspace.issue.saturating_sub(count - 1);
        for (n, (index, pod)) in issues.iter().enumerate().skip(start).take(count) {
            let y = columns[1].y + lines.len() as u16;
            lines.push(Line::styled(
                ellipsis(
                    &format!(
                        "{} {}",
                        if n == workspace.issue && dash.issues_focus {
                            "›"
                        } else {
                            " "
                        },
                        pod.name_any()
                    ),
                    columns[1].width as usize,
                ),
                if n == workspace.issue && dash.issues_focus {
                    theme::text_style().bg(theme::SURFACE)
                } else {
                    theme::text_style()
                },
            ));
            lines.push(Line::styled(
                ellipsis(&format!("  {}", reason(pod)), columns[1].width as usize),
                Style::default().fg(theme::CORAL),
            ));
            lines.push(Line::from(""));
            workspace.buttons.push((
                Rect::new(columns[1].x, y, columns[1].width, 2),
                format!("inspect {index}"),
            ));
        }
        if issues.is_empty() {
            lines.push(Line::styled(
                if status == WatchStatus::Synced {
                    "No pod issues observed"
                } else {
                    "Waiting for pod watch"
                },
                theme::muted_style(),
            ));
        }
        f.render_widget(Paragraph::new(lines), columns[1]);
    }
    let metadata = dash
        .groups
        .get(dash.selected)
        .map(|g| {
            format!(
                "Grouped by {} · nodes: {}",
                g.key.origin,
                if g.nodes.is_empty() {
                    "unscheduled".into()
                } else {
                    g.nodes.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            )
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(vec![
            Line::styled(
                ellipsis(&metadata, rows[4].width as usize),
                theme::muted_style(),
            ),
            Line::styled(
                "g groups · a issues · ↑↓ choose · Enter open · F3 all pods",
                theme::muted_style(),
            ),
            Line::styled(
                "Metrics refresh every 5s here; up to 60 local samples.",
                theme::muted_style(),
            ),
        ]),
        rows[4],
    );
}

fn trend(values: &[Option<f64>], width: usize) -> String {
    let values = &values[values.len().saturating_sub(width)..];
    let max = values
        .iter()
        .flatten()
        .copied()
        .fold(0f64, f64::max)
        .max(f64::EPSILON);
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return "Waiting for samples…".into();
    }
    values
        .iter()
        .map(|v| {
            v.map(|n| bars[((n / max * 7.).round() as usize).min(7)])
                .unwrap_or('·')
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_gaps_are_distinct_from_real_zero_usage() {
        assert_eq!(trend(&[Some(0.), None, Some(10.)], 10), "▁·█");
    }
    #[test]
    fn dashboard_exposes_group_metadata_and_handles_small_empty_terminals() {
        use ratatui::{Terminal, backend::TestBackend};
        let pod: DynamicObject = serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"demo","name":"api-1","labels":{"app":"api"}},"spec":{"nodeName":"worker-1"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"ready":true,"restartCount":12}]}})).unwrap();
        let mut workspace = Workspace::default();
        for (width, height) in [(150, 40), (80, 24), (30, 10), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|f| {
                    render(
                        f,
                        f.area(),
                        &[Arc::new(pod.clone())],
                        WatchStatus::Synced,
                        &mut workspace,
                    )
                })
                .unwrap();
            assert_eq!(workspace.dashboard.groups[0].restarts, 12);
            assert_eq!(workspace.dashboard.groups[0].nodes.len(), 1);
            if width >= 80 {
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("Unavailable"));
                assert!(text.contains("worker-1"));
                assert!(text.contains("app label"));
                assert!(
                    text.contains("1/1 ready"),
                    "groups must remain visible at 80x24"
                );
            }
            terminal
                .draw(|f| render(f, f.area(), &[], WatchStatus::Synced, &mut workspace))
                .unwrap();
            assert!(workspace.dashboard.groups.is_empty());
        }
    }
}
