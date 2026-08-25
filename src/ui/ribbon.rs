use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

pub const RIBBON_WIDTH: u16 = 1;

/// Reserve the leftmost column for the ribbon.
pub fn split_ribbon(area: Rect) -> (Rect, Rect) {
    let ribbon_w = RIBBON_WIDTH.min(area.width);
    let ribbon = Rect {
        x: area.x,
        y: area.y,
        width: ribbon_w,
        height: area.height,
    };
    let rest = Rect {
        x: area.x.saturating_add(ribbon_w),
        y: area.y,
        width: area.width.saturating_sub(ribbon_w),
        height: area.height,
    };
    (ribbon, rest)
}

/// Paint the cluster spine.
///
/// With twenty-odd clusters, "which cluster am I in?" is the question that
/// matters and the one people get wrong. A persistent colour answers it
/// peripherally, without reading anything.
pub fn render_ribbon(f: &mut Frame, area: Rect, cluster: Option<&str>, hits: &mut HitRegistry) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let color = cluster.map(theme::cluster_hue).unwrap_or(theme::DUSK);
    let block = Block::default().style(Style::default().fg(color).bg(color));
    f.render_widget(block, area);
    hits.push(area, 0, HitTarget::Ribbon);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn split_reserves_exactly_one_column_for_the_ribbon() {
        let (ribbon, rest) = split_ribbon(rect(0, 0, 80, 24));
        assert_eq!(ribbon.width, RIBBON_WIDTH);
        assert_eq!(ribbon.height, 24);
        assert_eq!(rest.x, RIBBON_WIDTH);
        assert_eq!(
            rest.width, 79,
            "the rest of the screen must lose exactly the ribbon column"
        );
    }

    #[test]
    fn split_on_a_zero_width_area_does_not_underflow() {
        let (ribbon, rest) = split_ribbon(rect(0, 0, 0, 24));
        assert_eq!(rest.width, 0);
        assert!(ribbon.width <= 1);
    }

    #[test]
    fn the_ribbon_is_painted_in_the_clusters_own_hue() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("tst-wsdc"), &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let expected = theme::cluster_hue("tst-wsdc");
        for y in 0..5 {
            assert_eq!(
                buf[(0, y)].style().fg,
                Some(expected),
                "ribbon row {y} was not the cluster hue"
            );
        }
    }

    #[test]
    fn two_clusters_paint_different_ribbons() {
        let paint = |name: &str| {
            let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
            let mut hits = HitRegistry::new();
            term.draw(|f| {
                let (ribbon, _) = split_ribbon(f.area());
                render_ribbon(f, ribbon, Some(name), &mut hits);
            })
            .unwrap();
            term.backend().buffer()[(0, 0)].style().fg
        };
        assert_ne!(paint("prod-eu"), paint("staging"));
    }

    #[test]
    fn with_no_cluster_the_ribbon_is_muted_not_absent() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, None, &mut hits);
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(
            buf[(0, 0)].style().fg,
            Some(theme::DUSK),
            "muted ribbon foreground must be DUSK"
        );
        assert_eq!(
            buf[(0, 0)].style().bg,
            Some(theme::DUSK),
            "muted ribbon background must be DUSK or it disappears"
        );
    }

    #[test]
    fn the_ribbon_is_a_solid_bar_not_a_coloured_glyph() {
        // Block fills its area with spaces, so fg alone paints nothing. If bg
        // is ever dropped the ribbon vanishes silently — every fg assertion
        // still passes, because fg is set correctly on an invisible cell.
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("prod-eu"), &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let expected = theme::cluster_hue("prod-eu");
        for y in 0..5 {
            let cell = &buf[(0, y)];
            assert_eq!(
                cell.style().bg,
                Some(expected),
                "row {y} has no background fill"
            );
            assert_eq!(
                cell.style().fg,
                Some(expected),
                "row {y} foreground drifted from the hue"
            );
        }
        // The neighbouring column must stay unpainted, or the "one cell wide"
        // guarantee is not actually being tested by anything.
        assert_ne!(
            buf[(1, 0)].style().bg,
            Some(expected),
            "the ribbon bled into column 1"
        );
    }

    #[test]
    fn the_ribbon_is_clickable_along_its_whole_height() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("prod"), &mut hits);
        })
        .unwrap();
        for y in 0..5 {
            assert_eq!(
                hits.hit(0, y),
                Some(&HitTarget::Ribbon),
                "ribbon not clickable at y={y}"
            );
        }
    }
}
