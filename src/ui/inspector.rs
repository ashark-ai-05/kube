//! One cancellable investigation session, scoped to an exact object and cluster.
use crate::ui::theme;
use crate::{
    app::event::AppEvent,
    cluster::{self, discovery::KindInfo, related},
    logs::{
        LogBuffer,
        stream::{Message, Options, StreamSession},
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use kube::{Client, ResourceExt, api::DynamicObject};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::PathBuf;
use tokio::{sync::mpsc, task::JoinHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Overview,
    Yaml,
    Events,
    Logs,
    Related,
    Metrics,
}
const MODES: [Mode; 6] = [
    Mode::Overview,
    Mode::Yaml,
    Mode::Events,
    Mode::Logs,
    Mode::Related,
    Mode::Metrics,
];
const LABELS: [&str; 6] = ["Overview", "YAML", "Events", "Logs", "Related", "Metrics"];

pub enum Effect {
    None,
    Close,
    Navigate(Box<DynamicObject>),
    Export(PathBuf),
    Copy(String),
}
enum Reply {
    Text(String),
    Related(related::Related),
}

pub struct Inspector {
    pub object: DynamicObject,
    pub mode: Mode,
    client: Client,
    kinds: Vec<KindInfo>,
    wake: mpsc::UnboundedSender<AppEvent>,
    task: Option<JoinHandle<()>>,
    reply: Option<mpsc::Receiver<Reply>>,
    stream: Option<StreamSession>,
    pub logs: LogBuffer,
    options: Options,
    text: String,
    related: Vec<DynamicObject>,
    selected: usize,
    scroll: usize,
    follow: bool,
    containers: Vec<String>,
    pub status: String,
    editing: Option<Edit>,
    input: String,
    filter: String,
    pretty: bool,
    area: Rect,
    body: Rect,
    tab_rects: Vec<Rect>,
    actions: Vec<(Rect, char)>,
}
#[derive(Clone, Copy)]
enum Edit {
    Search,
    Export,
    Since,
}
impl Drop for Inspector {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Inspector {
    pub fn new(
        object: DynamicObject,
        mode: Mode,
        client: Client,
        kinds: Vec<KindInfo>,
        wake: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        let mut s = Self {
            object,
            mode,
            client,
            kinds,
            wake,
            task: None,
            reply: None,
            stream: None,
            logs: LogBuffer::default(),
            options: Options::default(),
            text: String::new(),
            related: vec![],
            selected: 0,
            scroll: 0,
            follow: true,
            containers: vec![],
            status: String::new(),
            editing: None,
            input: String::new(),
            filter: String::new(),
            pretty: false,
            area: Rect::default(),
            body: Rect::default(),
            tab_rects: vec![],
            actions: vec![],
        };
        s.load();
        s
    }
    pub fn replace(&mut self, object: DynamicObject) {
        self.object = object;
        self.mode = Mode::Overview;
        self.options = Options::default();
        self.load();
    }
    pub fn refresh_object(&mut self, object: &DynamicObject) {
        if object.uid() == self.object.uid()
            && object.resource_version() != self.object.resource_version()
        {
            self.object = object.clone();
            if matches!(self.mode, Mode::Overview | Mode::Yaml) {
                self.load();
            }
        }
    }
    fn load(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.reply = None;
        self.stream = None;
        self.scroll = 0;
        self.selected = 0;
        self.text.clear();
        self.status = "Loading…".into();
        match self.mode {
            Mode::Overview => {
                self.text = diagnostics(&self.object);
                self.status = "Live object state".into();
            }
            Mode::Yaml => {
                self.text = crate::ui::views::detail::object_to_yaml(&self.object);
                self.status = "Secrets redacted · y copy · e export".into();
            }
            Mode::Logs => {
                self.logs = LogBuffer::default();
                let _ = self.logs.filter(&self.filter);
                self.follow = true;
                self.stream = Some(StreamSession::start(
                    self.client.clone(),
                    self.object.clone(),
                    self.options.clone(),
                    self.wake.clone(),
                ));
            }
            Mode::Events | Mode::Related | Mode::Metrics => {
                let object = self.object.clone();
                let client = self.client.clone();
                let kinds = self.kinds.clone();
                let mode = self.mode;
                let wake = self.wake.clone();
                let (tx, rx) = mpsc::channel(1);
                self.reply = Some(rx);
                self.task = Some(tokio::spawn(async move {
                    let reply=match mode {
                        Mode::Related=>Reply::Related(related::fetch_related(client,&object,&kinds).await),
                        Mode::Events=>{
                            let ns=object.namespace();
                            let result=crate::store::events::fetch_events_for_uid(&client,ns.as_deref(),&object.name_any(),object.uid().as_deref()).await;
                            Reply::Text(match result {Ok(events)=>if events.is_empty(){"No events recorded for this object.".into()}else{events.iter().map(|e|format!("{}  {}  {} (×{})\n{}\n",e.age,e.kind,e.reason,e.count,e.message)).collect::<Vec<_>>().join("\n")},Err(e)=>format!("Events unavailable: {}",cluster::safe_error_text(&e))})
                        },
                        _=>Reply::Text(metrics(&client,&object).await.unwrap_or_else(|e|format!("Metrics unavailable: {}\nRequires metrics-server and permission to read metrics.k8s.io.",cluster::safe_error_text(&e)))),
                    };
                    if tx.send(reply).await.is_ok() {
                        let _ = wake.send(AppEvent::Wake);
                    }
                }));
            }
        }
    }
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &mut self.reply
            && let Ok(reply) = rx.try_recv()
        {
            changed = true;
            self.status = "Loaded · R refresh".into();
            match reply {
                Reply::Text(t) => self.text = t,
                Reply::Related(r) => {
                    self.related = r.objects;
                    self.text = r.notes.join("\n");
                    if self.related.is_empty() && self.text.is_empty() {
                        self.text = "No related resources found.".into();
                    }
                }
            }
        }
        if let Some(stream) = &mut self.stream {
            let before = self.logs.matched();
            for msg in stream.drain() {
                changed = true;
                match msg {
                    Message::Line(line) => self.logs.push(line),
                    Message::Status(s) => self.status = s,
                    Message::Containers(c) => self.containers = c,
                }
            }
            if !self.follow {
                self.scroll = self
                    .scroll
                    .saturating_add(self.logs.matched().saturating_sub(before));
            }
        }
        changed
    }
    pub fn handle(&mut self, event: &Event) -> Effect {
        let mut code = None;
        match event {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                    return Effect::Close;
                }
                code = Some(k.code);
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => code = Some(KeyCode::PageUp),
                MouseEventKind::ScrollDown => code = Some(KeyCode::PageDown),
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    if let Some(i) = self
                        .tab_rects
                        .iter()
                        .position(|r| r.contains((m.column, m.row).into()))
                    {
                        self.mode = MODES[i];
                        self.load();
                        return Effect::None;
                    }
                    if let Some((_, key)) = self
                        .actions
                        .iter()
                        .find(|(r, _)| r.contains((m.column, m.row).into()))
                    {
                        code = Some(KeyCode::Char(*key));
                    } else if self.mode == Mode::Related
                        && self.body.contains((m.column, m.row).into())
                    {
                        self.selected = self.scroll + (m.row - self.body.y) as usize;
                        return self
                            .related
                            .get(self.selected)
                            .cloned()
                            .map(|o| Effect::Navigate(Box::new(o)))
                            .unwrap_or(Effect::None);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        let Some(code) = code else {
            return Effect::None;
        };
        if let Some(edit) = self.editing {
            match code {
                KeyCode::Esc => {
                    self.editing = None;
                }
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Char(c) => self.input.push(c),
                KeyCode::Enter => {
                    let value = self.input.clone();
                    self.editing = None;
                    match edit {
                        Edit::Search => match self.logs.filter(&value) {
                            Ok(()) => {
                                self.filter = value;
                                self.scroll = 0;
                            }
                            Err(e) => self.status = format!("Invalid search: {e}"),
                        },
                        Edit::Export => return Effect::Export(PathBuf::from(value)),
                        Edit::Since => match parse_duration(&value) {
                            Ok(seconds) => {
                                self.options.since_seconds = Some(seconds);
                                self.load();
                            }
                            Err(e) => self.status = e,
                        },
                    }
                }
                _ => {}
            }
            return Effect::None;
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return Effect::Close,
            KeyCode::Char(c @ '1'..='6') => {
                self.mode = MODES[c as usize - '1' as usize];
                self.load();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let current = MODES.iter().position(|m| *m == self.mode).unwrap_or(0);
                self.mode = MODES[(current + if code == KeyCode::BackTab { 5 } else { 1 }) % 6];
                self.load();
            }
            KeyCode::Char('l') => {
                self.mode = Mode::Logs;
                self.load();
            }
            KeyCode::Char('r') => {
                self.mode = Mode::Related;
                self.load();
            }
            KeyCode::Char('R') => self.load(),
            KeyCode::Char('p') if self.mode == Mode::Logs => {
                self.options.previous = !self.options.previous;
                self.load();
            }
            KeyCode::Char('c') if self.mode == Mode::Logs => {
                self.options.container = match &self.options.container {
                    None => self.containers.first().cloned(),
                    Some(current) => self
                        .containers
                        .iter()
                        .position(|c| c == current)
                        .and_then(|i| self.containers.get(i + 1).cloned()),
                };
                self.load();
            }
            KeyCode::Char('s') if self.mode == Mode::Logs => {
                self.editing = Some(Edit::Since);
                self.input = "1h".into();
            }
            KeyCode::Char('/') if self.mode == Mode::Logs => {
                self.editing = Some(Edit::Search);
                self.input = self.filter.clone();
            }
            KeyCode::Char(' ') | KeyCode::Char('f') if self.mode == Mode::Logs => {
                self.follow = !self.follow;
                if self.follow {
                    self.scroll = 0;
                }
            }
            KeyCode::Char('j') if self.mode == Mode::Logs => self.pretty = !self.pretty,
            KeyCode::Char('e') => {
                self.editing = Some(Edit::Export);
                self.input = format!(
                    "{}-{}.{}",
                    self.object.name_any(),
                    chrono::Utc::now().format("%Y%m%d-%H%M%S"),
                    if self.mode == Mode::Logs {
                        "log"
                    } else {
                        "txt"
                    }
                );
            }
            KeyCode::Char('y') => return Effect::Copy(self.export_text()),
            KeyCode::Enter if self.mode == Mode::Related => {
                return self
                    .related
                    .get(self.selected)
                    .cloned()
                    .map(|o| Effect::Navigate(Box::new(o)))
                    .unwrap_or(Effect::None);
            }
            KeyCode::Up | KeyCode::PageUp | KeyCode::Char('k') => {
                self.move_by(-if code == KeyCode::PageUp { 10 } else { 1 })
            }
            KeyCode::Down | KeyCode::PageDown | KeyCode::Char('j') => {
                self.move_by(if code == KeyCode::PageDown { 10 } else { 1 })
            }
            KeyCode::End => {
                self.follow = true;
                self.scroll = 0;
            }
            KeyCode::Home => {
                self.follow = false;
                self.scroll = self
                    .logs
                    .matched()
                    .saturating_sub(self.body.height as usize);
            }
            _ => {}
        }
        Effect::None
    }
    fn move_by(&mut self, delta: i32) {
        if self.mode == Mode::Logs {
            self.follow = false;
            self.scroll = self
                .scroll
                .saturating_add_signed(-(delta as isize))
                .min(self.logs.matched().saturating_sub(1));
            if self.scroll == 0 {
                self.follow = true;
            }
        } else if self.mode == Mode::Related {
            self.selected = self
                .selected
                .saturating_add_signed(delta as isize)
                .min(self.related.len().saturating_sub(1));
            self.scroll = self
                .selected
                .saturating_sub(self.body.height.saturating_sub(1) as usize);
        } else {
            self.scroll = self
                .scroll
                .saturating_add_signed(delta as isize)
                .min(self.text.lines().count().saturating_sub(1));
        }
    }
    pub fn export_text(&self) -> String {
        if self.mode == Mode::Logs {
            self.logs
                .matching()
                .map(|l| format!("[{}] {}\n", l.source, l.text))
                .collect()
        } else if self.mode == Mode::Related {
            self.related
                .iter()
                .map(|o| {
                    format!(
                        "{} {}/{}\n",
                        related::kind(o),
                        o.namespace().unwrap_or_default(),
                        o.name_any()
                    )
                })
                .collect()
        } else {
            self.text.clone()
        }
    }
    pub fn export(&mut self, path: PathBuf) -> anyhow::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(self.export_text().as_bytes())?;
        self.status = format!("Exported {}", path.display());
        Ok(())
    }
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        self.area = area;
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border_style())
            .style(Style::default().bg(theme::ABYSS))
            .title(format!(
                " {} · {}/{} ",
                related::kind(&self.object),
                self.object.namespace().unwrap_or_else(|| "cluster".into()),
                self.object.name_any()
            ));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
        self.body = rows[1];
        self.tab_rects.clear();
        let mut x = rows[0].x;
        for (i, label) in LABELS.iter().enumerate() {
            let text = format!("{} {} ", i + 1, label);
            let width = (text.len() as u16).min(rows[0].right().saturating_sub(x));
            let rect = Rect::new(x, rows[0].y, width, 1);
            self.tab_rects.push(rect);
            f.render_widget(
                Paragraph::new(text).style(if self.mode == MODES[i] {
                    theme::header_style().add_modifier(Modifier::REVERSED)
                } else {
                    theme::muted_style()
                }),
                rect,
            );
            x += width;
        }
        if self.mode == Mode::Logs {
            let lines: Vec<Line> = self
                .logs
                .visible(self.scroll, rows[1].height as usize)
                .into_iter()
                .map(|line| {
                    let body = if self.pretty {
                        line.text
                            .split_once(' ')
                            .and_then(|(_, body)| {
                                serde_json::from_str::<serde_json::Value>(body).ok()
                            })
                            .map(|v| serde_json::to_string(&v).unwrap_or_default())
                            .unwrap_or_else(|| line.text.clone())
                    } else {
                        line.text.clone()
                    };
                    let style = if body.to_lowercase().contains("error") {
                        Style::default().fg(theme::CORAL)
                    } else {
                        theme::text_style()
                    };
                    Line::from(vec![
                        Span::styled(format!("{}  ", line.source), theme::muted_style()),
                        Span::styled(body, style),
                    ])
                })
                .collect();
            f.render_widget(Paragraph::new(lines), rows[1]);
        } else if self.mode == Mode::Related {
            let lines: Vec<Line> = self
                .related
                .iter()
                .enumerate()
                .skip(self.scroll)
                .take(rows[1].height as usize)
                .map(|(i, o)| {
                    Line::styled(
                        format!(
                            "{} {:20} {}",
                            if i == self.selected { "›" } else { " " },
                            related::kind(o),
                            o.name_any()
                        ),
                        if i == self.selected {
                            theme::header_style()
                        } else {
                            theme::text_style()
                        },
                    )
                })
                .collect();
            f.render_widget(
                Paragraph::new(if lines.is_empty() {
                    vec![Line::from(self.text.clone())]
                } else {
                    lines
                }),
                rows[1],
            );
        } else {
            let lines: Vec<Line> = self
                .text
                .lines()
                .skip(self.scroll)
                .take(rows[1].height as usize)
                .map(|s| {
                    let style = if self.mode == Mode::Yaml && s.trim_start().starts_with('#') {
                        theme::muted_style()
                    } else if s.contains(':') {
                        theme::text_style()
                    } else {
                        theme::label_style()
                    };
                    Line::styled(s.to_string(), style)
                })
                .collect();
            f.render_widget(Paragraph::new(lines), rows[1]);
        }
        let status = if self.mode == Mode::Logs {
            format!(
                "{} · {} · {} lines · {:.1} MiB · {} evicted · {}",
                if self.follow { "FOLLOW" } else { "PAUSED" },
                self.options
                    .container
                    .as_deref()
                    .unwrap_or("all containers"),
                self.logs.matched(),
                self.logs.bytes() as f64 / 1048576.,
                self.logs.dropped,
                self.status
            )
        } else {
            self.status.clone()
        };
        f.render_widget(Paragraph::new(status).style(theme::muted_style()), rows[2]);
        self.actions.clear();
        let mut x = rows[3].x;
        for (key, label) in if self.mode == Mode::Logs {
            vec![
                ('f', "Follow"),
                ('c', "Container"),
                ('p', "Previous"),
                ('s', "Since"),
                ('/', "Search"),
                ('j', "JSON"),
                ('e', "Export"),
                ('y', "Copy"),
                ('q', "Close"),
            ]
        } else {
            vec![
                ('l', "Logs"),
                ('r', "Related"),
                ('R', "Refresh"),
                ('e', "Export"),
                ('y', "Copy"),
                ('q', "Close"),
            ]
        } {
            let text = format!("{key} {label}  ");
            let rect = Rect::new(
                x,
                rows[3].y,
                (text.len() as u16).min(rows[3].right().saturating_sub(x)),
                1,
            );
            f.render_widget(Paragraph::new(text).style(theme::label_style()), rect);
            x += rect.width;
            self.actions.push((rect, key));
        }
        if let Some(edit) = self.editing {
            let label = match edit {
                Edit::Search => "Search (literal or re:pattern)",
                Edit::Export => "Export to new file",
                Edit::Since => "Since (30s / 5m / 1h)",
            };
            f.render_widget(
                Paragraph::new(format!(
                    "{label}: {}█  Enter apply · Esc cancel",
                    self.input
                ))
                .style(theme::header_style()),
                rows[3],
            );
        }
    }
}

pub fn parse_duration(value: &str) -> Result<i64, String> {
    let (number, mult) = if let Some(n) = value.strip_suffix('s') {
        (n, 1)
    } else if let Some(n) = value.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = value.strip_suffix('h') {
        (n, 3600)
    } else {
        (value, 1)
    };
    number
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(mult))
        .filter(|n| *n > 0)
        .ok_or_else(|| "Use a positive duration, such as 30s, 5m or 1h".into())
}

pub fn diagnostics(obj: &DynamicObject) -> String {
    let mut lines: Vec<String> = crate::ui::views::detail::overview_rows(obj)
        .into_iter()
        .map(|(k, v)| format!("{k:18} {v}"))
        .collect();
    if let Some(status) = obj.data.get("status") {
        for key in [
            "replicas",
            "readyReplicas",
            "availableReplicas",
            "updatedReplicas",
            "unavailableReplicas",
            "succeeded",
            "failed",
        ] {
            if let Some(value) = status.get(key) {
                lines.push(format!("{key:18} {value}"));
            }
        }
        if let Some(conditions) = status.get("conditions").and_then(|v| v.as_array()) {
            for c in conditions {
                lines.push(format!(
                    "{} = {}  {}\n{}",
                    c["type"].as_str().unwrap_or("condition"),
                    c["status"].as_str().unwrap_or("?"),
                    c["reason"].as_str().unwrap_or(""),
                    c["message"].as_str().unwrap_or("")
                ));
            }
        }
        for key in ["initContainerStatuses", "containerStatuses"] {
            if let Some(containers) = status.get(key).and_then(|v| v.as_array()) {
                for c in containers {
                    lines.push(format!(
                        "\nContainer {} · ready={} · restarts={}",
                        c["name"].as_str().unwrap_or("?"),
                        c["ready"],
                        c["restartCount"]
                    ));
                    for key in ["state", "lastState"] {
                        if let Some(state) = c.get(key) {
                            lines.push(format!("{key}: {state}"));
                        }
                    }
                }
            }
        }
    }
    lines.join("\n")
}
async fn metrics(client: &Client, obj: &DynamicObject) -> anyhow::Result<String> {
    let path = match related::kind(obj) {
        "Pod" => format!(
            "/apis/metrics.k8s.io/v1beta1/namespaces/{}/pods/{}",
            obj.namespace().unwrap_or_default(),
            obj.name_any()
        ),
        "Node" => format!("/apis/metrics.k8s.io/v1beta1/nodes/{}", obj.name_any()),
        _ => {
            return Err(anyhow::anyhow!(
                "Select a Pod or Node for CPU and memory usage"
            ));
        }
    };
    let request = http::Request::get(path).body(Vec::new())?;
    let value: serde_json::Value = client.request(request).await?;
    Ok(serde_json::to_string_pretty(&value)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duration_rejects_zero_negative_and_overflow() {
        assert_eq!(parse_duration("5m").unwrap(), 300);
        for value in ["0", "-1h", "invalid", "99999999999999999h"] {
            assert!(parse_duration(value).is_err());
        }
    }
    #[tokio::test]
    async fn inspector_renders_tabs_and_log_controls_without_a_cluster() {
        let object:DynamicObject=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"demo","uid":"one"},"status":{"phase":"Running"}})).unwrap();
        let client =
            Client::try_from(kube::Config::new("http://127.0.0.1:9".parse().unwrap())).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut inspector = Inspector::new(object, Mode::Overview, client, vec![], tx);
        inspector.mode = Mode::Logs;
        inspector.logs.push(crate::logs::LogLine {
            source: "demo/web/app".into(),
            text: "2026-09-28T00:00:00Z error: example".into(),
        });
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
        terminal.draw(|f| inspector.render(f, f.area())).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        for expected in [
            "Overview",
            "Logs",
            "Related",
            "error: example",
            "FOLLOW",
            "Container",
            "Export",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        inspector.handle(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE,
        )));
        assert!(!inspector.follow);
    }
    #[test]
    fn secret_yaml_and_export_do_not_expose_last_applied_credentials() {
        let object:DynamicObject=serde_json::from_value(serde_json::json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"secret","annotations":{"kubectl.kubernetes.io/last-applied-configuration":"very-secret"}},"data":{"token":"dG9rZW4="},"stringData":{"password":"very-secret"}})).unwrap();
        let yaml = crate::ui::views::detail::object_to_yaml(&object);
        assert!(!yaml.contains("very-secret"));
        assert!(!yaml.contains("dG9rZW4="));
        assert!(yaml.contains("<redacted>"));
    }
}
