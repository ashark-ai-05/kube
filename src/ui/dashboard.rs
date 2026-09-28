use crate::{
    app::event::WatchStatus,
    dashboard::{
        Dashboard,
        pod::{self, Filter, Sort},
    },
    ui::{
        log_view::ellipsis,
        theme,
        workspace::{Workspace, health, health_style},
    },
};
use kube::{ResourceExt, api::DynamicObject};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState},
};
use std::sync::Arc;

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
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    let dashboard = &mut workspace.dashboard;
    let counts = dashboard.refresh(objects);
    let compact = inner.height < 28;
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(if !dashboard.details {
            0
        } else if compact {
            7
        } else {
            11
        }),
        Constraint::Length(1),
    ])
    .spacing(1)
    .split(inner);
    let status_label = if status == WatchStatus::Synced {
        "live"
    } else {
        "watch incomplete"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("POD MONITOR  ", theme::header_style()),
            Span::styled(
                format!("{} pods · {status_label}", objects.len()),
                theme::muted_style(),
            ),
        ])),
        sections[0],
    );
    let filters = [
        Filter::All,
        Filter::NotReady,
        Filter::Restarts,
        Filter::HighMemory,
    ];
    let tiles = Layout::horizontal([Constraint::Fill(1); 4])
        .spacing(1)
        .split(sections[1]);
    for (i, filter) in filters.iter().enumerate() {
        let selected = dashboard.filter == *filter;
        let label = if inner.width < 70 {
            match filter {
                Filter::NotReady => "Unready",
                Filter::HighMemory => "Mem 80%",
                _ => filter.label(),
            }
        } else {
            filter.label()
        };
        f.render_widget(
            Paragraph::new(format!("{label} {}", counts[i]))
                .style(if selected {
                    theme::text_style().bg(theme::SURFACE)
                } else {
                    theme::muted_style()
                })
                .block(
                    Block::default()
                        .borders(Borders::BOTTOM)
                        .border_style(if selected {
                            theme::label_style()
                        } else {
                            theme::border_style()
                        }),
                ),
            tiles[i],
        );
        workspace
            .buttons
            .push((tiles[i], format!("pod-filter {}", filter.key())));
    }
    let wide = inner.width >= 100;
    let medium = inner.width >= 70;
    let mut headers = vec!["Name", "Ready", "Restarts", "Age", "CPU", "Memory"];
    let mut widths = vec![
        Constraint::Fill(3),
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(if medium { 17 } else { 10 }),
    ];
    if medium {
        headers.push("Status");
        widths.push(Constraint::Length(16));
    }
    if wide {
        headers.push("Node");
        widths.push(Constraint::Fill(1));
    }
    if !medium {
        headers.remove(3);
        widths.remove(3);
        headers.remove(1);
        widths.remove(1);
    }
    let cell_rects = Layout::horizontal(widths.clone()).spacing(1).split(Rect {
        x: sections[2].x + 2,
        width: sections[2].width.saturating_sub(2),
        height: 1,
        ..sections[2]
    });
    for (i, header) in headers.iter().enumerate() {
        if Sort::parse(header).is_some() {
            workspace
                .buttons
                .push((cell_rects[i], format!("pod-sort {header}")));
        }
    }
    let head = Row::new(headers.iter().map(|header| {
        Cell::from(format!(
            "{}{}",
            header,
            if dashboard.sort.label() == *header {
                if dashboard.descending { " ↓" } else { " ↑" }
            } else {
                ""
            }
        ))
    }))
    .style(theme::label_style())
    .bottom_margin(1);
    let visible = sections[2].height.saturating_sub(2).max(1) as usize;
    dashboard.list_height = visible;
    let start = dashboard.selected.saturating_sub(visible.saturating_sub(1));
    let multi_namespace = objects.first().is_some_and(|first| {
        objects
            .iter()
            .any(|p| p.metadata.namespace != first.metadata.namespace)
    });
    let rows: Vec<_> = dashboard
        .rows
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, row)| {
            let object = &row.object;
            let name = if multi_namespace {
                format!(
                    "{}/{}",
                    object.namespace().unwrap_or_default(),
                    object.name_any()
                )
            } else {
                object.name_any()
            };
            let mem = row
                .usage
                .map(|u| {
                    format!(
                        "{:.0}Mi{}",
                        u.memory_bytes / 1048576.,
                        if medium {
                            row.budget
                                .memory_percent(Some(u))
                                .map(|p| format!(" {p:.0}%"))
                                .unwrap_or_else(|| " / —".into())
                        } else {
                            String::new()
                        }
                    )
                })
                .unwrap_or_else(|| "—".into());
            let all = [
                Cell::from(name).style(health_style(health(object))),
                Cell::from(pod::ready(object)),
                Cell::from(row.restarts.to_string()).style(if row.restarts > 0 {
                    Style::default().fg(theme::AMBER)
                } else {
                    theme::muted_style()
                }),
                Cell::from(row.age.map(pod::duration).unwrap_or_else(|| "—".into())),
                Cell::from(
                    row.usage
                        .map(|u| format!("{:.0}m", u.cpu_milli))
                        .unwrap_or_else(|| "—".into()),
                ),
                Cell::from(mem).style(
                    if row
                        .budget
                        .memory_percent(row.usage)
                        .is_some_and(|n| n >= 80.)
                    {
                        Style::default().fg(theme::AMBER)
                    } else {
                        theme::text_style()
                    },
                ),
                Cell::from(pod::status(object)).style(health_style(health(object))),
                Cell::from(
                    object.data["spec"]["nodeName"]
                        .as_str()
                        .unwrap_or("Unscheduled")
                        .to_string(),
                ),
            ];
            let indices: Vec<usize> = if wide {
                (0..8).collect()
            } else if medium {
                (0..7).collect()
            } else {
                vec![0, 2, 4, 5]
            };
            workspace.buttons.push((
                Rect::new(
                    sections[2].x,
                    sections[2].y + 2 + (i - start) as u16,
                    sections[2].width,
                    1,
                ),
                format!("pod-select {i}"),
            ));
            Row::new(
                indices
                    .into_iter()
                    .map(|i| all[i].clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut table_state = TableState::default()
        .with_selected((!dashboard.rows.is_empty()).then_some(dashboard.selected - start));
    f.render_stateful_widget(
        Table::new(rows, widths)
            .header(head)
            .column_spacing(1)
            .row_highlight_style(theme::text_style().bg(theme::SURFACE))
            .highlight_symbol("› "),
        sections[2],
        &mut table_state,
    );
    if dashboard.rows.is_empty() {
        f.render_widget(
            Paragraph::new("No pods match this view · 0 resets filters")
                .style(theme::muted_style()),
            Rect {
                y: sections[2].y + 2,
                height: 1,
                ..sections[2]
            },
        );
    }
    if dashboard.details {
        render_detail(f, sections[3], dashboard);
    }
    f.render_widget(Paragraph::new(if compact{"↑↓ select · Enter inspect · l logs · s sort · d / D details"}else{"↑↓ select · Enter inspect · l logs · s sort · / search · d containers · D hide details · 0–3 filters"}).style(theme::muted_style()),sections[4]);
}
fn render_detail(f: &mut Frame, area: Rect, dashboard: &Dashboard) {
    let Some(row) = dashboard.rows.get(dashboard.selected) else {
        return;
    };
    let object = &row.object;
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::label_style())
        .title(Line::styled(
            format!(
                " {} / {} · {} ",
                object.namespace().unwrap_or_default(),
                object.name_any(),
                pod::status(object)
            ),
            theme::label_style(),
        ));
    let inside = block.inner(area);
    f.render_widget(block, area);
    let sections = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(inside);
    let metadata = format!(
        "{} · node {} · age {} · last restart {}",
        pod::owner(object),
        object.data["spec"]["nodeName"]
            .as_str()
            .unwrap_or("Unscheduled"),
        row.age.map(pod::duration).unwrap_or_else(|| "—".into()),
        pod::last_restart(object)
            .map(|n| format!("{} ago", pod::duration(n)))
            .unwrap_or_else(|| "—".into())
    );
    f.render_widget(
        Paragraph::new(ellipsis(&metadata, sections[0].width as usize)).style(theme::muted_style()),
        sections[0],
    );
    if dashboard.containers {
        render_containers(f, sections[1], dashboard, &row.object);
    } else {
        let columns = Layout::horizontal([Constraint::Fill(1); 2])
            .spacing(2)
            .split(sections[1]);
        for (i, title) in ["CPU", "MEMORY"].iter().enumerate() {
            let color = if i == 0 { theme::TEAL } else { theme::VIOLET };
            let value = row
                .usage
                .map(|u| {
                    if i == 0 {
                        format!("{:.1} mCPU", u.cpu_milli)
                    } else {
                        format!("{:.1} MiB", u.memory_bytes / 1048576.)
                    }
                })
                .unwrap_or_else(|| "Unavailable".into());
            let budget = if i == 0 {
                format!(
                    "request {} · limit {}",
                    cpu(row.budget.cpu_request),
                    cpu(row.budget.cpu_limit)
                )
            } else {
                format!(
                    "request {} · limit {}",
                    memory(row.budget.memory_request),
                    memory(row.budget.memory_limit)
                )
            };
            let mut lines = vec![
                Line::from(vec![Span::styled(
                    format!("{title}  {value}"),
                    Style::default().fg(color).bold(),
                )]),
                Line::styled(
                    ellipsis(&budget, columns[i].width as usize),
                    theme::muted_style(),
                ),
            ];
            if columns[i].height > 2 {
                let values: Vec<_> = dashboard
                    .history
                    .iter()
                    .map(|u| {
                        u.map(|u| {
                            if i == 0 {
                                u.cpu_milli
                            } else {
                                u.memory_bytes / 1048576.
                            }
                        })
                    })
                    .collect();
                lines.push(Line::styled(
                    trend(&values, columns[i].width as usize),
                    Style::default().fg(color),
                ));
            }
            if columns[i].height > 3 {
                lines.push(Line::styled(
                    if row.usage.is_some() {
                        "5s polling · selected-pod session samples".into()
                    } else {
                        dashboard
                            .metrics
                            .error
                            .clone()
                            .unwrap_or_else(|| "Waiting for metrics-server samples".into())
                    },
                    theme::muted_style(),
                ));
            }
            if columns[i].height > 4 {
                let note = if i == 0 {
                    format!("{} restarts · {} ready", row.restarts, pod::ready(object))
                } else {
                    let ended = pod::termination(object);
                    if ended.is_empty() {
                        "Memory % in table is usage / limit".into()
                    } else {
                        ended
                    }
                };
                lines.push(Line::styled(
                    ellipsis(&note, columns[i].width as usize),
                    theme::muted_style(),
                ));
            }
            f.render_widget(
                Paragraph::new(lines).style(Style::default().bg(theme::ABYSS)),
                columns[i],
            );
        }
    }
    f.render_widget(
        Paragraph::new(ellipsis(
            &dashboard.events.note_for(&pod::Identity::of(object)),
            sections[2].width as usize,
        ))
        .style(theme::muted_style()),
        sections[2],
    );
}
fn render_containers(f: &mut Frame, area: Rect, dashboard: &Dashboard, object: &DynamicObject) {
    let lines: Vec<_> = object.data["spec"]["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            object.data["spec"]["initContainers"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .flat_map(|c| {
            let name = c["name"].as_str().unwrap_or("container");
            let usage = dashboard.metrics.container(object, name);
            let status = pod::statuses(object).find(|s| s["name"] == name);
            let uptime = status
                .and_then(|s| s["state"]["running"]["startedAt"].as_str())
                .and_then(pod::since)
                .map(pod::duration)
                .unwrap_or_else(|| "—".into());
            let probe = |key: &str| {
                if c.get(key).is_some_and(|v| v.is_object()) {
                    "set"
                } else {
                    "—"
                }
            };
            let text = format!(
                "{name} · {} · {} · uptime {uptime} · ready {}",
                cpu(usage.map(|u| u.cpu_milli)),
                memory(usage.map(|u| u.memory_bytes)),
                if status.is_some_and(|s| s["ready"] == true) {
                    "yes"
                } else {
                    "no"
                }
            );
            let detail = format!(
                "  probes: live {} / ready {} / startup {} · {}",
                probe("livenessProbe"),
                probe("readinessProbe"),
                probe("startupProbe"),
                c["image"].as_str().unwrap_or("")
            );
            [
                Line::styled(ellipsis(&text, area.width as usize), theme::text_style()),
                Line::styled(ellipsis(&detail, area.width as usize), theme::muted_style()),
            ]
        })
        .take(area.height as usize)
        .collect();
    f.render_widget(Paragraph::new(lines), area);
}
fn cpu(n: Option<f64>) -> String {
    n.map(|n| format!("{n:.0}m")).unwrap_or_else(|| "—".into())
}
fn memory(n: Option<f64>) -> String {
    n.map(|n| format!("{:.0}Mi", n / 1048576.))
        .unwrap_or_else(|| "—".into())
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
    use ratatui::{Terminal, backend::TestBackend};
    fn objects() -> Vec<Arc<DynamicObject>> {
        ["web-a","web-b"].iter().map(|name| Arc::new(serde_json::from_value(serde_json::json!({
            "apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"demo","uid":name},
            "spec":{"nodeName":"worker","containers":[{"name":"app","image":"example:1","livenessProbe":{"httpGet":{"path":"/health","port":8080}},"readinessProbe":{"httpGet":{"path":"/ready","port":8080}}}]},
            "status":{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":2}],"conditions":[{"type":"Ready","status":"True"}]}
        })).unwrap())).collect()
    }
    #[test]
    fn responsive_flat_view_exposes_selection_probes_and_honest_metric_gaps() {
        let objects = objects();
        for (width, height) in [(150, 40), (80, 24), (48, 16)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut workspace = Workspace::default();
            terminal
                .draw(|f| render(f, f.area(), &objects, WatchStatus::Synced, &mut workspace))
                .unwrap();
            let snapshot = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(snapshot.contains("POD MONITOR"), "{width}x{height}");
            assert!(snapshot.contains("web-a"));
            assert!(!snapshot.contains("POD GROUPS"));
            if height >= 24 {
                assert!(snapshot.contains("Unavailable"));
            }
            workspace.dashboard.containers = true;
            terminal
                .draw(|f| render(f, f.area(), &objects, WatchStatus::Synced, &mut workspace))
                .unwrap();
            let snapshot = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            if width >= 80 && height >= 24 {
                assert!(snapshot.contains("live set / ready set"));
            }
            assert_eq!(
                workspace
                    .dashboard
                    .selected_pod()
                    .unwrap()
                    .metadata
                    .name
                    .as_deref(),
                Some("web-a")
            );
        }
    }
    #[test]
    fn gaps_are_not_zero_measurements() {
        assert_eq!(trend(&[Some(0.), None, Some(2.)], 3), "▁·█");
        assert_eq!(trend(&[Some(1.), None], 1), "·");
    }
}
