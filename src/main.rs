use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, GroupVersionKind};
use kube_tui::app::event::{AppEvent, WatchStatus, coalesce};
use kube_tui::app::input::{Action, action_for, apply_selection};
use kube_tui::cli::{CliOutcome, NamespaceScope, parse_args};
use kube_tui::cluster;
use kube_tui::store::watch::{ResourceStore, SharedStore, spawn_watch};
use kube_tui::terminal::{RealTerminal, TerminalGuard, install_panic_hook};
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{TableView, render_table};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

/// Describe why a supervised task stopped, including the panic message.
///
/// The panic hook deliberately does not print for background threads (it would
/// staircase into the live alternate screen), so this is the only path by which
/// a worker panic's payload reaches the user.
fn join_failure_detail(task: &str, e: tokio::task::JoinError) -> String {
    if e.is_panic() {
        let p = e.into_panic();
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with non-string payload".to_string());
        format!("{task} task panicked: {msg}")
    } else {
        format!("{task} task was cancelled")
    }
}

/// Watch a background task and turn its death into a visible error plus a quit.
///
/// Tokio swallows a panicking task at the `JoinHandle` boundary; without this
/// the UI keeps drawing as if everything were still live.
fn supervise(
    task: &'static str,
    handle: tokio::task::JoinHandle<()>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        if let Err(e) = handle.await {
            let _ = tx.send(AppEvent::Error(join_failure_detail(task, e)));
            let _ = tx.send(AppEvent::Quit);
        }
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments first, before any terminal setup.
    // This allows us to handle --help and errors cleanly to stdout/stderr.
    let cli_outcome = parse_args(std::env::args().skip(1));
    match cli_outcome {
        CliOutcome::Help => {
            println!("Usage: kube [OPTIONS]");
            println!();
            println!("OPTIONS:");
            println!("  -n, --namespace <namespace>   Watch a specific namespace");
            println!("  -A, --all-namespaces          Watch all namespaces");
            println!("  -h, --help                    Show this help message");
            std::process::exit(0);
        }
        CliOutcome::Error(msg) => {
            eprintln!("kube: {msg}");
            std::process::exit(2);
        }
        CliOutcome::Run(scope) => {
            // Continue with the parsed scope
            run_with_scope(scope).await?;
            return Ok(());
        }
    }
}

async fn run_with_scope(cli_scope: NamespaceScope) -> anyhow::Result<()> {
    // First: a panic anywhere below must still leave the terminal usable.
    install_panic_hook();

    // Corporate kubeconfigs are routinely split across several files joined by
    // KUBECONFIG; `cluster::connect()` only ever reads the default location, so
    // the multi-file path from Task 3b is required here.
    //
    // HOME is expected to be set on any interactive shell; falling back to "."
    // only affects the fallback kubeconfig path (~/.kube/config) when it is
    // missing, which is itself an unusual environment worth degrading
    // gracefully in rather than panicking over.
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let opts = cluster::ConnectOptions {
        kubeconfig_paths: cluster::kubeconfig_paths_from_env(
            std::env::var("KUBECONFIG").ok().as_deref(),
            std::path::Path::new(&home),
        ),
        ..Default::default()
    };

    // The terminal has not been touched yet, so on failure this prints
    // straight to stderr rather than corrupting an alternate screen.
    let client = match cluster::connect_with(&opts).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("kube: could not connect to a cluster: {e:#}");
            std::process::exit(1);
        }
    };

    let contexts = cluster::load_contexts().unwrap_or_default();
    let current_context = contexts
        .iter()
        .find(|c| c.is_current)
        .map(|c| {
            (
                c.name.clone(),
                c.namespace.clone().unwrap_or_else(|| "default".into()),
            )
        })
        .unwrap_or_else(|| ("unknown".into(), "default".into()));

    // Resolve the CLI scope to a namespace for the watch and a display string for the UI.
    let (watch_namespace, display_namespace) = match cli_scope {
        NamespaceScope::One(ns) => (Some(ns.clone()), ns),
        NamespaceScope::All => (None, "all namespaces".into()),
        NamespaceScope::FromContext => (Some(current_context.1.clone()), current_context.1.clone()),
    };

    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let pod_ar = ApiResource::erase::<Pod>(&());
    let pod_gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let watch_handle = spawn_watch(
        client.clone(),
        pod_ar,
        watch_namespace,
        store.clone(),
        tx.clone(),
    );

    // Tokio swallows a panicking task at the JoinHandle boundary. Without this,
    // a dead watch leaves the UI drawing indefinitely against a store nothing
    // is updating any more, showing stale data as if it were live.
    supervise("watch", watch_handle, tx.clone());

    // Feed terminal input into the same channel so there is one wake source.
    let input_handle = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut events = crossterm::event::EventStream::new();
            loop {
                match events.next().await {
                    Some(Ok(e)) => {
                        if tx.send(AppEvent::Input(e)).is_err() {
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        // Without this the UI would stay alive accepting nothing,
                        // and raw mode means there is no signal-based way out.
                        let _ = tx.send(AppEvent::Error(format!("input stream failed: {e}")));
                        let _ = tx.send(AppEvent::Quit);
                        break;
                    }
                    None => {
                        let _ = tx.send(AppEvent::Error("input stream ended".to_string()));
                        let _ = tx.send(AppEvent::Quit);
                        break;
                    }
                }
            }
        })
    };

    // Same reasoning as the watch task: a panicking input reader would
    // otherwise stop delivering keystrokes and mouse clicks with no visible
    // symptom beyond "the UI stopped responding".
    supervise("input", input_handle, tx.clone());

    // `kill <pid>` skips every `Drop`, so without this the process dies holding
    // raw mode, mouse capture and the alternate screen — the dead shell this
    // whole design exists to prevent. SIGINT arrives as Ctrl-C through
    // crossterm in raw mode and is already handled by `action_for`.
    #[cfg(unix)]
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut term_sig =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(s) => s,
                    // Without a signal handler the terminal survives via Drop on
                    // normal exit only; log and continue rather than failing startup.
                    Err(e) => {
                        let _ =
                            tx.send(AppEvent::Error(format!("SIGTERM handler unavailable: {e}")));
                        return;
                    }
                };
            term_sig.recv().await;
            let _ = tx.send(AppEvent::Quit);
        });
    }

    // Deliberately NOT `ratatui::init()`: it calls `std::panic::take_hook()`
    // and installs a hook that restores the terminal from *any* thread, which
    // both discards the hook installed above and tears the screen down under a
    // still-running render loop when a background task panics. Manual setup
    // installs no hook, and `TerminalGuard` already covers everything
    // `ratatui::init()` gave us.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    // The guard must exist before any further fallible call that can
    // `?`-return: raw mode and the alternate screen are now on, and nothing
    // else restores the terminal until this guard drops. It is installed
    // before mouse capture so no successful setup step is ever left un-undone.
    let _guard = TerminalGuard::new(RealTerminal);
    let mut term = Terminal::new(CrosstermBackend::new(stdout))?;
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;

    let mut view = TableView::new();
    let mut hits = HitRegistry::new();
    let mut selected: usize = 0;
    let mut last_error: Option<String> = None;
    let mut status = WatchStatus::Initialising;
    // Nothing has been painted yet, so the first batch must draw whatever it is.
    let mut needs_redraw = true;

    loop {
        // Block for at least one event, then drain everything queued behind it.
        let Some(first) = rx.recv().await else {
            break;
        };
        let mut batch = vec![first];
        while let Ok(e) = rx.try_recv() {
            batch.push(e);
        }
        let batch = coalesce(batch);

        if batch.quit {
            break;
        }
        // Anything that can change what the frame looks like arms a redraw.
        // With all-motion mouse reporting, a bare mouse move otherwise costs a
        // full repaint, and `columns_for` reformats every object each frame.
        needs_redraw |= batch.store_dirty;
        if let Some(e) = batch.errors.last() {
            last_error = Some(e.clone());
            needs_redraw = true;
        }
        if let Some((_, s)) = batch.status_changes.last() {
            status = *s;
            needs_redraw = true;
        }

        // Read the store snapshot into a local Vec before drawing; the render
        // closure below must be synchronous and must not acquire any locks.
        let objects = store.read().await.objects(&pod_gvk);

        let mut quit = false;
        for input in &batch.inputs {
            // A resize changes the layout but produces no action, so it has to
            // arm the redraw itself.
            if matches!(input, crossterm::event::Event::Resize(_, _)) {
                needs_redraw = true;
            }
            // `hits` reflects the PREVIOUS frame's layout: input always
            // arrives after the last draw, so on the very first iteration the
            // registry is empty and a click before the first paint is a
            // no-op. That is correct, not a bug to fix by drawing early.
            match action_for(input, &hits) {
                Action::Quit => quit = true,
                Action::SelectRow(i) => {
                    selected = i.min(objects.len().saturating_sub(1));
                    needs_redraw = true;
                }
                Action::ScrollBy(d) => {
                    selected = apply_selection(selected, d, objects.len());
                    needs_redraw = true;
                }
                Action::SortByColumn(_) => needs_redraw = true,
                Action::None => {}
            }
        }
        if quit {
            break;
        }
        if !needs_redraw {
            continue;
        }
        needs_redraw = false;

        view.state.select(if objects.is_empty() {
            None
        } else {
            Some(selected)
        });

        hits.clear();
        term.draw(|f| {
            let chunks =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(f.area());
            render_table(f, chunks[0], &objects, &pod_gvk, &mut view, &mut hits);
            render_status(
                f,
                chunks[1],
                &current_context.0,
                &display_namespace,
                status,
                objects.len(),
                last_error.as_deref(),
                &mut hits,
            );
        })?;
    }

    // No explicit teardown: dropping `term` shows the cursor again and then
    // `_guard` runs `RealTerminal::restore()`, which is exactly the disable
    // mouse capture / leave alternate screen / disable raw mode sequence the
    // old `ratatui::restore()` path performed. Normal exit and panic exit now
    // share one restoration implementation, so neither can drift from the other.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn join_error_from(f: impl FnOnce() + Send + 'static) -> tokio::task::JoinError {
        let handle = tokio::task::spawn_blocking(f);
        handle.await.expect_err("the task must have failed")
    }

    #[tokio::test]
    async fn a_str_panic_payload_reaches_the_user() {
        let e = join_error_from(|| panic!("watcher exploded")).await;
        assert_eq!(
            join_failure_detail("watch", e),
            "watch task panicked: watcher exploded",
            "the payload is the only record of a background panic: the hook does not print it"
        );
    }

    #[tokio::test]
    async fn a_string_panic_payload_reaches_the_user() {
        let e = join_error_from(|| panic!("{}", format!("code {}", 7))).await;
        assert_eq!(
            join_failure_detail("input", e),
            "input task panicked: code 7"
        );
    }

    #[tokio::test]
    async fn a_non_string_panic_payload_still_reports_something() {
        let e = join_error_from(|| std::panic::panic_any(42u8)).await;
        assert_eq!(
            join_failure_detail("watch", e),
            "watch task panicked: panic with non-string payload"
        );
    }

    #[tokio::test]
    async fn a_cancelled_task_is_distinguished_from_a_panic() {
        let handle = tokio::spawn(async {
            // Never completes; the abort below is what ends it.
            std::future::pending::<()>().await;
        });
        handle.abort();
        let e = handle.await.expect_err("aborting must yield a JoinError");
        assert_eq!(join_failure_detail("watch", e), "watch task was cancelled");
    }
}
