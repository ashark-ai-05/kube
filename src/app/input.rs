use crate::ui::hit::{HitRegistry, HitTarget};
use crossterm::event::{
    Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

/// Scroll wheel step, in rows. Matches typical terminal conventions.
const SCROLL_STEP: i32 = 3;

/// Which pane input is currently addressed to.
///
/// Replaces the `overlay_open: bool` this function used to take. A boolean
/// answered exactly one question — "does the picker have focus" — and there
/// are now four panes that change what a keystroke means: `j` moves the table
/// selection, the sidebar selection, scrolls the detail pane's YAML, or types
/// a letter into a picker filter, depending only on this. Encoding that as
/// two or three booleans would make the illegal combinations (a picker AND a
/// detail pane both focused) representable; an enum makes focus one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The kind tree on the left.
    Sidebar,
    /// The object table.
    Table,
    /// The detail pane, drawn over the table.
    Detail,
    /// A modal picker (clusters or namespaces), drawn over everything.
    Picker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SelectRow(usize),
    ScrollBy(i32),
    SortByColumn(usize),
    /// Move focus to the next pane (`Tab`).
    ToggleFocus,
    /// Click on a sidebar row — an index into `ui::tree::flatten`'s output,
    /// absolute rather than screen-relative (see `HitTarget::TreeRow`).
    SelectTreeRow(usize),
    /// Move the sidebar selection by a delta, clamped. Distinct from
    /// `ScrollBy` because the two lists are independent and both are on
    /// screen at once — one action for "move a selection" would have to be
    /// disambiguated by focus at every call site instead of once here.
    ScrollTree(i32),
    /// Act on the sidebar's selected row: expand/collapse a group, or make a
    /// kind the active one.
    ActivateTreeRow,
    /// Open the detail pane on the table's selected row.
    OpenDetail,
    CloseDetail,
    /// Click on one of the detail pane's tabs, in `TAB_ORDER` position.
    SelectDetailTab(usize),
    /// Move the detail pane's active tab by a delta, wrapping.
    CycleDetailTab(i32),
    /// Scroll the detail pane's active tab's content by a delta in rows.
    ScrollDetail(i32),
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
/// `focus` is an explicit parameter rather than state this function tracks
/// itself. Which pane has input changes what almost every key means — `Esc`
/// closes a picker instead of quitting, every character (including `j`, `k`,
/// `c`, `n`, `q`) becomes filter text instead of navigation, `j` moves the
/// sidebar rather than the table — so the caller, which owns that state,
/// passes it in rather than reinterpreting a stateless mapper's output after
/// the fact.
pub fn action_for(event: &CtEvent, hits: &HitRegistry, focus: Focus) -> Action {
    let overlay_open = matches!(focus, Focus::Picker);
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
            action_for(&click(5, 3), &registry(), Focus::Table),
            Action::SelectRow(1)
        );
    }

    #[test]
    fn clicking_a_header_sorts_by_that_column() {
        assert_eq!(
            action_for(&click(5, 1), &registry(), Focus::Table),
            Action::SortByColumn(2)
        );
    }

    #[test]
    fn clicking_empty_space_does_nothing() {
        assert_eq!(
            action_for(&click(5, 40), &registry(), Focus::Table),
            Action::None
        );
    }

    #[test]
    fn scrolling_over_the_table_scrolls_it() {
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollDown, 5, 3),
                &registry(),
                Focus::Table
            ),
            Action::ScrollBy(3)
        );
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollUp, 5, 3),
                &registry(),
                Focus::Table
            ),
            Action::ScrollBy(-3)
        );
    }

    #[test]
    fn scrolling_over_nothing_does_nothing() {
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollDown, 5, 40),
                &registry(),
                Focus::Table
            ),
            Action::None,
            "scroll targets the region under the cursor, not the focused pane"
        );
    }

    #[test]
    fn arrow_and_vim_keys_move_the_selection() {
        assert_eq!(
            action_for(&key(KeyCode::Down), &registry(), Focus::Table),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &registry(), Focus::Table),
            Action::ScrollBy(-1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('j')), &registry(), Focus::Table),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('k')), &registry(), Focus::Table),
            Action::ScrollBy(-1)
        );
    }

    #[test]
    fn q_and_esc_quit() {
        assert_eq!(
            action_for(&key(KeyCode::Char('q')), &registry(), Focus::Table),
            Action::Quit
        );
        assert_eq!(
            action_for(&key(KeyCode::Esc), &registry(), Focus::Table),
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
                Focus::Table
            ),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(
                &key_kind(KeyCode::Char('j'), KeyEventKind::Release),
                &r,
                Focus::Table
            ),
            Action::None
        );
        assert_eq!(
            action_for(
                &key_kind(KeyCode::Char('q'), KeyEventKind::Release),
                &r,
                Focus::Table
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
        assert_eq!(action_for(&ev, &registry(), Focus::Table), Action::Quit);
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
        assert_eq!(action_for(&ev, &registry(), Focus::Picker), Action::Quit);
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
                action_for(&ev, &r, Focus::Table),
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
            action_for(&key(KeyCode::Char('c')), &r, Focus::Table),
            Action::OpenClusterPicker
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('n')), &r, Focus::Table),
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
            action_for(&click(0, 5), &r, Focus::Table),
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
        assert_eq!(
            action_for(&click(20, 5), &r, Focus::Picker),
            Action::PickerSelect(3)
        );
    }

    #[test]
    fn escape_closes_an_open_picker() {
        assert_eq!(
            action_for(&key(KeyCode::Esc), &registry(), Focus::Picker),
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
                action_for(&key(KeyCode::Char(c)), &r, Focus::Picker),
                Action::PickerFilterChar(c),
                "'{c}' must become filter text while a picker is open"
            );
        }
    }

    #[test]
    fn backspace_erases_the_pickers_filter() {
        assert_eq!(
            action_for(&key(KeyCode::Backspace), &registry(), Focus::Picker),
            Action::PickerBackspace
        );
    }

    #[test]
    fn enter_confirms_the_pickers_highlighted_row() {
        assert_eq!(
            action_for(&key(KeyCode::Enter), &registry(), Focus::Picker),
            Action::PickerConfirm
        );
    }

    #[test]
    fn up_and_down_navigate_the_open_picker_rather_than_typing() {
        let r = registry();
        assert_eq!(
            action_for(&key(KeyCode::Down), &r, Focus::Picker),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &r, Focus::Picker),
            Action::ScrollBy(-1)
        );
    }

    // --- Task 10: sidebar and detail pane focus ---

    /// A registry laid out like a real frame: a sidebar row on the left, a
    /// table row and a column header to its right, and the detail pane's tab
    /// and close zones (z=1, as `render_detail` registers them) over the
    /// table. Every new binding is exercised against the SAME registry, so a
    /// mapping that happens to work only because its zone is the only one
    /// present cannot pass.
    fn full_registry() -> HitRegistry {
        let mut r = HitRegistry::new();
        r.push(
            Rect {
                x: 0,
                y: 3,
                width: 20,
                height: 1,
            },
            0,
            HitTarget::TreeRow(4),
        );
        r.push(
            Rect {
                x: 20,
                y: 1,
                width: 40,
                height: 1,
            },
            0,
            HitTarget::ColumnHeader(2),
        );
        r.push(
            Rect {
                x: 20,
                y: 3,
                width: 40,
                height: 1,
            },
            0,
            HitTarget::TableRow(7),
        );
        r.push(
            Rect {
                x: 22,
                y: 1,
                width: 6,
                height: 1,
            },
            1,
            HitTarget::DetailTab(2),
        );
        r.push(
            Rect {
                x: 57,
                y: 1,
                width: 3,
                height: 1,
            },
            1,
            HitTarget::DetailClose,
        );
        r
    }

    #[test]
    fn clicking_a_sidebar_row_selects_that_kind_row() {
        // The sidebar is always visible, so its rows stay clickable from the
        // table and from an open detail pane — only a modal picker suppresses
        // them.
        for focus in [Focus::Sidebar, Focus::Table, Focus::Detail] {
            assert_eq!(
                action_for(&click(5, 3), &full_registry(), focus),
                Action::SelectTreeRow(4),
                "a sidebar click must reach the tree from {focus:?}"
            );
        }
        assert_eq!(
            action_for(&click(5, 3), &full_registry(), Focus::Picker),
            Action::None,
            "a modal picker must swallow sidebar clicks like every other pane's"
        );
    }

    #[test]
    fn clicking_a_detail_tab_selects_it_and_the_close_box_closes_the_pane() {
        // Both must be reachable by mouse: `Esc` alone is not enough for a
        // mouse-driven tool.
        assert_eq!(
            action_for(&click(24, 1), &full_registry(), Focus::Detail),
            Action::SelectDetailTab(2)
        );
        assert_eq!(
            action_for(&click(58, 1), &full_registry(), Focus::Detail),
            Action::CloseDetail
        );
    }

    #[test]
    fn a_detail_tab_click_wins_over_the_column_header_beneath_it() {
        // The tab zone (z=1) and the table's column-header zone (z=0) overlap
        // at x=24,y=1 in `full_registry`. Resolving to the header would sort
        // the table behind a pane the user is looking at — and a fixture
        // where the two did NOT overlap could not tell the two orderings
        // apart at all.
        assert_ne!(
            action_for(&click(24, 1), &full_registry(), Focus::Detail),
            Action::SortByColumn(2),
            "the pane on top must capture the click"
        );
    }

    #[test]
    fn the_table_is_not_clickable_underneath_an_open_detail_pane() {
        assert_eq!(
            action_for(&click(30, 3), &full_registry(), Focus::Detail),
            Action::None,
            "a click that lands on the table while the detail pane covers it \
             must not move a selection the user cannot see"
        );
    }

    #[test]
    fn enter_on_the_table_opens_the_detail_pane_and_escape_closes_it() {
        assert_eq!(
            action_for(&key(KeyCode::Enter), &full_registry(), Focus::Table),
            Action::OpenDetail
        );
        assert_eq!(
            action_for(&key(KeyCode::Esc), &full_registry(), Focus::Detail),
            Action::CloseDetail,
            "Esc must close the pane rather than quitting the application"
        );
    }

    #[test]
    fn tab_moves_focus_between_the_sidebar_and_the_table() {
        for focus in [Focus::Sidebar, Focus::Table] {
            assert_eq!(
                action_for(&key(KeyCode::Tab), &full_registry(), focus),
                Action::ToggleFocus,
                "from {focus:?}"
            );
        }
    }

    #[test]
    fn the_sidebar_is_fully_navigable_from_the_keyboard() {
        // Every kind must be reachable without a mouse: move the selection,
        // then act on it (expand a group, or make a kind active).
        let r = full_registry();
        assert_eq!(
            action_for(&key(KeyCode::Down), &r, Focus::Sidebar),
            Action::ScrollTree(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('j')), &r, Focus::Sidebar),
            Action::ScrollTree(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Up), &r, Focus::Sidebar),
            Action::ScrollTree(-1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Enter), &r, Focus::Sidebar),
            Action::ActivateTreeRow
        );
        assert_eq!(
            action_for(&key(KeyCode::Char(' ')), &r, Focus::Sidebar),
            Action::ActivateTreeRow
        );
    }

    #[test]
    fn sidebar_keys_do_not_also_move_the_table() {
        // The two lists are on screen simultaneously. `j` with the sidebar
        // focused must move the sidebar only — an implementation that ignored
        // focus for navigation would return `ScrollBy` here and move both.
        assert_ne!(
            action_for(&key(KeyCode::Char('j')), &full_registry(), Focus::Sidebar),
            Action::ScrollBy(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('j')), &full_registry(), Focus::Table),
            Action::ScrollBy(1),
            "and with the table focused it must still move the table"
        );
    }

    #[test]
    fn the_detail_panes_tabs_and_content_are_reachable_from_the_keyboard() {
        let r = full_registry();
        assert_eq!(
            action_for(&key(KeyCode::Tab), &r, Focus::Detail),
            Action::CycleDetailTab(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Right), &r, Focus::Detail),
            Action::CycleDetailTab(1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Left), &r, Focus::Detail),
            Action::CycleDetailTab(-1)
        );
        assert_eq!(
            action_for(&key(KeyCode::Down), &r, Focus::Detail),
            Action::ScrollDetail(1),
            "Down must scroll the open tab's content, not a table row"
        );
        assert_eq!(
            action_for(&key(KeyCode::Char('k')), &r, Focus::Detail),
            Action::ScrollDetail(-1)
        );
    }

    #[test]
    fn the_wheel_scrolls_whichever_pane_it_is_over() {
        let r = full_registry();
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollDown, 5, 3), &r, Focus::Table),
            Action::ScrollTree(SCROLL_STEP),
            "the wheel targets the region under the cursor, so over the \
             sidebar it must move the sidebar even though the table has focus"
        );
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollUp, 30, 3), &r, Focus::Table),
            Action::ScrollBy(-SCROLL_STEP)
        );
        assert_eq!(
            action_for(
                &scroll(MouseEventKind::ScrollDown, 30, 3),
                &r,
                Focus::Detail
            ),
            Action::ScrollDetail(SCROLL_STEP),
            "with the pane covering the table, the wheel over it must scroll \
             the pane's content"
        );
    }

    #[test]
    fn table_clicks_and_scrolls_are_suppressed_while_a_picker_is_open() {
        let r = registry();
        assert_eq!(
            action_for(&click(5, 3), &r, Focus::Picker),
            Action::None,
            "a table row click must not select while a picker has focus"
        );
        assert_eq!(
            action_for(&click(5, 1), &r, Focus::Picker),
            Action::None,
            "a header click must not sort while a picker has focus"
        );
        assert_eq!(
            action_for(&scroll(MouseEventKind::ScrollDown, 5, 3), &r, Focus::Picker),
            Action::None,
            "scroll must not move the table while a picker has focus"
        );
    }
}
