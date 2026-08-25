use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, GroupVersionKind};
use kube_tui::app::event::{AppEvent, WatchStatus, coalesce};
use kube_tui::app::input::{Action, action_for, apply_selection};
use kube_tui::cluster;
use kube_tui::store::watch::{ResourceStore, SharedStore, spawn_watch};
use kube_tui::terminal::{RealTerminal, TerminalGuard, install_panic_hook};
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{TableView, render_table};
use ratatui::layout::{Constraint, Layout};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
    let current = contexts
        .iter()
        .find(|c| c.is_current)
        .map(|c| {
            (
                c.name.clone(),
                c.namespace.clone().unwrap_or_else(|| "default".into()),
            )
        })
        .unwrap_or_else(|| ("unknown".into(), "default".into()));

    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let pod_ar = ApiResource::erase::<Pod>(&());
    let pod_gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let watch_handle = spawn_watch(
        client.clone(),
        pod_ar,
        Some(current.1.clone()),
        store.clone(),
        tx.clone(),
    );

    // Tokio swallows a panicking task at the JoinHandle boundary. Without this,
    // a dead watch leaves the UI drawing indefinitely against a store nothing
    // is updating any more, showing stale data as if it were live.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = watch_handle.await {
                let reason = if e.is_panic() {
                    "panicked"
                } else {
                    "was cancelled"
                };
                let _ = tx.send(AppEvent::Error(format!("watch task {reason}; exiting")));
                let _ = tx.send(AppEvent::Quit);
            }
        });
    }

    // Feed terminal input into the same channel so there is one wake source.
    let input_handle = {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut events = crossterm::event::EventStream::new();
            while let Some(Ok(e)) = events.next().await {
                if tx.send(AppEvent::Input(e)).is_err() {
                    break;
                }
            }
        })
    };

    // Same reasoning as the watch task: a panicking input reader would
    // otherwise stop delivering keystrokes and mouse clicks with no visible
    // symptom beyond "the UI stopped responding".
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = input_handle.await {
                let reason = if e.is_panic() {
                    "panicked"
                } else {
                    "was cancelled"
                };
                let _ = tx.send(AppEvent::Error(format!("input task {reason}; exiting")));
                let _ = tx.send(AppEvent::Quit);
            }
        });
    }

    let mut term = ratatui::init();
    // The guard must exist before any fallible call that can `?`-return: once
    // `ratatui::init()` has entered the alternate screen and raw mode, nothing
    // else restores the terminal until this guard drops.
    let mut guard = TerminalGuard::new(RealTerminal);
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;

    let mut view = TableView::new();
    let mut hits = HitRegistry::new();
    let mut selected: usize = 0;
    let mut last_error: Option<String> = None;
    let mut status = WatchStatus::Initialising;

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
        if let Some(e) = batch.errors.last() {
            last_error = Some(e.clone());
        }
        if let Some((_, s)) = batch.status_changes.last() {
            status = *s;
        }

        // Read the store snapshot into a local Vec before drawing; the render
        // closure below must be synchronous and must not acquire any locks.
        //
        // Redraw on every batch regardless of `batch.store_dirty`: input alone
        // can change the selection. The "10,000 deltas cost one repaint"
        // property comes from draining the channel before drawing, not from
        // this flag.
        let objects = store.read().await.objects(&pod_gvk);

        let mut quit = false;
        for input in &batch.inputs {
            // `hits` reflects the PREVIOUS frame's layout: input always
            // arrives after the last draw, so on the very first iteration the
            // registry is empty and a click before the first paint is a
            // no-op. That is correct, not a bug to fix by drawing early.
            match action_for(input, &hits) {
                Action::Quit => quit = true,
                Action::SelectRow(i) => selected = i.min(objects.len().saturating_sub(1)),
                Action::ScrollBy(d) => selected = apply_selection(selected, d, objects.len()),
                Action::SortByColumn(_) | Action::None => {}
            }
        }
        if quit {
            break;
        }

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
                &current.0,
                &current.1,
                status,
                objects.len(),
                last_error.as_deref(),
                &mut hits,
            );
        })?;
    }

    guard.disarm();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    Ok(())
}
