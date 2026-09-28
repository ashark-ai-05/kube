//! Nonblocking command drawer with an explicit, scoped action preview.
use crate::{
    app::event::AppEvent,
    assistant::{self, Intent, local::Bundle},
    store::watch::StoreId,
    ui::{log_view::ellipsis, theme},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use kube::{ResourceExt, api::DynamicObject};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tokio::{sync::mpsc, task::JoinHandle};

pub struct Assistant {
    pub open: bool,
    pub query: String,
    pub object: Option<DynamicObject>,
    pub scope: Option<StoreId>,
    context: String,
    namespace: String,
    status: String,
    proposal: Option<Intent>,
    model: Option<Bundle>,
    task: Option<JoinHandle<()>>,
    reply: Option<mpsc::Receiver<Result<Intent, String>>>,
    wake: mpsc::UnboundedSender<AppEvent>,
    apply_area: Rect,
    reviewed: bool,
}
impl Drop for Assistant {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Assistant {
    pub fn new(wake: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            open: false,
            query: String::new(),
            object: None,
            scope: None,
            context: String::new(),
            namespace: String::new(),
            status: String::new(),
            proposal: None,
            model: Bundle::discover(),
            task: None,
            reply: None,
            wake,
            apply_area: Rect::default(),
            reviewed: false,
        }
    }
    fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.reply = None;
    }
    pub fn start(
        &mut self,
        scope: StoreId,
        context: &str,
        namespace: &str,
        object: Option<DynamicObject>,
        query: &str,
    ) {
        self.cancel();
        self.open = true;
        self.scope = Some(scope);
        self.context = context.into();
        self.namespace = namespace.into();
        self.object = object;
        self.query = query.into();
        self.proposal = None;
        self.status =
            "Try: show failing pods · previous logs for this pod · why is this deployment stuck?"
                .into();
        if !query.is_empty() {
            self.submit();
        }
    }
    pub fn sync_scope(&mut self, scope: StoreId) {
        if self.open && self.scope != Some(scope) {
            self.cancel();
            self.scope = None;
            self.proposal = None;
            self.status =
                "Cluster or namespace changed. Close this drawer and ask again in the new scope."
                    .into();
        }
    }
    fn submit(&mut self) {
        self.cancel();
        self.reviewed = false;
        self.proposal = None;
        if self.scope.is_none() {
            return;
        }
        match assistant::parse(&self.query) {
            Ok(Some(intent)) => {
                self.proposal = Some(intent);
                self.status =
                    "Built-in interpretation · review the action, then Enter to apply".into();
            }
            Err(error) => self.status = error,
            Ok(None) => {
                let Some(model) = self.model.clone() else {
                    self.status="This wording needs the optional AI bundle. Try “show failing pods” or use Ctrl-K Commands.".into();
                    return;
                };
                let query = self.query.clone();
                let wake = self.wake.clone();
                let (tx, rx) = mpsc::channel(1);
                self.reply = Some(rx);
                self.status =
                    "Interpreting locally… Esc cancels. Your cluster data stays in the app.".into();
                self.task = Some(tokio::spawn(async move {
                    let result = model.infer(&query).await;
                    if tx.send(result).await.is_ok() {
                        let _ = wake.send(AppEvent::Wake);
                    }
                }));
            }
        }
    }
    pub fn drain(&mut self) -> bool {
        if let Some(rx) = &mut self.reply
            && let Ok(result) = rx.try_recv()
        {
            self.cancel();
            match result {
                Ok(intent) => {
                    self.proposal = Some(intent);
                    self.status="Experimental local suggestion · check the action carefully, then Enter to apply".into();
                }
                Err(e) => self.status = e,
            };
            return true;
        }
        false
    }
    pub fn handle(&mut self, event: &Event) -> Option<Intent> {
        if let Event::Mouse(m) = event
            && m.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
            && self.apply_area.contains((m.column, m.row).into())
            && self.proposal.is_some()
            && self.reviewed
        {
            self.open = false;
            return self.proposal.take();
        }
        if let Event::Key(k) = event
            && k.kind == KeyEventKind::Press
        {
            match k.code {
                KeyCode::Esc => {
                    self.cancel();
                    self.open = false;
                    self.proposal = None;
                }
                KeyCode::Enter if self.proposal.is_some() && self.reviewed => {
                    self.open = false;
                    return self.proposal.take();
                }
                KeyCode::Enter if self.task.is_none() => self.submit(),
                KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.cancel();
                    self.query.clear();
                    self.proposal = None;
                }
                KeyCode::Char(c)
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && self.query.len() + c.len_utf8() <= 512 =>
                {
                    self.cancel();
                    self.query.push(c);
                    self.proposal = None;
                }
                KeyCode::Backspace => {
                    self.cancel();
                    self.query.pop();
                    self.proposal = None;
                }
                _ => {}
            }
        }
        None
    }
    pub fn render(&mut self, f: &mut Frame) {
        if !self.open {
            return;
        }
        let full = f.area();
        let height = 12.min(full.height);
        let area = Rect::new(
            1.min(full.width),
            full.height - height,
            full.width.saturating_sub(2),
            height,
        );
        f.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::VIOLET))
            .style(Style::default().bg(theme::ABYSS).fg(theme::PAPER))
            .title(if self.model.is_some() {
                " Ask Kube · local model available "
            } else {
                " Ask Kube · built-in commands "
            });
        let inner = block.inner(area);
        f.render_widget(block, area);
        let rows = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let selected = self
            .object
            .as_ref()
            .map(|o| o.name_any())
            .unwrap_or_else(|| "none".into());
        f.render_widget(
            Paragraph::new(format!(
                "{} · {} · selected: {}",
                self.context, self.namespace, selected
            ))
            .style(theme::muted_style()),
            rows[0],
        );
        // Keep the cursor and end of long input visible without slicing UTF-8.
        let query: String = self
            .query
            .chars()
            .rev()
            .take(rows[1].width.saturating_sub(5) as usize)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        f.render_widget(
            Paragraph::new(format!("> {query}▏")).style(Style::default().fg(theme::PAPER)),
            rows[1],
        );
        let mut lines = vec![Line::styled(self.status.clone(), theme::muted_style())];
        if let Some(intent) = &self.proposal {
            lines.push(Line::from(""));
            lines.push(Line::styled(
                ellipsis(&intent.describe(), rows[2].width as usize),
                theme::label_style(),
            ));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[2]);
        self.reviewed = self.proposal.is_some() && rows[2].height >= 3 && rows[2].width >= 30;
        self.apply_area = if self.reviewed {
            rows[3]
        } else {
            Rect::default()
        };
        f.render_widget(
            Paragraph::new(if self.proposal.is_some() {
                " Enter apply this action · type to revise · Esc cancel "
            } else {
                " Enter interpret · Ctrl-U clear · Esc close "
            })
            .style(Style::default().fg(theme::VIOLET)),
            rows[3],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::watch::ResourceStore;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    fn enter() -> Event {
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))
    }
    #[test]
    fn interpretation_requires_a_second_enter_and_scope_changes_invalidate_it() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut assistant = Assistant::new(tx);
        let first = Arc::new(RwLock::new(ResourceStore::new()));
        let second = Arc::new(RwLock::new(ResourceStore::new()));
        assistant.start(StoreId::of(&first), "dev", "demo", None, "");
        for c in "show pods".chars() {
            assert!(
                assistant
                    .handle(&Event::Key(crossterm::event::KeyEvent::new(
                        KeyCode::Char(c),
                        KeyModifiers::NONE
                    )))
                    .is_none()
            );
        }
        assert!(assistant.handle(&enter()).is_none());
        assert!(assistant.proposal.is_some());
        assistant.sync_scope(StoreId::of(&second));
        assert!(assistant.handle(&enter()).is_none());
        assert!(assistant.proposal.is_none());
    }
    #[test]
    fn editing_a_preview_removes_its_executable_action() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut assistant = Assistant::new(tx);
        let store = Arc::new(RwLock::new(ResourceStore::new()));
        assistant.start(StoreId::of(&store), "dev", "demo", None, "show pods");
        assert!(assistant.proposal.is_some());
        assistant.handle(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Backspace,
            KeyModifiers::NONE,
        )));
        assert!(assistant.proposal.is_none());
    }
    #[test]
    fn a_preview_must_be_painted_before_enter_can_apply_it() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut assistant = Assistant::new(tx);
        let store = Arc::new(RwLock::new(ResourceStore::new()));
        assistant.start(StoreId::of(&store), "dev", "demo", None, "show pods");
        assert!(assistant.handle(&enter()).is_none());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| assistant.render(f)).unwrap();
        assert!(assistant.handle(&enter()).is_some());
    }
}
