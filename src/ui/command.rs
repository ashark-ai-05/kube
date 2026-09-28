use crate::ui::theme;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};

pub const COMMANDS: &[(&str, &str)] = &[
    ("logs", "Live logs for the selected pod or workload"),
    ("related", "Explore owners, pods, services and storage"),
    ("overview", "Readiness, rollout and container diagnostics"),
    ("yaml", "Inspect YAML"),
    ("events", "Events for this exact object"),
    ("metrics", "CPU and memory from metrics-server"),
    ("cluster", "Choose cluster"),
    ("namespace", "Choose namespace"),
    ("filter", "Filter resources by name or label:key=value"),
    ("clear", "Clear the resource filter"),
    ("mouse", "Toggle mouse capture for terminal text selection"),
    ("sidebar", "Toggle resource sidebar"),
    ("write", "Enable explicitly confirmed write actions"),
    ("readonly", "Disable write actions"),
    ("scale 3", "Scale selected workload (confirmation required)"),
    (
        "restart",
        "Restart selected workload (confirmation required)",
    ),
    ("delete", "Delete selected resource (confirmation required)"),
    ("exec", "Open a shell in a selected pod via kubectl"),
    (
        "forward 8080:80",
        "Forward localhost port to selected pod via kubectl",
    ),
    ("stop-forwards", "Stop this application's port forwards"),
    ("help", "Keyboard and mouse guide"),
];
#[derive(Default)]
pub struct CommandBar {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub help: bool,
    hit_rows: Vec<(Rect, String)>,
}
impl CommandBar {
    pub fn start(&mut self) {
        self.open = true;
        self.help = false;
        self.query.clear();
        self.selected = 0;
    }
    pub fn items(&self) -> Vec<(&str, &str)> {
        let query = self.query.to_lowercase();
        let mut items: Vec<_> = COMMANDS
            .iter()
            .copied()
            .filter(|(name, description)| fuzzy_match(&query, &format!("{name} {description}")))
            .collect();
        items.sort_by_key(|(name, _)| {
            if *name == query {
                0
            } else if name.starts_with(&query) {
                1
            } else {
                2
            }
        });
        items
    }
    pub fn handle(&mut self, event: &Event) -> Option<String> {
        if let Event::Mouse(m) = event
            && matches!(
                m.kind,
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
            && let Some((_, command)) = self
                .hit_rows
                .iter()
                .find(|(r, _)| r.contains((m.column, m.row).into()))
        {
            self.open = false;
            return Some(command.clone());
        }
        if let Event::Key(k) = event
            && k.kind == KeyEventKind::Press
        {
            match k.code {
                KeyCode::Esc => {
                    self.open = false;
                    self.help = false;
                }
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.selected = 0;
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(self.items().len().saturating_sub(1))
                }
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Enter => {
                    let dynamic = self
                        .query
                        .split_whitespace()
                        .next()
                        .is_some_and(|v| matches!(v, "scale" | "forward" | "exec"));
                    let command = if dynamic {
                        self.query.clone()
                    } else {
                        self.items()
                            .get(self.selected)
                            .map(|v| v.0.to_string())
                            .unwrap_or_else(|| self.query.clone())
                    };
                    self.open = false;
                    return Some(command);
                }
                _ => {}
            }
        }
        None
    }
    pub fn render(&mut self, f: &mut Frame) {
        let area = crate::ui::views::picker::centered(f.area(), 80, 70);
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_style())
            .title(if self.help {
                " Keyboard & mouse "
            } else {
                " Command palette "
            });
        let inner = block.inner(area);
        f.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .split(inner);
        if self.help {
            f.render_widget(Paragraph::new("Tab focus · ↑/↓ or j/k navigate · Enter inspect\n: command palette · / filter · c cluster · n namespace\nl logs · r related · 1–6 inspector tabs\n[ / ] resize sidebar · b toggle sidebar · drag pane divider\nm mouse capture · Shift+drag terminal text selection\nLogs: f follow/pause · c container · p previous · s time window\n/ search (re: for regex) · e export · y copy\nEsc close/back · q quit · Ctrl-C quit\n\nWrites start disabled. :write enables confirmations;\n:readonly disables them. Each mutation shows its exact target.\nExec and port-forward require kubectl on PATH.").style(theme::text_style()),inner);
            return;
        }
        f.render_widget(
            Paragraph::new(format!("> {}█", self.query)).style(theme::header_style()),
            rows[0],
        );
        self.hit_rows = self
            .items()
            .iter()
            .enumerate()
            .take(rows[1].height as usize)
            .map(|(i, (command, _))| {
                (
                    Rect::new(rows[1].x, rows[1].y + i as u16, rows[1].width, 1),
                    command.to_string(),
                )
            })
            .collect();
        let lines: Vec<Line> = self
            .items()
            .iter()
            .enumerate()
            .take(rows[1].height as usize)
            .map(|(i, (name, description))| {
                Line::styled(
                    format!(
                        "{} {:19} {}",
                        if i == self.selected { "›" } else { " " },
                        name,
                        description
                    ),
                    if i == self.selected {
                        theme::header_style()
                    } else {
                        theme::text_style()
                    },
                )
            })
            .collect();
        f.render_widget(Paragraph::new(lines), rows[1]);
        f.render_widget(
            Paragraph::new("↑/↓ select · Enter run · Esc close").style(theme::muted_style()),
            rows[2],
        );
    }
}
pub fn fuzzy_match(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    let mut chars = text.chars();
    query
        .to_lowercase()
        .chars()
        .all(|c| chars.any(|next| next == c))
}
pub fn matches_resource(query: &str, obj: &kube::api::DynamicObject) -> bool {
    use kube::ResourceExt;
    query.split_whitespace().all(|part| {
        if let Some(label) = part.strip_prefix("label:") {
            if let Some((key, value)) = label.split_once("!=") {
                return obj.labels().get(key).is_none_or(|v| v != value);
            }
            if let Some((key, value)) = label.split_once('=') {
                return obj.labels().get(key).is_some_and(|v| v == value);
            }
            return obj.labels().contains_key(label);
        }
        fuzzy_match(
            part,
            &format!("{}/{}", obj.namespace().unwrap_or_default(), obj.name_any()),
        )
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_command_wins_over_description_match() {
        let mut bar = CommandBar {
            query: "help".into(),
            ..Default::default()
        };
        assert_eq!(
            bar.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE
            )))
            .as_deref(),
            Some("help")
        );
    }
    #[test]
    fn filter_composes_fuzzy_name_and_label_predicates() {
        let p=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"payments-api","namespace":"prod","labels":{"app":"payments"}}})).unwrap();
        assert!(matches_resource("payapi label:app=payments", &p));
        assert!(!matches_resource("label:app!=payments", &p));
        assert!(!matches_resource("label:missing", &p));
    }
}
