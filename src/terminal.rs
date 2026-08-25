use std::io::{self, Write};

/// Abstracts terminal restoration so guard behaviour is testable without a TTY.
pub trait TerminalControl {
    fn restore(&self) -> io::Result<()>;
}

/// Restores the real terminal: leaves alternate screen, disables mouse capture
/// and raw mode. Safe to call more than once.
pub struct RealTerminal;

impl TerminalControl for RealTerminal {
    fn restore(&self) -> io::Result<()> {
        use crossterm::event::DisableMouseCapture;
        use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
        let mut out = io::stdout();
        let _ = crossterm::execute!(out, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
        out.flush()
    }
}

/// Restores the terminal when dropped, including during panic unwind.
pub struct TerminalGuard<T: TerminalControl> {
    control: T,
    active: bool,
}

impl<T: TerminalControl> TerminalGuard<T> {
    pub fn new(control: T) -> Self {
        Self {
            control,
            active: true,
        }
    }

    /// Give up responsibility for restoration (the caller has already restored).
    pub fn disarm(&mut self) {
        self.active = false;
    }
}

impl<T: TerminalControl> Drop for TerminalGuard<T> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.control.restore();
        }
    }
}

/// Installs a panic hook that restores the terminal before the default hook
/// prints. Without this, a panic leaves the user in a terminal with no echo.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = RealTerminal.restore();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SpyTerminal(Arc<AtomicUsize>);

    impl TerminalControl for SpyTerminal {
        fn restore(&self) -> std::io::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn guard_restores_terminal_exactly_once_on_drop() {
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let _guard = TerminalGuard::new(SpyTerminal(calls.clone()));
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "must not restore while alive"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "must restore once on drop");
    }

    #[test]
    fn disarmed_guard_does_not_restore() {
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let mut guard = TerminalGuard::new(SpyTerminal(calls.clone()));
            guard.disarm();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "disarmed guard must not restore"
        );
    }
}
