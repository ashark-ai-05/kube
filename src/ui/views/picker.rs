// RED step: only the tests exist so far. Picker, PickerItem,
// filtered_indices, centered, and render_picker are not yet defined —
// cargo test --lib picker is expected to fail to compile.

#[cfg(test)]
mod tests {
    use crate::ui::hit::{HitRegistry, HitTarget};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use ratatui::widgets::Paragraph;

    use super::{Picker, PickerItem, centered, filtered_indices, render_picker};

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
                if let Some(HitTarget::PickerRow(i)) = hits.hit(x, y) {
                    if !found.contains(i) {
                        found.push(*i);
                    }
                }
            }
        }
        assert_eq!(
            found,
            vec![0, 1],
            "filter 'prod' shows two rows, indices into the FILTERED list"
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
