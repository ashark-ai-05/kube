mod terminal;

use terminal::{RealTerminal, TerminalGuard, install_panic_hook};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_hook();
    let mut term = ratatui::init();
    let mut guard = TerminalGuard::new(RealTerminal);

    term.draw(|f| {
        f.render_widget(
            ratatui::widgets::Paragraph::new("kube — press any key to exit"),
            f.area(),
        );
    })?;
    let _ = crossterm::event::read()?;

    guard.disarm();
    ratatui::restore();
    Ok(())
}
