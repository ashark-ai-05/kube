//! Focused navigation and a health overview derived only from the watched scope.
use crate::{
    app::event::WatchStatus,
    ui::{log_view::ellipsis, theme},
};
use kube::{
    ResourceExt,
    api::{DynamicObject, GroupVersionKind},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ready,
    Attention,
    Completed,
    Unknown,
}
pub fn health(object: &DynamicObject) -> Health {
    let status = &object.data["status"];
    let phase = status["phase"].as_str().unwrap_or("");
    if phase == "Succeeded" {
        return Health::Completed;
    }
    if object.metadata.deletion_timestamp.is_some() {
        return Health::Attention;
    }
    if matches!(phase, "Pending" | "Failed" | "Unknown") {
        return Health::Attention;
    }
    for key in ["initContainerStatuses", "containerStatuses"] {
        if status[key].as_array().is_some_and(|cs| {
            cs.iter().any(|c| {
                c["state"]["waiting"]["reason"]
                    .as_str()
                    .is_some_and(|r| !matches!(r, "PodInitializing" | "ContainerCreating"))
                    || c["state"]["terminated"]["exitCode"]
                        .as_i64()
                        .is_some_and(|n| n != 0)
            })
        }) {
            return Health::Attention;
        }
    }
    if let Some(conditions) = status["conditions"].as_array() {
        if let Some(ready) = conditions.iter().find(|c| c["type"] == "Ready") {
            return if ready["status"] == "True" {
                Health::Ready
            } else {
                Health::Attention
            };
        }
        if conditions.iter().any(|c| {
            (c["type"] == "Failed" && c["status"] == "True")
                || (c["type"] == "Available" && c["status"] == "False")
        }) {
            return Health::Attention;
        }
        if conditions
            .iter()
            .any(|c| c["type"] == "Complete" && c["status"] == "True")
        {
            return Health::Completed;
        }
    }
    if phase == "Running" {
        return Health::Unknown;
    }
    if let Some(desired) = object.data["spec"]["replicas"].as_u64() {
        return if status["readyReplicas"].as_u64().unwrap_or(0) >= desired {
            Health::Ready
        } else {
            Health::Attention
        };
    }
    Health::Unknown
}
pub fn reason(object: &DynamicObject) -> String {
    for key in ["initContainerStatuses", "containerStatuses"] {
        if let Some(cs) = object.data["status"][key].as_array() {
            for c in cs {
                for state in ["waiting", "terminated"] {
                    if let Some(reason) = c["state"][state]["reason"].as_str() {
                        return reason.into();
                    }
                }
            }
        }
    }
    object.data["status"]["phase"]
        .as_str()
        .unwrap_or("Not ready")
        .into()
}
pub fn health_style(value: Health) -> Style {
    Style::default().fg(match value {
        Health::Ready => theme::TEAL,
        Health::Attention => theme::CORAL,
        Health::Completed | Health::Unknown => theme::MIST,
    })
}

pub struct Workspace {
    pub home: bool,
    pub catalog: bool,
    pub selected: usize,
    pub issue: usize,
    pub recent: Vec<DynamicObject>,
    buttons: Vec<(Rect, String)>,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            home: true,
            catalog: false,
            selected: 0,
            issue: 0,
            recent: vec![],
            buttons: vec![],
        }
    }
}
impl Workspace {
    pub fn remember(&mut self, object: &DynamicObject) {
        self.recent
            .retain(|o| o.uid() != object.uid() || o.name_any() != object.name_any());
        self.recent.insert(0, object.clone());
        self.recent.truncate(6);
    }
    pub fn command_at(&self, x: u16, y: u16) -> Option<String> {
        self.buttons
            .iter()
            .rev()
            .find(|(r, _)| r.contains((x, y).into()))
            .map(|(_, c)| c.clone())
    }
    pub fn clear_hits(&mut self) {
        self.buttons.clear();
    }
    pub fn nav_commands(&self) -> Vec<String> {
        let mut out = [
            "home",
            "kind Pod",
            "kind Deployment",
            "kind StatefulSet",
            "kind Job",
            "kind Service",
            "resources",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
        out.extend((0..self.recent.len()).map(|n| format!("recent {n}")));
        out
    }
    pub fn render_rail(
        &mut self,
        f: &mut Frame,
        area: Rect,
        kind: &GroupVersionKind,
        focused: bool,
    ) {
        f.render_widget(Clear, area);
        f.render_widget(
            Block::default().style(Style::default().bg(theme::ABYSS)),
            area,
        );
        if area.width < 6 || area.height < 3 {
            return;
        }
        let labels = [
            "◈  Fleet radar",
            "▤  Pods",
            "▤  Deployments",
            "▤  StatefulSets",
            "▤  Jobs",
            "⇄  Services",
            "⌕  All resources",
        ];
        let commands = self.nav_commands();
        let mut y = area.y + 1;
        f.render_widget(
            Paragraph::new(" WORKSPACE").style(theme::muted_style()),
            Rect::new(area.x + 1, y, area.width - 2, 1),
        );
        y += 2;
        for (i, label) in labels.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let subject = if i == 0 {
                self.home
            } else {
                !self.home && commands[i] == format!("kind {}", kind.kind)
            };
            let style = if focused && self.selected == i {
                theme::header_style().bg(theme::DUSK)
            } else if subject {
                theme::label_style().bg(theme::SURFACE)
            } else {
                theme::text_style()
            };
            let r = Rect::new(area.x + 1, y, area.width - 2, 1);
            f.render_widget(
                Paragraph::new(ellipsis(label, r.width as usize)).style(style),
                r,
            );
            self.buttons.push((r, commands[i].clone()));
            y += 2;
        }
        y += 1;
        if y < area.bottom() {
            f.render_widget(
                Paragraph::new(" RECENT").style(theme::muted_style()),
                Rect::new(area.x + 1, y, area.width - 2, 1),
            );
            y += 2;
        }
        for (i, object) in self.recent.iter().enumerate() {
            if y >= area.bottom().saturating_sub(2) {
                break;
            }
            let r = Rect::new(area.x + 1, y, area.width - 2, 1);
            f.render_widget(
                Paragraph::new(ellipsis(
                    &format!(" {}", object.name_any()),
                    r.width as usize,
                ))
                .style(if focused && self.selected == i + 7 {
                    theme::header_style().bg(theme::DUSK)
                } else {
                    theme::muted_style()
                }),
                r,
            );
            self.buttons.push((r, format!("recent {i}")));
            y += 1;
        }
        if area.height > 22 {
            let r = Rect::new(area.x + 1, area.bottom() - 3, area.width - 2, 2);
            f.render_widget(
                Paragraph::new(" ^Space Ask Kube\n ^K Commands · ? Help")
                    .style(Style::default().fg(theme::VIOLET)),
                r,
            );
            self.buttons.push((r, "ask".into()));
        }
    }
    pub fn render_home(
        &mut self,
        f: &mut Frame,
        area: Rect,
        objects: &[Arc<DynamicObject>],
        status: WatchStatus,
    ) {
        f.render_widget(Clear, area);
        f.render_widget(
            Block::default().style(Style::default().bg(theme::INK)),
            area,
        );
        let inner = Rect::new(
            area.x + 2,
            area.y + 1,
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        );
        let rows = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Fill(1),
            Constraint::Length(2),
        ])
        .split(inner);
        f.render_widget(
            Paragraph::new(vec![
                Line::styled("Fleet radar", theme::header_style()),
                Line::styled(
                    "Live signals from pods in the selected cluster and namespace",
                    theme::muted_style(),
                ),
            ]),
            rows[0],
        );
        let mut counts = [0usize; 4];
        let mut issues = Vec::new();
        let mut namespaces = BTreeMap::<&str, (usize, usize)>::new();
        for (index, object) in objects.iter().enumerate() {
            let value = health(object);
            counts[match value {
                Health::Ready => 0,
                Health::Attention => 1,
                Health::Completed => 2,
                Health::Unknown => 3,
            }] += 1;
            if value == Health::Attention {
                issues.push((index, object));
            }
            let entry = namespaces
                .entry(object.metadata.namespace.as_deref().unwrap_or(""))
                .or_default();
            entry.0 += 1;
            entry.1 += usize::from(value == Health::Attention);
        }
        let cards = Layout::horizontal([Constraint::Fill(1); 4])
            .spacing(1)
            .split(rows[1]);
        for (i, (label, color)) in [
            ("READY", theme::TEAL),
            ("NEEDS ATTENTION", theme::CORAL),
            ("COMPLETED", theme::MIST),
            ("UNKNOWN", theme::AMBER),
        ]
        .iter()
        .enumerate()
        {
            f.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        format!(" {}", counts[i]),
                        Style::default().fg(*color).bold(),
                    ),
                    Line::styled(format!(" {label}"), theme::muted_style()),
                ])
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(*color))
                        .style(Style::default().bg(theme::ABYSS)),
                ),
                cards[i],
            );
        }
        let columns = Layout::horizontal(if area.width >= 90 {
            vec![Constraint::Percentage(62), Constraint::Percentage(38)]
        } else {
            vec![Constraint::Percentage(100), Constraint::Length(0)]
        })
        .spacing(3)
        .split(rows[2]);
        self.issue = self.issue.min(issues.len().saturating_sub(1));
        let mut lines = vec![
            Line::styled("ATTENTION QUEUE", theme::muted_style()),
            Line::from(""),
        ];
        if issues.is_empty() {
            lines.push(Line::styled(
                if matches!(status, WatchStatus::Synced) {
                    "No pod issues in this watched scope."
                } else {
                    "Waiting for a healthy watch; counts may be incomplete."
                },
                theme::label_style(),
            ));
        }
        let visible = (columns[0].height.saturating_sub(2) / 3).max(1) as usize;
        let start = self.issue.saturating_sub(visible - 1);
        for (n, (index, object)) in issues.iter().enumerate().skip(start).take(visible) {
            let y = columns[0].y + lines.len() as u16;
            let style = if n == self.issue {
                theme::text_style().bg(theme::SURFACE)
            } else {
                theme::text_style()
            };
            lines.push(Line::styled(
                ellipsis(
                    &format!(
                        "{} {}",
                        if n == self.issue { "›" } else { " " },
                        object.name_any()
                    ),
                    columns[0].width as usize,
                ),
                style,
            ));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}", reason(object)),
                    theme::phase_style(&reason(object)),
                ),
                Span::styled(
                    format!(" · {}", object.namespace().unwrap_or_default()),
                    theme::muted_style(),
                ),
            ]));
            lines.push(Line::from(""));
            self.buttons.push((
                Rect::new(columns[0].x, y, columns[0].width, 2),
                format!("inspect {index}"),
            ));
        }
        f.render_widget(Paragraph::new(lines), columns[0]);
        let mut lines = vec![
            Line::styled("NAMESPACE SIGNALS", theme::muted_style()),
            Line::from(""),
        ];
        for (name, (total, bad)) in namespaces
            .iter()
            .take(columns[1].height.saturating_sub(4) as usize / 2)
        {
            lines.push(Line::styled(
                ellipsis(name, columns[1].width as usize),
                theme::text_style(),
            ));
            lines.push(Line::from(vec![
                Span::styled(
                    "▰".repeat((*total).min(18)),
                    if *bad > 0 {
                        theme::phase_style("Pending")
                    } else {
                        theme::label_style()
                    },
                ),
                Span::styled(
                    format!(" {total} pods · {bad} issues"),
                    theme::muted_style(),
                ),
            ]));
        }
        f.render_widget(Paragraph::new(lines), columns[1]);
        f.render_widget(Paragraph::new("↑/↓ choose issue · Enter investigate · F3 resource browser\nCounts reflect the current pod watch; other clusters are not queried.").style(theme::muted_style()).wrap(Wrap{trim:false}),rows[3]);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pod(status: serde_json::Value) -> DynamicObject {
        serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"test"},"status":status})).unwrap()
    }
    #[test]
    fn running_is_not_proof_of_readiness() {
        assert_eq!(
            health(&pod(serde_json::json!({"phase":"Running"}))),
            Health::Unknown
        );
        assert_eq!(
            health(&pod(
                serde_json::json!({"phase":"Running","conditions":[{"type":"Ready","status":"False"}]})
            )),
            Health::Attention
        );
    }
    #[test]
    fn completed_jobs_are_not_incidents() {
        assert_eq!(
            health(&pod(serde_json::json!({"phase":"Succeeded"}))),
            Health::Completed
        );
    }
    #[test]
    fn crashloop_overrides_running_phase() {
        assert_eq!(
            health(&pod(
                serde_json::json!({"phase":"Running","containerStatuses":[{"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]})
            )),
            Health::Attention
        );
    }
}
