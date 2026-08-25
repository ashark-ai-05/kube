use crate::ui::hit::{HitRegistry, HitTarget};
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
    pub selected: usize,
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
/// map it back through `filtered_indices` to the real item.
pub fn render_picker(f: &mut Frame, area: Rect, picker: &Picker, hits: &mut HitRegistry) {
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
    let rows = inner.height.saturating_sub(1);
    for (row, &item_idx) in matches.iter().take(rows as usize).enumerate() {
        let y = list_y + row as u16;
        let item = &picker.items[item_idx];
        let selected = row == picker.selected;

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
        // the FILTERED list, because that is what the user actually clicked.
        hits.push(row_area, 1, HitTarget::PickerRow(row));
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
    fn render_to_string(picker: &Picker, w: u16, h: u16) -> (String, HitRegistry) {
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
    fn render_over_noise(picker: &Picker, w: u16, h: u16) -> (String, HitRegistry) {
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
        let p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
        };
        let (text, _) = render_to_string(&p, 60, 16);
        assert!(text.contains("Clusters"), "title missing:\n{text}");
        assert!(text.contains("prod-eu"), "items missing:\n{text}");
    }

    #[test]
    fn each_visible_picker_row_is_clickable_and_maps_to_the_filtered_index() {
        let p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: "prod".into(),
            selected: 0,
        };
        let (_, hits) = render_to_string(&p, 60, 16);
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
        let p_wsdc = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: "wsdc".into(),
            selected: 0,
        };
        let (_, hits) = render_to_string(&p_wsdc, 60, 16);
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
        let p = Picker {
            title: "T".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
        };
        let (text, _) = render_over_noise(&p, 60, 16);
        assert!(
            !text.contains("XXXXXXXX"),
            "background bled through the overlay:\n{text}"
        );
    }
}
