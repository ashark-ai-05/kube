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
    layout::Rect,
    style::Style,
    widgets::{Block, Clear, Paragraph},
};
use std::sync::Arc;

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
    if object.metadata.deletion_timestamp.is_some() {
        return "Terminating".into();
    }
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
    pub(crate) buttons: Vec<(Rect, String)>,
    pub dashboard: crate::dashboard::Dashboard,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            dashboard: crate::dashboard::Dashboard::default(),
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
        crate::ui::dashboard::render(f, area, objects, status, self);
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
