use crate::ui::hit::{HitRegistry, HitTarget};
use crossterm::event::{Event as CtEvent, KeyCode, MouseButton, MouseEventKind};

/// Scroll wheel step, in rows. Matches typical terminal conventions.
const SCROLL_STEP: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SelectRow(usize),
    ScrollBy(i32),
    SortByColumn(usize),
    Quit,
    None,
}

/// Translate a raw input event into an action, resolving mouse position
/// through the current frame's hit registry.
pub fn action_for(event: &CtEvent, hits: &HitRegistry) -> Action {
    match event {
        CtEvent::Mouse(m) => match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(i)) => Action::SelectRow(*i),
                Some(HitTarget::ColumnHeader(i)) => Action::SortByColumn(*i),
                _ => Action::None,
            },
            // Scroll applies to whatever is under the cursor, not to focus.
            MouseEventKind::ScrollDown => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(SCROLL_STEP)
                }
                _ => Action::None,
            },
            MouseEventKind::ScrollUp => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(-SCROLL_STEP)
                }
                _ => Action::None,
            },
            _ => Action::None,
        },
        CtEvent::Key(k) => match k.code {
            KeyCode::Down | KeyCode::Char('j') => Action::ScrollBy(1),
            KeyCode::Up | KeyCode::Char('k') => Action::ScrollBy(-1),
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::None,
        },
        _ => Action::None,
    }
}

/// Move a selection index by `delta`, clamped to the list.
pub fn apply_selection(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as i64 + delta as i64;
    next.clamp(0, len as i64 - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use ratatui::layout::Rect;

    fn registry() -> HitRegistry {
        let mut r = HitRegistry::new();
        r.push(
            Rect {
                x: 0,
                y: 2,
                width: 40,
                height: 1,
            },
            0,
            HitTarget::TableRow(0),
        );
        r.push(
            Rect {
                x: 0,
                y: 3,
                width: 40,
                height: 1,
            },
            0,
            HitTarget::TableRow(1),
        );
        r.push(
            Rect {
                x: 0,
                y: 1,
                width: 40,
                height: 1,
            },
            0,
            HitTarget::ColumnHeader(2),
        );
        r
    }

    fn click(col: u16, row: u16) -> CtEvent {
        CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn scroll(kind: MouseEventKind, col: u16, row: u16) -> CtEvent {
        CtEvent::Mouse(MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn key(code: KeyCode) -> CtEvent {
        CtEvent::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn clicking_a_row_selects_it() {
        assert_eq!(action_for(&click(5, 3), &registry()), Action::SelectRow(1));
    }

    #[test]
    fn clicking_a_header_sorts_by_that_column() {
        assert_eq!(
            action_for(&click(5, 1), &registry()),
            Action::SortByColumn(2)
        );
    }

    #[test]
    fn clicking_empty_space_does_nothing() {
        assert_eq!(action_for(&click(5, 40), &registry()), Action::None);
    }

    #[test]
    fn scrolling_over_the_table_scrolls_it() {
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollDown, 5, 3), &registry()),
            Action::ScrollBy(3)
        );
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollUp, 5, 3), &registry()),
            Action::ScrollBy(-3)
        );
    }

    #[test]
    fn scrolling_over_nothing_does_nothing() {
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollDown, 5, 40), &registry()),
            Action::None,
            "scroll targets the region under the cursor, not the focused pane"
        );
    }

    #[test]
    fn arrow_and_vim_keys_move_the_selection() {
        assert_eq!(
            action_for(&key(KeyCode::Down), &registry()),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &registry()),
            Action::ScrollBy(-1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('j')), &registry()),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('k')), &registry()),
            Action::ScrollBy(-1)
        );
    }

    #[test]
    fn q_and_esc_quit() {
        assert_eq!(
            action_for(&key(KeyCode::Char('q')), &registry()),
            Action::Quit
        );
        assert_eq!(action_for(&key(KeyCode::Esc), &registry()), Action::Quit);
    }

    #[test]
    fn selection_clamps_at_both_ends() {
        assert_eq!(apply_selection(0, -1, 5), 0, "must not wrap past the top");
        assert_eq!(apply_selection(4, 1, 5), 4, "must not wrap past the bottom");
        assert_eq!(apply_selection(2, 1, 5), 3);
        assert_eq!(apply_selection(2, -1, 5), 1);
        assert_eq!(
            apply_selection(0, 10, 5),
            4,
            "a big jump clamps to the last row"
        );
    }

    #[test]
    fn selection_on_an_empty_list_stays_at_zero() {
        assert_eq!(
            apply_selection(0, 1, 0),
            0,
            "an empty table must not index out of bounds"
        );
    }
}
