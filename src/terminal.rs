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

/// Whether a panic on a thread with this name should restore the terminal.
///
/// Only the main thread owns the terminal. A panic on a Tokio worker restores
/// nothing, because the render loop is still running and would keep drawing
/// into a torn-down screen.
pub fn should_restore_on_panic(thread_name: Option<&str>) -> bool {
    thread_name == Some("main")
}

/// Whether a panic on a thread with this name may print through the default
/// hook.
///
/// The default hook writes to stderr, which during a TUI session is the live
/// alternate screen in raw mode: with no carriage return on newline the
/// message staircases across the UI, and teardown then discards the buffer, so
/// the screen is corrupted *and* the panic location is lost. Only the main
/// thread — whose panic ends the session anyway, after the terminal has been
/// restored — may print. Background panics reach the user through the task
/// supervisor in `main`, which surfaces the payload as an `AppEvent::Error`.
pub fn should_print_panic(thread_name: Option<&str>) -> bool {
    thread_name == Some("main")
}

/// Installs a panic hook that restores the terminal before the default hook
/// prints. Without this, a panic leaves the user in a terminal with no echo.
///
/// The hook is process-global, so it also fires for panics on background
/// Tokio tasks. Only the main thread's panic restores the terminal and prints
/// — see `should_restore_on_panic` and `should_print_panic`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name();
        if should_restore_on_panic(name) {
            let _ = RealTerminal.restore();
        }
        if should_print_panic(name) {
            previous(info);
        }
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

    #[test]
    fn only_the_main_thread_restores_the_terminal_on_panic() {
        assert!(should_restore_on_panic(Some("main")));
        assert!(
            !should_restore_on_panic(Some("tokio-runtime-worker")),
            "a worker panic must not tear down a terminal the render loop is still using"
        );
        assert!(
            !should_restore_on_panic(None),
            "unnamed threads are not the main thread"
        );
    }

    #[test]
    fn only_the_main_thread_prints_its_panic() {
        assert!(should_print_panic(Some("main")));
        assert!(
            !should_print_panic(Some("tokio-runtime-worker")),
            "printing into a live alternate screen in raw mode staircases the \
             message across the UI and is then discarded by teardown"
        );
        assert!(!should_print_panic(None));
    }
}
