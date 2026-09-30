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

        // Row 1 — identity: brand chip, cluster name, namespace chip, then
        // (right side) the read/write mode chip and the command-palette hint.
        let top = Rect::new(area.x, area.y, area.width, area.height.min(1));
        let cells = Layout::horizontal([
            Constraint::Length(9),      // brand chip " ◆ KUBE "
            Constraint::Length(1),      // gap
            Constraint::Fill(1),        // cluster name
            Constraint::Percentage(24), // namespace chip
            Constraint::Length(1),      // gap
            Constraint::Length(13),     // mode chip
            Constraint::Length(1),      // gap
            Constraint::Length(13),     // ^K Commands
        ])
        .split(top);
        f.render_widget(
            Paragraph::new(" ◆ KUBE ").style(
                Style::default()
                    .bg(theme::INDIGO)
                    .fg(theme::PAPER)
                    .add_modifier(Modifier::BOLD),
            ),
            cells[0],
        );
        f.render_widget(
            Paragraph::new(format!(
                "  {} ▾",
                ellipsis(state.context, cells[2].width.saturating_sub(4) as usize)
            ))
            .style(Style::default().fg(theme::cluster_hue(state.context))),
            cells[2],
        );
        self.buttons.push((cells[2], "cluster"));
        f.render_widget(
            Paragraph::new(format!(
                " ns: {} ▾ ",
                ellipsis(state.namespace, cells[3].width.saturating_sub(9) as usize)
            ))
            .style(
                Style::default()
                    .bg(theme::DUSK)
                    .fg(theme::PAPER)
                    .add_modifier(Modifier::BOLD),
            ),
            cells[3],
        );
        self.buttons.push((cells[3], "namespace"));
        f.render_widget(
            Paragraph::new(if state.writable {
                " WRITE ON "
            } else {
                " READ ONLY "
            })
            .style(if state.writable {
                Style::default()
                    .bg(theme::AMBER)
                    .fg(theme::INK)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().bg(theme::SURFACE).fg(theme::TEAL)
            }),
            cells[5],
        );
        f.render_widget(
            Paragraph::new(" ^K Commands ").style(theme::muted_style()),
            cells[7],
        );
        self.buttons.push((cells[7], "palette"));
        if area.height < 2 {
            return;
        }

        // Row 2 — navigation tabs, two-space gaps, with ? Help right-aligned.
        let nav_row = Rect::new(area.x, area.y + 1, area.width, 1);
        let labels = [
            (" F2 Pod monitor ", "home"),
            (" F3 Focus studio ", "browse"),
            (" ^R Resources ", "resources"),
            (" ^Space Ask Kube ", "ask"),
        ];
        let help_label = " ? Help ";
        let help_width = (help_label.len() as u16).min(nav_row.width);
        if nav_row.width > help_width {
            let help_rect = Rect::new(
                nav_row.x + nav_row.width - help_width,
                nav_row.y,
                help_width,
                1,
            );
            f.render_widget(
                Paragraph::new(help_label).style(theme::muted_style()),
                help_rect,
            );
            self.buttons.push((help_rect, "help"));
        }
        let mut x = nav_row.x;
        for (label, command) in labels {
            let width = (label.len() as u16).min(nav_row.width.saturating_sub(x - nav_row.x));
            let rect = Rect::new(x, nav_row.y, width, 1);
            let active = (command == "home" && state.home) || (command == "browse" && !state.home);
            f.render_widget(
                Paragraph::new(label).style(if active {
                    Style::default()
                        .bg(theme::SURFACE)
                        .fg(theme::TEAL)
                        .add_modifier(Modifier::BOLD)
                } else if command == "ask" {
                    Style::default().fg(theme::VIOLET)
                } else {
                    theme::muted_style()
                }),
                rect,
            );
            self.buttons.push((rect, command));
            x += width + 2;
        }
        if area.height < 3 {
            return;
        }

        // Row 3 — context: active kind + count, the filter field, and any
        // transient notice. No duplicate "Tab switch pane" hint: the footer
        // already carries key hints, so an empty notice draws nothing.
        let row = Rect::new(area.x, area.y + 2, area.width, 1);
        let cells = Layout::horizontal([
            Constraint::Length(25.min(area.width / 3)),
            Constraint::Percentage(36),
            Constraint::Fill(1),
        ])
        .split(row);
        f.render_widget(
            ratatui::text::Line::from(vec![
                ratatui::text::Span::raw(" "),
                ratatui::text::Span::styled(
                    state.kind.to_string(),
                    Style::default()
                        .fg(theme::PERIWINKLE)
                        .add_modifier(Modifier::BOLD),
                ),
                ratatui::text::Span::raw("  "),
                ratatui::text::Span::styled(
                    state.count.to_string(),
                    Style::default().fg(theme::VIOLET),
                ),
            ]),
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
        if !state.notice.is_empty() {
            f.render_widget(
                Paragraph::new(state.notice).style(theme::muted_style()),
                cells[2],
            );
        }
    }
}
