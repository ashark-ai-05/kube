use ratatui::layout::Rect;
use ratatui::Frame;
use crate::ui::hit::{HitRegistry, HitTarget};

pub const RIBBON_WIDTH: u16 = 1;

pub fn split_ribbon(area: Rect) -> (Rect, Rect) {
    todo!()
}

pub fn render_ribbon(f: &mut Frame, area: Rect, cluster: Option<&str>, hits: &mut HitRegistry) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use crate::ui::theme;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect { Rect { x, y, width: w, height: h } }

    #[test]
    fn split_reserves_exactly_one_column_for_the_ribbon() {
        let (ribbon, rest) = split_ribbon(rect(0, 0, 80, 24));
        assert_eq!(ribbon.width, RIBBON_WIDTH);
        assert_eq!(ribbon.height, 24);
        assert_eq!(rest.x, RIBBON_WIDTH);
        assert_eq!(rest.width, 79, "the rest of the screen must lose exactly the ribbon column");
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
        }).unwrap();

        let buf = term.backend().buffer();
        let expected = theme::cluster_hue("tst-wsdc");
        for y in 0..5 {
            assert_eq!(buf[(0, y)].style().fg, Some(expected), "ribbon row {y} was not the cluster hue");
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
            }).unwrap();
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
        }).unwrap();
        assert_eq!(term.backend().buffer()[(0, 0)].style().fg, Some(theme::DUSK));
    }

    #[test]
    fn the_ribbon_is_clickable_along_its_whole_height() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("prod"), &mut hits);
        }).unwrap();
        for y in 0..5 {
            assert_eq!(hits.hit(0, y), Some(&HitTarget::Ribbon), "ribbon not clickable at y={y}");
        }
    }
}
