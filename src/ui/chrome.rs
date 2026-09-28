use crate::ui::{log_view::ellipsis, theme};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    widgets::{Block, Paragraph},
};
pub const HEADER_HEIGHT: u16 = 3;
#[derive(Default)]
pub struct Header {
    buttons: Vec<(Rect, &'static str)>,
}
pub struct HeaderState<'a> {
    pub context: &'a str,
    pub namespace: &'a str,
    pub kind: &'a str,
    pub count: usize,
    pub writable: bool,
    pub filter: &'a str,
    pub editing: bool,
    pub notice: &'a str,
    pub home: bool,
}
impl Header {
    pub fn command_at(&self, x: u16, y: u16) -> Option<&'static str> {
        self.buttons
            .iter()
            .find(|(area, _)| area.contains((x, y).into()))
            .map(|(_, command)| *command)
    }
    pub fn render(&mut self, f: &mut Frame, state: HeaderState<'_>) {
        self.buttons.clear();
        let area = Rect::new(0, 0, f.area().width, HEADER_HEIGHT.min(f.area().height));
        f.render_widget(
            Block::default().style(Style::default().bg(theme::ABYSS).fg(theme::PAPER)),
            area,
        );
        let top = Rect::new(area.x, area.y, area.width, area.height.min(1));
        let cells = Layout::horizontal([
            Constraint::Length(7),
            Constraint::Fill(1),
            Constraint::Percentage(28),
            Constraint::Length(12),
            Constraint::Length(15),
        ])
        .split(top);
        f.render_widget(
            Paragraph::new(" KUBE ").style(theme::header_style()),
            cells[0],
        );
        f.render_widget(
            Paragraph::new(format!(
                " {} ▾",
                ellipsis(state.context, cells[1].width.saturating_sub(3) as usize)
            ))
            .style(Style::default().fg(theme::cluster_hue(state.context))),
            cells[1],
        );
        self.buttons.push((cells[1], "cluster"));
        f.render_widget(
            Paragraph::new(format!(
                " ns: {} ▾",
                ellipsis(state.namespace, cells[2].width.saturating_sub(8) as usize)
            ))
            .style(
                Style::default()
                    .bg(theme::DUSK)
                    .fg(theme::PAPER)
                    .add_modifier(Modifier::BOLD),
            ),
            cells[2],
        );
        self.buttons.push((cells[2], "namespace"));
        f.render_widget(
            Paragraph::new(if state.writable {
                "  WRITE ON"
            } else {
                "  READ ONLY"
            })
            .style(Style::default().fg(if state.writable {
                theme::AMBER
            } else {
                theme::TEAL
            })),
            cells[3],
        );
        f.render_widget(
            Paragraph::new(" ^K Commands ").style(theme::muted_style()),
            cells[4],
        );
        self.buttons.push((cells[4], "palette"));
        if area.height < 2 {
            return;
        }
        let row = Rect::new(area.x, area.y + 1, area.width, 1);
        let cells = Layout::horizontal([
            Constraint::Length(25.min(area.width / 3)),
            Constraint::Percentage(36),
            Constraint::Fill(1),
            Constraint::Length(8),
        ])
        .split(row);
        f.render_widget(
            Paragraph::new(format!(" {}  {}", state.kind, state.count)).style(theme::text_style()),
            cells[0],
        );
        let query = if state.editing {
            format!(" / {}▏", state.filter)
        } else if state.filter.is_empty() {
            " / Filter resources…".into()
        } else {
            format!(" / {}  · active", state.filter)
        };
        f.render_widget(
            Paragraph::new(query).style(if state.editing {
                theme::header_style()
            } else {
                theme::muted_style()
            }),
            cells[1],
        );
        self.buttons.push((cells[1], "filter"));
        f.render_widget(
            Paragraph::new(if state.notice.is_empty() {
                "Tab switch pane · Enter inspect"
            } else {
                state.notice
            })
            .style(theme::muted_style()),
            cells[2],
        );
        f.render_widget(
            Paragraph::new(" ? Help ").style(theme::muted_style()),
            cells[3],
        );
        self.buttons.push((cells[3], "help"));
        if area.height > 2 {
            let labels = [
                (" F2 Fleet radar ", "home"),
                (" F3 Focus studio ", "browse"),
                (" ^R Resources ", "resources"),
                (" ^Space Ask Kube ", "ask"),
            ];
            let mut x = 1;
            for (label, command) in labels {
                let width = (label.len() as u16).min(area.width.saturating_sub(x));
                let rect = Rect::new(x, 2, width, 1);
                let active =
                    (command == "home" && state.home) || (command == "browse" && !state.home);
                f.render_widget(
                    Paragraph::new(label).style(if active {
                        theme::label_style().bg(theme::SURFACE)
                    } else if command == "ask" {
                        Style::default().fg(theme::VIOLET)
                    } else {
                        theme::muted_style()
                    }),
                    rect,
                );
                self.buttons.push((rect, command));
                x += width + 1;
            }
        }
    }
}
