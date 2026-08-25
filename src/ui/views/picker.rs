use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::scroll;
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// One selectable entry in the picker: a cluster or a namespace.
pub struct PickerItem {
    pub label: String,
    pub detail: String,
    pub accent: Option<Color>,
}

/// State for a single modal picker instance. Reused for both the cluster
/// picker and the namespace picker — the two differ only in what items and
/// title they're constructed with.
pub struct Picker {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub filter: String,
    /// Index into the FILTERED list, matching `HitTarget::PickerRow`.
    pub selected: usize,
    /// Scroll offset into the filtered list, owned here and advanced by
    /// `render_picker` each frame — the same arrangement `TableView` uses.
    /// Without it the picker drew only the first screenful while `selected`
    /// ranged over the whole filtered list, so on a kubeconfig with 20+
    /// contexts the ones past the fold were never drawn, registered no hit
    /// zone, and could be confirmed by Enter without ever appearing
    /// highlighted.
    pub scroll: usize,
}

/// Clamp `selected` to the filtered list, so it always names a row that
/// exists.
///
/// Items and filter both change under an open picker: `main.rs` rebuilds the
/// item list every event-loop pass (cluster states and observed namespaces
/// both move on their own), and a concurrent switch can empty it entirely.
/// Left unclamped, `selected` names a row that is gone and Enter resolves to
/// nothing — the picker closes having silently done nothing at all.
pub fn clamp_selection(picker: &mut Picker) {
    let n = filtered_indices(&picker.items, &picker.filter).len();
    picker.selected = picker.selected.min(n.saturating_sub(1));
}

/// Case-insensitive substring match over item labels.
pub fn filtered_indices(items: &[PickerItem], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..items.len()).collect();
    }
    let needle = filter.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.label.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

/// A centred rectangle occupying the given percentage of `area`.
pub fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = (area.width as u32 * pct_w as u32 / 100) as u16;
    let h = (area.height as u32 * pct_h as u32 / 100) as u16;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

/// Draw the filterable overlay and register a clickable zone for every
/// visible row.
///
/// `Clear` is rendered first: without it, the frame beneath (whatever was
/// drawn earlier in this same `Frame` — the table, the ribbon) shows through
/// wherever this overlay doesn't explicitly paint a glyph, because
/// `Block`'s background only sets cell *style*, not cell *content*.
///
/// Zones register at z-index 1 so `HitRegistry` resolves clicks here over
/// the table beneath (which registers at z=0), and the row index carried by
/// `HitTarget::PickerRow` is the index into the *filtered* list — that is
/// what the user actually clicked, and it is the caller's job (Task 9) to
/// map it back through `filtered_indices` to the real item. Scrolling does
/// not change that contract: the index registered is `scroll + row`, still
/// an index into the filtered list, never a screen position.
///
/// This view owns its scrolling, exactly as `render_table` does: it advances
/// `picker.scroll` by the least amount that keeps `picker.selected` on
/// screen, then draws and registers only that window.
pub fn render_picker(f: &mut Frame, area: Rect, picker: &mut Picker, hits: &mut HitRegistry) {
    // Items and filter change under an open picker, so a selection can be
    // left past the end. Clamp here so no caller has to remember to — the
    // same defensive clamp `render_table` performs.
    clamp_selection(picker);
    let matches = filtered_indices(&picker.items, &picker.filter);

    f.render_widget(Clear, area);

    let title = format!(" {} ", picker.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_style(true))
        .title(Span::styled(title, theme::header_style()))
        .style(Style::default().bg(theme::ABYSS));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Filter line, then the list beneath it.
    let filter_line = Line::from(vec![
        Span::styled("\u{2315} ", theme::label_style()),
        Span::styled(picker.filter.clone(), theme::text_style()),
    ]);
    f.render_widget(Paragraph::new(filter_line), Rect { height: 1, ..inner });

    let list_y = inner.y.saturating_add(1);
    // The filter line takes the first inner row; the rest is list.
    let rows = inner.height.saturating_sub(1) as usize;
    picker.scroll = scroll::scroll_offset(picker.selected, picker.scroll, rows);
    let visible = scroll::window(picker.scroll, rows, matches.len());

    for (row, &item_idx) in matches[visible.clone()].iter().enumerate() {
        // The index the user is actually pointing at: into the filtered list,
        // not the screen. `visible.start` is `picker.scroll`.
        let filtered_index = visible.start + row;
        let y = list_y + row as u16;
        let item = &picker.items[item_idx];
        let selected = filtered_index == picker.selected;

        let accent = item.accent.unwrap_or(theme::MIST);
        let mut style = Style::default().fg(theme::PAPER);
        if selected {
            style = style.bg(theme::DUSK).add_modifier(Modifier::BOLD);
        }

        let line = Line::from(vec![
            Span::styled("\u{258A} ", Style::default().fg(accent)),
            Span::styled(item.label.clone(), style),
            Span::styled(
                if item.detail.is_empty() {
                    String::new()
                } else {
                    format!("  {}", item.detail)
                },
                theme::muted_style(),
            ),
        ]);
        let row_area = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(line).style(style), row_area);

        // z=1 so the overlay wins over the table beneath. The index is into
        // the FILTERED list, because that is what the user actually clicked —
        // scrolled or not, so a click after scrolling picks the row drawn
        // there rather than the one that used to be there.
        hits.push(row_area, 1, HitTarget::PickerRow(filtered_index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn items() -> Vec<PickerItem> {
        ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]
            .iter()
            .map(|n| PickerItem {
                label: n.to_string(),
                detail: String::new(),
                accent: None,
            })
            .collect()
    }

    /// Render the picker into a fresh buffer and flatten it to text plus the
    /// hit zones it registered.
    fn render_to_string(picker: &mut Picker, w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_picker(f, area, picker, &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    /// Like `render_to_string`, but first paints the whole area with rows of
    /// `X` in the SAME frame, simulating whatever was drawn underneath the
    /// overlay (the table, the ribbon) before the picker renders on top. If
    /// the picker fails to `Clear` first, `Block`'s background only touches
    /// cell *style* — the leftover `X` glyphs stay put wherever the overlay
    /// doesn't explicitly draw its own text, and this is how that surfaces.
    fn render_over_noise(picker: &mut Picker, w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            let noise: Vec<Line> = (0..h).map(|_| Line::from("X".repeat(w as usize))).collect();
            f.render_widget(Paragraph::new(noise), area);
            render_picker(f, area, picker, &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    /// Render the picker and return the `Style` painted at the label text of
    /// two given row positions (0-indexed within the visible/filtered list).
    /// Geometry mirrors `render_picker`'s own layout — one row for the top
    /// border, one for the filter line, so row 0's label starts at (x=3,
    /// y=2) — confirmed against the actual rendered buffer (dumped via
    /// `render_to_string`) rather than assumed:
    /// ```text
    ///  0: "╭ Clusters ────...
    ///  1: "│⌕             ...
    ///  2: "│▊ prod-eu     ...   <- row 0, label starts at x=3
    ///  3: "│▊ prod-us     ...   <- row 1
    /// ```
    fn render_row_styles(picker: &mut Picker, row_a: usize, row_b: usize) -> (Style, Style) {
        let mut term = Terminal::new(TestBackend::new(60, 16)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_picker(f, area, picker, &mut hits);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let style_at = |row: usize| buf[(3, 2 + row as u16)].style();
        (style_at(row_a), style_at(row_b))
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        assert_eq!(filtered_indices(&items(), "").len(), 5);
    }

    #[test]
    fn filtering_is_a_case_insensitive_substring_match() {
        assert_eq!(filtered_indices(&items(), "PROD"), vec![0, 1]);
        assert_eq!(filtered_indices(&items(), "wsdc"), vec![4]);
    }

    #[test]
    fn a_filter_matching_nothing_yields_an_empty_list_not_everything() {
        assert!(filtered_indices(&items(), "zzzz").is_empty());
    }

    #[test]
    fn centered_leaves_a_margin_on_every_side() {
        let a = centered(
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 40,
            },
            60,
            60,
        );
        assert!(a.x > 0 && a.y > 0);
        assert!(a.x + a.width < 100);
        assert!(a.y + a.height < 40);
    }

    #[test]
    fn centered_on_a_tiny_area_does_not_underflow() {
        let a = centered(
            Rect {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            },
            60,
            60,
        );
        assert!(a.width <= 3 && a.height <= 2);
    }

    #[test]
    fn the_picker_draws_its_title_and_items() {
        let mut p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
            scroll: 0,
        };
        let (text, _) = render_to_string(&mut p, 60, 16);
        assert!(text.contains("Clusters"), "title missing:\n{text}");
        assert!(text.contains("prod-eu"), "items missing:\n{text}");
    }

    #[test]
    fn each_visible_picker_row_is_clickable_and_maps_to_the_filtered_index() {
        let mut p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: "prod".into(),
            selected: 0,
            scroll: 0,
        };
        let (_, hits) = render_to_string(&mut p, 60, 16);
        let mut found = Vec::new();
        for y in 0..16u16 {
            for x in 0..60u16 {
                if let Some(HitTarget::PickerRow(i)) = hits.hit(x, y)
                    && !found.contains(i)
                {
                    found.push(*i);
                }
            }
        }
        assert_eq!(
            found,
            vec![0, 1],
            "filter 'prod' shows two rows, indices into the FILTERED list"
        );

        // "prod" happens to match items already at the front of the
        // unfiltered list, so the assertion above passes even if
        // render_picker registered the unfiltered index — filtered and
        // unfiltered indices coincide for [0, 1]. "wsdc" matches only
        // "tst-wsdc", the unfiltered index 4, which the filtered list
        // renders at row 0: this is what actually distinguishes "index
        // into the filtered list" from "index into the full list", and
        // getting it wrong is how a filtered click selects the wrong
        // cluster.
        let mut p_wsdc = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: "wsdc".into(),
            selected: 0,
            scroll: 0,
        };
        let (_, hits) = render_to_string(&mut p_wsdc, 60, 16);
        let mut found = Vec::new();
        for y in 0..16u16 {
            for x in 0..60u16 {
                if let Some(HitTarget::PickerRow(i)) = hits.hit(x, y)
                    && !found.contains(i)
                {
                    found.push(*i);
                }
            }
        }
        assert_eq!(
            found,
            vec![0],
            "filter 'wsdc' shows one row at filtered position 0, \
             not the unfiltered index 4"
        );
    }

    #[test]
    fn the_overlay_covers_what_is_beneath_it() {
        // Without Clear, the previous frame's content shows through the modal.
        let mut p = Picker {
            title: "T".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
            scroll: 0,
        };
        let (text, _) = render_over_noise(&mut p, 60, 16);
        assert!(
            !text.contains("XXXXXXXX"),
            "background bled through the overlay:\n{text}"
        );
    }

    #[test]
    fn picker_rows_win_over_the_table_beneath_them() {
        // The overlay draws on top; its hit zones must win by Z-INDEX, not
        // merely by being registered later. This matters because
        // HitRegistry's own tie-break rule ("later registration wins at
        // equal z", see hit.rs) means a table-then-picker registration
        // order would make the picker win *anyway*, even at z=0 — that
        // ordering can never distinguish "z=1 wins" from "registered last
        // wins" and was tried first here and found not to fail under the
        // z=0 mutation (see task-6-report.md for the empirical check). So
        // the competing table zone is registered AFTER the picker instead,
        // at equal z=0: with that adversarial ordering, only render_picker
        // actually using z=1 can make the picker win.
        let mut hits = HitRegistry::new();
        let mut p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
            scroll: 0,
        };
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered(f.area(), 60, 60);
            render_picker(f, area, &mut p, &mut hits);
        })
        .unwrap();

        // Find a coordinate the picker actually registered.
        let mut coord = None;
        'outer: for y in 0..24u16 {
            for x in 0..80u16 {
                if matches!(hits.hit(x, y), Some(HitTarget::PickerRow(_))) {
                    coord = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (x, y) = coord.expect("picker registered no rows");

        // Register a competing table zone over that same coordinate,
        // AFTER the picker, at z=0 — the adversarial ordering.
        hits.push(
            Rect {
                x,
                y,
                width: 1,
                height: 1,
            },
            0,
            HitTarget::TableRow(99),
        );

        assert!(
            matches!(hits.hit(x, y), Some(HitTarget::PickerRow(_))),
            "a table zone registered AFTER the picker, at equal z, must still \
             lose to it — the picker only wins here because it registers at \
             z=1, not because of registration order"
        );
    }

    #[test]
    fn the_selected_row_is_visually_distinct_from_the_others() {
        // Task 5's ribbon shipped six tests that all asserted fg and none that
        // asserted bg, so dropping the background fill left it invisible with
        // a green suite. Assert whatever actually distinguishes the row.
        let mut p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: String::new(),
            selected: 1,
            scroll: 0,
        };
        let (styles_selected, styles_other) = render_row_styles(&mut p, 1, 0);
        assert_ne!(
            styles_selected, styles_other,
            "the selected picker row must render differently from an unselected one"
        );
    }

    // --- Scrolling: a kubeconfig with more contexts than fit on screen ---

    /// Twenty clusters, named so no label is a substring of another
    /// ("cluster-1" would match inside "cluster-19"; two digits do not).
    fn many_clusters() -> Vec<PickerItem> {
        (0..20)
            .map(|i| PickerItem {
                label: format!("cluster-{i:02}"),
                detail: String::new(),
                accent: None,
            })
            .collect()
    }

    /// A viewport with room for exactly 11 list rows: 14 lines, less two
    /// borders and the filter line. Confirmed against a real buffer dump —
    /// rows draw at y=2..=12, labels start at x=3, y=13 is the bottom
    /// border. Twenty items into eleven rows is the whole point: nine of
    /// them can only be reached by scrolling.
    fn render_scrolled(picker: &mut Picker) -> (Vec<String>, HitRegistry, Vec<Style>) {
        let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| render_picker(f, f.area(), picker, &mut hits))
            .unwrap();
        let buf = term.backend().buffer();
        let lines: Vec<String> = (0..14u16)
            .map(|y| (0..60u16).map(|x| buf[(x, y)].symbol()).collect())
            .collect();
        let styles: Vec<Style> = (0..14u16).map(|y| buf[(3, y)].style()).collect();
        (lines, hits, styles)
    }

    #[test]
    fn a_selection_past_the_first_screenful_is_actually_drawn() {
        // The shipped bug: `matches.iter().take(rows)` drew only the first
        // eleven of twenty clusters while `main.rs` let `selected` walk the
        // whole filtered list. Nineteen presses of Down left `selected = 19`
        // with cluster-19 never rendered — and Enter then connected to a
        // cluster the user had never seen selected.
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: String::new(),
            selected: 19,
            scroll: 0,
        };
        let (lines, _, _) = render_scrolled(&mut p);
        let text = lines.join("\n");
        assert!(
            text.contains("cluster-19"),
            "the selected cluster must be on screen; got:\n{text}"
        );
        assert!(
            !text.contains("cluster-00"),
            "the window must have scrolled off the top, not grown; got:\n{text}"
        );
        assert_eq!(
            p.scroll, 9,
            "the offset must move by the minimum that brings row 19 into an \
             11-row window, not jump to the selection"
        );
    }

    #[test]
    fn the_selected_row_is_highlighted_after_scrolling_to_it() {
        // Drawing it is not enough: the highlight is compared against the
        // FILTERED index, so a comparison left against the screen row would
        // put the highlight on cluster-09 (screen row 0) — or, once the
        // selection exceeds the row count, on nothing at all.
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: String::new(),
            selected: 19,
            scroll: 0,
        };
        let (lines, _, styles) = render_scrolled(&mut p);
        assert!(
            lines[12].contains("cluster-19"),
            "expected cluster-19 on the last list row; got: {}",
            lines[12]
        );
        assert_eq!(
            styles[12].bg,
            Some(theme::DUSK),
            "the selected row must carry the selection background"
        );
        assert_ne!(
            styles[2].bg,
            Some(theme::DUSK),
            "an unselected row (cluster-09, the first drawn) must not be \
             highlighted — a highlight keyed off the screen row would land here"
        );
    }

    #[test]
    fn every_drawn_row_registers_a_hit_zone_carrying_its_filtered_index() {
        // "Every action reachable by mouse alone" — the clusters past the
        // first screenful registered no hit zone at all, so they could not
        // be clicked however far the list was scrolled.
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: String::new(),
            selected: 19,
            scroll: 0,
        };
        let (lines, hits, _) = render_scrolled(&mut p);

        let mut found = Vec::new();
        for y in 0..14u16 {
            if let Some(HitTarget::PickerRow(i)) = hits.hit(5, y) {
                found.push(*i);
            }
        }
        assert_eq!(
            found,
            (9..20).collect::<Vec<usize>>(),
            "the eleven rows on screen are filtered indices 9..=19, not 0..=10"
        );

        // And the zone at each y names the row actually DRAWN there — the
        // Plan 1 invariant, re-checked under a scrolled window.
        for (k, y) in (2..13u16).enumerate() {
            let expected_label = format!("cluster-{:02}", 9 + k);
            assert!(
                lines[y as usize].contains(&expected_label),
                "expected {expected_label} at y={y}; got: {}",
                lines[y as usize]
            );
            assert_eq!(
                hits.hit(5, y),
                Some(&HitTarget::PickerRow(9 + k)),
                "clicking the row drawn at y={y} must resolve to the cluster shown there"
            );
        }
    }

    #[test]
    fn scrolling_back_up_follows_the_selection() {
        // The offset must move both ways: having scrolled down to 19, moving
        // the selection back to 0 must bring the top of the list back rather
        // than leaving the selection above the window.
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: String::new(),
            selected: 19,
            scroll: 0,
        };
        let _ = render_scrolled(&mut p);
        assert_eq!(p.scroll, 9);

        p.selected = 0;
        let (lines, _, _) = render_scrolled(&mut p);
        assert_eq!(p.scroll, 0, "the window must follow the selection upward");
        assert!(
            lines.join("\n").contains("cluster-00"),
            "cluster-00 must be back on screen"
        );
    }

    #[test]
    fn a_filter_narrower_than_the_viewport_does_not_scroll() {
        // Nothing about the fix may disturb the common case: a list that
        // fits must draw from the top with no offset at all.
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: "cluster-1".into(),
            selected: 0,
            scroll: 0,
        };
        let (lines, hits, _) = render_scrolled(&mut p);
        assert_eq!(p.scroll, 0);
        assert!(
            lines[2].contains("cluster-10"),
            "the first match must draw on the first list row; got: {}",
            lines[2]
        );
        assert_eq!(
            hits.hit(5, 2),
            Some(&HitTarget::PickerRow(0)),
            "with ten matches in an eleven-row window the filtered index is \
             still the position within the MATCHES, which for cluster-10 is 0"
        );
    }

    #[test]
    fn a_selection_left_past_the_end_of_a_shrunken_list_is_clamped() {
        // A cluster switch can empty the object list under an open namespace
        // picker, dropping it from many items to one. Left unclamped,
        // `selected` names a row that no longer exists and a confirm on it
        // resolves to nothing: the picker vanishes having done nothing.
        let mut p = Picker {
            title: "Namespaces".into(),
            items: vec![PickerItem {
                label: "all namespaces".into(),
                detail: String::new(),
                accent: None,
            }],
            filter: String::new(),
            selected: 8,
            scroll: 0,
        };
        clamp_selection(&mut p);
        assert_eq!(
            p.selected, 0,
            "a selection past the end must name the last surviving row"
        );
    }

    #[test]
    fn clamping_an_empty_list_yields_zero_rather_than_underflowing() {
        let mut p = Picker {
            title: "Clusters".into(),
            items: many_clusters(),
            filter: "matches-nothing".into(),
            selected: 7,
            scroll: 0,
        };
        clamp_selection(&mut p);
        assert_eq!(p.selected, 0);
    }
}
