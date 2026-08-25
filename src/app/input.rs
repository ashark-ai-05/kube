use crate::ui::hit::{HitRegistry, HitTarget};
use crossterm::event::{
    Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

/// Scroll wheel step, in rows. Matches typical terminal conventions.
const SCROLL_STEP: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SelectRow(usize),
    ScrollBy(i32),
    SortByColumn(usize),
    Quit,
    OpenClusterPicker,
    OpenNamespacePicker,
    /// Index into the picker's FILTERED list — the caller must map it back
    /// through `filtered_indices` before acting on it.
    PickerSelect(usize),
    /// Enter: confirm whatever row the picker currently has highlighted
    /// (`Picker::selected`, also a filtered-list index).
    PickerConfirm,
    PickerFilterChar(char),
    PickerBackspace,
    ClosePicker,
    None,
}

/// Translate a raw input event into an action, resolving mouse position
/// through the current frame's hit registry.
///
/// `overlay_open` is an explicit parameter rather than state this function
/// tracks itself. Whether a picker has input focus changes what almost
/// every key means — `Esc` closes it instead of quitting, and every
/// character (including `j`, `k`, `c`, `n`, `q`) becomes filter text
/// instead of navigation, open, or quit — so the caller, which owns that
/// state, passes it in rather than reinterpreting a stateless mapper's
/// output after the fact.
pub fn action_for(event: &CtEvent, hits: &HitRegistry, overlay_open: bool) -> Action {
    match event {
        CtEvent::Mouse(m) => match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match hits.hit(m.column, m.row) {
                Some(HitTarget::PickerRow(i)) => Action::PickerSelect(*i),
                Some(HitTarget::Ribbon) => Action::OpenClusterPicker,
                Some(HitTarget::TableRow(i)) if !overlay_open => Action::SelectRow(*i),
                Some(HitTarget::ColumnHeader(i)) if !overlay_open => Action::SortByColumn(*i),
                _ => Action::None,
            },
            // Scroll applies to whatever is under the cursor, not to focus —
            // but suppressed while a picker is open: the table beneath it is
            // not what the user is looking at.
            MouseEventKind::ScrollDown if !overlay_open => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(SCROLL_STEP)
                }
                _ => Action::None,
            },
            MouseEventKind::ScrollUp if !overlay_open => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(-SCROLL_STEP)
                }
                _ => Action::None,
            },
            _ => Action::None,
        },
        CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
            // Raw mode disables the terminal's own SIGINT handling, so
            // Ctrl-C must keep working as an emergency quit no matter what
            // has focus.
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                return Action::Quit;
            }
            if overlay_open {
                match k.code {
                    KeyCode::Esc => Action::ClosePicker,
                    KeyCode::Backspace => Action::PickerBackspace,
                    KeyCode::Enter => Action::PickerConfirm,
                    KeyCode::Up => Action::ScrollBy(-1),
                    KeyCode::Down => Action::ScrollBy(1),
                    // Every other character — including j, k, c, n, q — is
                    // filter text, not a binding. A picker whose own
                    // alphabet doubled as bindings would make some cluster
                    // names untypeable.
                    KeyCode::Char(c) => Action::PickerFilterChar(c),
                    _ => Action::None,
                }
            } else {
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => Action::ScrollBy(1),
                    KeyCode::Up | KeyCode::Char('k') => Action::ScrollBy(-1),
                    KeyCode::Char('c') => Action::OpenClusterPicker,
                    KeyCode::Char('n') => Action::OpenNamespacePicker,
                    KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                    _ => Action::None,
                }
            }
        }
        _ => Action::None,
    }
}

/// Move a selection index by `delta`, clamped to the list.
///
/// Uses `i128` intermediates so that no `usize` input can wrap or panic,
/// including values above `i64::MAX`.
pub fn apply_selection(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let last = (len - 1) as i128;
    let next = current as i128 + delta as i128;
    next.clamp(0, last) as usize
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

    fn key_kind(code: KeyCode, kind: KeyEventKind) -> CtEvent {
        CtEvent::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn clicking_a_row_selects_it() {
        assert_eq!(
            action_for(&click(5, 3), &registry(), false),
            Action::SelectRow(1)
        );
    }

    #[test]
    fn clicking_a_header_sorts_by_that_column() {
        assert_eq!(
            action_for(&click(5, 1), &registry(), false),
            Action::SortByColumn(2)
        );
    }

    #[test]
    fn clicking_empty_space_does_nothing() {
        assert_eq!(action_for(&click(5, 40), &registry(), false), Action::None);
    }

    #[test]
    fn scrolling_over_the_table_scrolls_it() {
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollDown, 5, 3),
                &registry(),
                false
            ),
            Action::ScrollBy(3)
        );
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollUp, 5, 3), &registry(), false),
            Action::ScrollBy(-3)
        );
    }

    #[test]
    fn scrolling_over_nothing_does_nothing() {
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollDown, 5, 40),
                &registry(),
                false
            ),
            Action::None,
            "scroll targets the region under the cursor, not the focused pane"
        );
    }

    #[test]
    fn arrow_and_vim_keys_move_the_selection() {
        assert_eq!(
            action_for(&key(KeyCode::Down), &registry(), false),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &registry(), false),
            Action::ScrollBy(-1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('j')), &registry(), false),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('k')), &registry(), false),
            Action::ScrollBy(-1)
        );
    }

    #[test]
    fn q_and_esc_quit() {
        assert_eq!(
            action_for(&key(KeyCode::Char('q')), &registry(), false),
            Action::Quit
        );
        assert_eq!(
            action_for(&key(KeyCode::Esc), &registry(), false),
            Action::Quit
        );
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

    #[test]
    fn key_release_events_are_ignored_so_one_press_acts_once() {
        // Windows and the Kitty keyboard protocol emit Press AND Release for a
        // single keystroke. Acting on both moves the selection two rows per press.
        let r = registry();
        assert_eq!(
            action_for(
                &key_kind(KeyCode::Char('j'), KeyEventKind::Press),
                &r,
                false
            ),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(
                &key_kind(KeyCode::Char('j'), KeyEventKind::Release),
                &r,
                false
            ),
            Action::None
        );
        assert_eq!(
            action_for(
                &key_kind(KeyCode::Char('q'), KeyEventKind::Release),
                &r,
                false
            ),
            Action::None
        );
    }

    #[test]
    fn ctrl_c_quits_since_raw_mode_disables_the_signal() {
        let ev = CtEvent::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(action_for(&ev, &registry(), false), Action::Quit);
    }

    #[test]
    fn ctrl_c_quits_even_with_a_picker_open() {
        // Raw mode disables the terminal's own SIGINT handling; the
        // emergency quit must stay reachable no matter what has focus.
        let ev = CtEvent::Key(KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(action_for(&ev, &registry(), true), Action::Quit);
    }

    #[test]
    fn only_the_left_button_selects() {
        let r = registry();
        for button in [MouseButton::Right, MouseButton::Middle] {
            let ev = CtEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Down(button),
                column: 5,
                row: 3,
                modifiers: KeyModifiers::NONE,
            });
            assert_eq!(
                action_for(&ev, &r, false),
                Action::None,
                "{button:?} must not select a row"
            );
        }
    }

    #[test]
    fn selection_does_not_panic_at_the_extremes_of_usize() {
        assert_eq!(apply_selection(usize::MAX, 1, usize::MAX), usize::MAX - 1);
        assert_eq!(apply_selection(0, i32::MIN, 10), 0);
        assert_eq!(apply_selection(usize::MAX, i32::MAX, 10), 9);
    }

    // --- Task 9: overlays, focus, and input ---

    #[test]
    fn c_opens_the_cluster_picker_and_n_the_namespace_picker() {
        let r = registry();
        assert_eq!(
            action_for(&key(KeyCode::Char('c')), &r, false),
            Action::OpenClusterPicker
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('n')), &r, false),
            Action::OpenNamespacePicker
        );
    }

    #[test]
    fn clicking_the_ribbon_opens_the_cluster_picker() {
        let mut r = HitRegistry::new();
        r.push(
            Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 24,
            },
            0,
            HitTarget::Ribbon,
        );
        assert_eq!(
            action_for(&click(0, 5), &r, false),
            Action::OpenClusterPicker
        );
    }

    #[test]
    fn clicking_a_picker_row_selects_it() {
        let mut r = HitRegistry::new();
        r.push(
            Rect {
                x: 10,
                y: 5,
                width: 40,
                height: 1,
            },
            1,
            HitTarget::PickerRow(3),
        );
        assert_eq!(action_for(&click(20, 5), &r, true), Action::PickerSelect(3));
    }

    #[test]
    fn escape_closes_an_open_picker() {
        assert_eq!(
            action_for(&key(KeyCode::Esc), &registry(), true),
            Action::ClosePicker
        );
    }

    #[test]
    fn while_a_picker_is_open_every_character_becomes_filter_text() {
        // j, k, c, n and q are all real characters someone might type into a
        // cluster or namespace filter (e.g. "k8s-jkc-north"). Ignoring
        // `overlay_open` here would intercept them as navigation/open/quit
        // instead of routing them to the filter — this is the case that
        // actually distinguishes "focus routes to the picker" from "focus is
        // ignored", not merely Esc.
        let r = registry();
        for c in ['j', 'k', 'c', 'n', 'q'] {
            assert_eq!(
                action_for(&key(KeyCode::Char(c)), &r, true),
                Action::PickerFilterChar(c),
                "'{c}' must become filter text while a picker is open"
            );
        }
    }

    #[test]
    fn backspace_erases_the_pickers_filter() {
        assert_eq!(
            action_for(&key(KeyCode::Backspace), &registry(), true),
            Action::PickerBackspace
        );
    }

    #[test]
    fn enter_confirms_the_pickers_highlighted_row() {
        assert_eq!(
            action_for(&key(KeyCode::Enter), &registry(), true),
            Action::PickerConfirm
        );
    }

    #[test]
    fn up_and_down_navigate_the_open_picker_rather_than_typing() {
        let r = registry();
        assert_eq!(
            action_for(&key(KeyCode::Down), &r, true),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &r, true),
            Action::ScrollBy(-1)
        );
    }

    #[test]
    fn table_clicks_and_scrolls_are_suppressed_while_a_picker_is_open() {
        let r = registry();
        assert_eq!(
            action_for(&click(5, 3), &r, true),
            Action::None,
            "a table row click must not select while a picker has focus"
        );
        assert_eq!(
            action_for(&click(5, 1), &r, true),
            Action::None,
            "a header click must not sort while a picker has focus"
        );
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollDown, 5, 3), &r, true),
            Action::None,
            "scroll must not move the table while a picker has focus"
        );
    }
}
