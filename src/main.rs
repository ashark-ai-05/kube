use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::Client;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube_tui::app::Overlay;
use kube_tui::app::event::{AppEvent, WatchStatus, coalesce};
use kube_tui::app::input::{Action, action_for, apply_selection};
use kube_tui::app::session::{
    Session, SessionEvent, SharedSession, is_deliberate_abort, switch_cluster,
};
use kube_tui::cli::{CliOutcome, NamespaceScope, parse_args, should_hint_all_namespaces};
use kube_tui::cluster;
use kube_tui::cluster::{ClusterEntry, ClusterId, ClusterRegistry, ConnectionState};
use kube_tui::store::watch::{ResourceStore, spawn_watch};
use kube_tui::terminal::{RealTerminal, TerminalGuard, install_panic_hook};
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::ribbon::{render_ribbon, split_ribbon};
use kube_tui::ui::theme;
use kube_tui::ui::views::picker::{Picker, PickerItem, centered, filtered_indices, render_picker};
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{TableView, render_table};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

/// A namespace choice meaning "watch everything", not a literal namespace.
/// Not a valid Kubernetes namespace name (it contains a space), so it can
/// never collide with a real one.
const ALL_NAMESPACES_LABEL: &str = "all namespaces";

/// Build the cluster picker's item list fresh from the registry.
///
/// The registry is the only source of truth for connection state —
/// `SessionEvent`s are emitted after the session lock is released, so
/// concurrent switches can emit them in an order that disagrees with the
/// registry. Reading state through events here instead would risk showing a
/// cluster as connected when a later, still-in-flight attempt has already
/// superseded it.
fn cluster_picker_items(entries: &[ClusterEntry]) -> Vec<PickerItem> {
    entries
        .iter()
        .map(|e| {
            let detail = match &e.state {
                ConnectionState::Disconnected => String::new(),
                ConnectionState::Connecting => "connecting…".to_string(),
                ConnectionState::Connected => "connected".to_string(),
                ConnectionState::Failed { reason } => format!("failed: {reason}"),
            };
            PickerItem {
                label: e.id.0.clone(),
                detail,
                accent: Some(theme::cluster_hue(&e.id.0)),
            }
        })
        .collect()
}

/// Build the namespace picker's item list from the namespaces actually
/// present in the objects currently loaded, plus an "all namespaces" entry.
///
/// There is no cluster-wide namespace listing wired up (that would be its
/// own `Namespace` watch) — this reflects what the current watch has
/// actually seen, which is complete whenever the default all-namespaces
/// scope is active and partial otherwise.
fn namespace_picker_items(objects: &[Arc<DynamicObject>]) -> Vec<PickerItem> {
    let names: BTreeSet<String> = objects
        .iter()
        .filter_map(|o| o.metadata.namespace.clone())
        .collect();
    let mut items = Vec::with_capacity(names.len() + 1);
    items.push(PickerItem {
        label: ALL_NAMESPACES_LABEL.to_string(),
        detail: "watch every namespace".to_string(),
        accent: None,
    });
    items.extend(names.into_iter().map(|n| PickerItem {
        label: n,
        detail: String::new(),
        accent: None,
    }));
    items
}

/// `None` means "all namespaces" — the sentinel label, not a real namespace.
fn namespace_choice_from_label(label: &str) -> Option<String> {
    if label == ALL_NAMESPACES_LABEL {
        None
    } else {
        Some(label.to_string())
    }
}

/// Name of the cluster a switch is connecting to, if any.
///
/// While a connect is in flight the active cluster is still the OLD one —
/// `switch_cluster` only tears down and activates the new one on success —
/// so a status bar that reads only `registry.active()` shows no sign of an
/// attempt in progress. This must scan for a `ConnectionState::Connecting`
/// entry instead; the registry is the only source of truth for connection
/// state, since `SessionEvent`s are emitted after the session lock is
/// released and can arrive out of order across concurrent switches.
fn connecting_cluster_name(entries: &[ClusterEntry]) -> Option<String> {
    entries
        .iter()
        .find(|e| matches!(e.state, ConnectionState::Connecting))
        .map(|e| e.id.0.clone())
}

/// Resolve a filtered-list index back to the item it actually refers to.
///
/// `HitTarget::PickerRow` and `Picker::selected` both carry an index into
/// the FILTERED list, not the full item list — mapping through
/// `filtered_indices` is what makes a filtered click (or Enter) act on the
/// row actually shown rather than on whatever unfiltered index happens to
/// share that number. Getting this wrong is how a filtered click selects
/// the wrong cluster.
fn resolve_picker_choice(picker: &Picker, filtered_index: usize) -> Option<String> {
    let matches = filtered_indices(&picker.items, &picker.filter);
    matches
        .get(filtered_index)
        .and_then(|&real| picker.items.get(real))
        .map(|item| item.label.clone())
}

/// Record the client currently in use for the active cluster.
///
/// `Session` holds the store, the watch handles and the registry, but not
/// the `Client` itself — nothing needed it before namespace switching, which
/// restarts the watch against the SAME cluster without reconnecting. A
/// cluster switch mints a fresh `Client`, reachable only inside the closure
/// `switch_cluster` hands it to; this cell is how that value survives past
/// the switch for a later namespace change to reuse. `std::sync::Mutex`
/// rather than the session's `tokio::sync::Mutex`: writes happen inside
/// `switch_cluster`'s `spawn_watches` closure, which the session lock is
/// held across — a second lock acquired and released synchronously, with no
/// `.await` in between, cannot deadlock that.
fn set_current_client(cell: &StdMutex<Client>, client: Client) {
    match cell.lock() {
        Ok(mut guard) => *guard = client,
        Err(poisoned) => *poisoned.into_inner() = client,
    }
}

fn get_current_client(cell: &StdMutex<Client>) -> Client {
    match cell.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Draw one frame: the ribbon, then the table and status bar, then the
/// overlay LAST — so its z=1 hit zones (registered by `render_picker`)
/// resolve above the table's z=0 zones wherever the two overlap, and its
/// own content (behind a `Clear`) paints over whatever the table left in
/// that region.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    f: &mut Frame,
    objects: &[Arc<DynamicObject>],
    gvk: &GroupVersionKind,
    view: &mut TableView,
    context_name: &str,
    display_namespace: &str,
    status: WatchStatus,
    last_error: Option<&str>,
    show_hint: bool,
    connecting: Option<&str>,
    overlay: &Overlay,
    hits: &mut HitRegistry,
) {
    let full = f.area();
    let (ribbon_area, rest) = split_ribbon(full);
    render_ribbon(f, ribbon_area, Some(context_name), hits);

    let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(rest);
    render_table(f, chunks[0], objects, gvk, view, hits);
    render_status(
        f,
        chunks[1],
        context_name,
        display_namespace,
        status,
        objects.len(),
        last_error,
        show_hint,
        connecting,
        hits,
    );

    if let Some(picker) = overlay.picker() {
        let area = centered(f.area(), 60, 60);
        render_picker(f, area, picker, hits);
    }
}

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

/// Cancels the task it holds when dropped.
///
/// Dropping a `JoinHandle` *detaches* its task rather than cancelling it, so a
/// supervisor that is aborted while awaiting one would leave the watch beneath
/// it running — the exact leak `WatchHandles` exists to prevent.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Watch a background task and turn its death into a visible error plus a quit.
///
/// Tokio swallows a panicking task at the `JoinHandle` boundary; without this
/// the UI keeps drawing as if everything were still live.
///
/// Returns the supervisor's own handle. Only one owner can await a
/// `JoinHandle`, and observing a panic requires owning it, so the supervisor
/// takes the watch and this handle is what goes into `WatchHandles`; aborting
/// it cancels the watch underneath through `AbortOnDrop`.
fn supervise(
    task: &'static str,
    handle: tokio::task::JoinHandle<()>,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> tokio::task::JoinHandle<()> {
    let abort_watch = handle.abort_handle();
    tokio::spawn(async move {
        let _cancel_on_abort = AbortOnDrop(abort_watch);
        match handle.await {
            Ok(()) => {}
            // Switching clusters aborts the outgoing cluster's watches. That
            // is a normal teardown, not a crash: quitting here would exit the
            // application on the user's first cluster switch.
            Err(e) if is_deliberate_abort(&e) => {}
            Err(e) => {
                let _ = tx.send(AppEvent::Error(join_failure_detail(task, e)));
                let _ = tx.send(AppEvent::Quit);
            }
        }
    })
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
    let (startup_context_name, context_namespace, namespace_from_context) = contexts
        .iter()
        .find(|c| c.is_current)
        .map(|c| {
            let (ns, was_explicit) = c
                .namespace
                .clone()
                .map(|ns| (ns, true))
                .unwrap_or_else(|| ("default".into(), false));
            (c.name.clone(), ns, was_explicit)
        })
        .unwrap_or_else(|| ("unknown".into(), "default".into(), false));

    // Resolve the CLI scope to a namespace for the watch, display string for UI, and fallback flag.
    // The fallback flag is true when we're watching the "default" namespace because the context
    // didn't specify a namespace (not because the user chose it explicitly or via -n).
    let (watch_namespace, mut display_namespace, mut is_fallback_namespace) = match cli_scope {
        NamespaceScope::One(ns) => (Some(ns.clone()), ns, false),
        NamespaceScope::All => (None, "all namespaces".into(), false),
        NamespaceScope::FromContext => {
            let is_fallback = !namespace_from_context && context_namespace == "default";
            (
                Some(context_namespace.clone()),
                context_namespace,
                is_fallback,
            )
        }
    };

    // Everything belonging to "the cluster on screen" lives behind one handle
    // so that a switch can replace it wholesale — the store in particular is
    // replaced rather than cleared. See `switch_cluster`.
    let session: SharedSession = Arc::new(Mutex::new(Session::new(
        ClusterRegistry::from_contexts(contexts),
    )));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    // Tracks the Client for whichever cluster is currently active. `Session`
    // holds the store, the handles and the registry, but not the Client
    // itself — nothing needed it before namespace switching, which restarts
    // the watch against the SAME cluster without reconnecting. A cluster
    // switch (via `switch_cluster`) mints a fresh Client reachable only
    // inside the closure it is handed to; this cell is how that value
    // survives past the switch for a later namespace change to reuse.
    let current_client: Arc<StdMutex<Client>> = Arc::new(StdMutex::new(client.clone()));

    let pod_ar = ApiResource::erase::<Pod>(&());
    let pod_gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let watch_handle = spawn_watch(
        client.clone(),
        pod_ar.clone(),
        watch_namespace,
        session.lock().await.store.clone(),
        tx.clone(),
    );

    // Tokio swallows a panicking task at the JoinHandle boundary. Without this,
    // a dead watch leaves the UI drawing indefinitely against a store nothing
    // is updating any more, showing stale data as if it were live.
    //
    // The session tracks the SUPERVISOR's handle, not the watch's, so that the
    // first cluster switch tears this watch down with all the others; aborting
    // a supervisor cancels its watch. See `supervise`.
    session
        .lock()
        .await
        .handles
        .push(supervise("watch", watch_handle, tx.clone()));

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
    //
    // Deliberately not tracked in the session: the input reader outlives every
    // cluster, so a switch must not abort it.
    let _input_supervisor = supervise("input", input_handle, tx.clone());

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
    let mut last_error: Option<String> = None;
    let mut status = WatchStatus::Initialising;
    // At most one picker is ever open; opening one replaces whatever was
    // open before. Neither cluster nor namespace picking touched a network
    // before this task — this is what makes them reachable.
    let mut overlay = Overlay::None;
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
        // A switch changes the ribbon, the status bar and the whole table, and
        // "connecting" must appear before the attempt that produced it
        // finishes — which is the entire reason it is announced as an event.
        if !batch.session_events.is_empty() {
            needs_redraw = true;
        }
        for e in &batch.session_events {
            if let SessionEvent::ConnectFailed { id, reason } = e {
                last_error = Some(format!("connecting to {}: {reason}", id.0));
            }
        }

        // A cluster switch REPLACES the store and changes which cluster is
        // active, so both are re-read every pass rather than captured once: a
        // clone taken before a switch would keep showing the previous
        // cluster's objects under the previous cluster's name for ever.
        //
        // The session guard is released before the store is locked — holding
        // it across an await would block `switch_cluster`, which needs it to
        // announce "connecting" while this loop is still running.
        let (store, active_cluster, entries) = {
            let s = session.lock().await;
            (
                s.store.clone(),
                s.registry.active().map(|e| e.id.0.clone()),
                s.registry.entries().to_vec(),
            )
        };
        let context_name = active_cluster.unwrap_or_else(|| startup_context_name.clone());
        let connecting_name = connecting_cluster_name(&entries);
        // Read the store snapshot into a local Vec before drawing; the render
        // closure below must be synchronous and must not acquire any locks.
        let objects = store.read().await.objects(&pod_gvk);

        // Keep an open picker's items current: the registry (cluster
        // states) and the object list (namespaces seen) can both change
        // while it's on screen, and it must reflect that rather than a
        // snapshot taken at open time. Filter and selection are untouched.
        match &mut overlay {
            Overlay::ClusterPicker(p) => p.items = cluster_picker_items(&entries),
            Overlay::NamespacePicker(p) => p.items = namespace_picker_items(&objects),
            Overlay::None => {}
        }

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
            let action = action_for(input, &hits, overlay.is_open());
            // Enter and a picker-row click both confirm a choice; they carry
            // the chosen row differently (Enter uses the picker's own
            // highlighted position, a click carries the filtered index it
            // actually landed on) but resolve identically from there.
            let confirm_index = match action {
                Action::PickerSelect(i) => Some(i),
                Action::PickerConfirm => overlay.picker().map(|p| p.selected),
                _ => None,
            };
            match action {
                Action::Quit => quit = true,
                Action::SelectRow(i) => {
                    view.selected = i.min(objects.len().saturating_sub(1));
                    needs_redraw = true;
                }
                Action::ScrollBy(d) => {
                    match &mut overlay {
                        Overlay::None => {
                            view.selected = apply_selection(view.selected, d, objects.len());
                        }
                        Overlay::ClusterPicker(p) | Overlay::NamespacePicker(p) => {
                            let n = filtered_indices(&p.items, &p.filter).len();
                            p.selected = apply_selection(p.selected, d, n);
                        }
                    }
                    needs_redraw = true;
                }
                Action::SortByColumn(_) => needs_redraw = true,
                Action::OpenClusterPicker => {
                    overlay = Overlay::ClusterPicker(Picker {
                        title: "Clusters".into(),
                        items: cluster_picker_items(&entries),
                        filter: String::new(),
                        selected: 0,
                    });
                    needs_redraw = true;
                }
                Action::OpenNamespacePicker => {
                    overlay = Overlay::NamespacePicker(Picker {
                        title: "Namespaces".into(),
                        items: namespace_picker_items(&objects),
                        filter: String::new(),
                        selected: 0,
                    });
                    needs_redraw = true;
                }
                Action::ClosePicker => {
                    overlay = Overlay::None;
                    needs_redraw = true;
                }
                Action::PickerFilterChar(c) => {
                    if let Some(p) = overlay.picker_mut() {
                        p.filter.push(c);
                        p.selected = 0;
                        needs_redraw = true;
                    }
                }
                Action::PickerBackspace => {
                    if let Some(p) = overlay.picker_mut() {
                        p.filter.pop();
                        p.selected = 0;
                        needs_redraw = true;
                    }
                }
                Action::PickerSelect(_) | Action::PickerConfirm => {
                    if let Some(i) = confirm_index {
                        match std::mem::take(&mut overlay) {
                            Overlay::ClusterPicker(p) => {
                                if let Some(label) = resolve_picker_choice(&p, i) {
                                    let target = ClusterId(label);
                                    let mut switch_opts = opts.clone();
                                    switch_opts.context = Some(target.0.clone());
                                    let session2 = session.clone();
                                    let tx2 = tx.clone();
                                    let pod_ar2 = pod_ar.clone();
                                    let current_client2 = current_client.clone();
                                    tokio::spawn(async move {
                                        switch_cluster(
                                            session2,
                                            target,
                                            tx2.clone(),
                                            move || async move {
                                                cluster::connect_with(&switch_opts).await
                                            },
                                            move |client, store| {
                                                set_current_client(
                                                    &current_client2,
                                                    client.clone(),
                                                );
                                                // Contexts frequently set no namespace and
                                                // `default` is empty on these clusters — the
                                                // picker overrides whatever `-A`/`-n` chose
                                                // for the INITIAL connect with all-namespaces
                                                // on every subsequent switch.
                                                supervise(
                                                    "watch",
                                                    spawn_watch(
                                                        client,
                                                        pod_ar2,
                                                        None,
                                                        store,
                                                        tx2.clone(),
                                                    ),
                                                    tx2,
                                                )
                                            },
                                        )
                                        .await;
                                    });
                                }
                            }
                            Overlay::NamespacePicker(p) => {
                                if let Some(label) = resolve_picker_choice(&p, i) {
                                    let ns_choice = namespace_choice_from_label(&label);
                                    let client_now = get_current_client(&current_client);
                                    let mut s = session.lock().await;
                                    s.handles.abort_all();
                                    s.store = Arc::new(RwLock::new(ResourceStore::new()));
                                    let new_store = s.store.clone();
                                    let handle = spawn_watch(
                                        client_now,
                                        pod_ar.clone(),
                                        ns_choice.clone(),
                                        new_store,
                                        tx.clone(),
                                    );
                                    s.handles.push(supervise("watch", handle, tx.clone()));
                                    drop(s);
                                    display_namespace = ns_choice
                                        .unwrap_or_else(|| ALL_NAMESPACES_LABEL.to_string());
                                    is_fallback_namespace = false;
                                }
                            }
                            Overlay::None => {}
                        }
                        needs_redraw = true;
                    }
                }
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

        hits.clear();
        let show_hint = should_hint_all_namespaces(is_fallback_namespace, objects.len());
        term.draw(|f| {
            render_frame(
                f,
                &objects,
                &pod_gvk,
                &mut view,
                &context_name,
                &display_namespace,
                status,
                last_error.as_deref(),
                show_hint,
                connecting_name.as_deref(),
                &overlay,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Increments a counter when dropped. Aborting a task drops its future, so
    /// this fires on cancellation but not while the task is merely suspended.
    struct DropSignal(Arc<AtomicUsize>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

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

    #[tokio::test]
    async fn a_deliberately_aborted_watch_does_not_quit_the_app() {
        // Every cluster switch aborts the outgoing cluster's watches. A
        // supervisor that treats any Err as a death would send Quit, so the
        // very first switch would exit the application.
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let watch = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        watch.abort();
        supervise("watch", watch, tx)
            .await
            .expect("the supervisor itself must not die");

        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(format!("{e:?}"));
        }
        assert!(
            got.is_empty(),
            "a deliberate abort must produce no error and no quit, got {got:?}"
        );
    }

    #[tokio::test]
    async fn a_panicking_watch_still_quits_the_app() {
        // The other half of the same decision: silencing cancellation must not
        // silence a real crash, which would leave the UI drawing stale data.
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let watch = tokio::spawn(async { panic!("watcher exploded") });
        supervise("watch", watch, tx)
            .await
            .expect("the supervisor itself must not die");

        let mut got = Vec::new();
        while let Ok(e) = rx.try_recv() {
            got.push(e);
        }
        assert!(
            matches!(got.first(), Some(AppEvent::Error(m)) if m.contains("watcher exploded")),
            "the panic payload must reach the user, got {got:?}"
        );
        assert!(
            matches!(got.get(1), Some(AppEvent::Quit)),
            "a dead watch must end the app rather than draw stale data, got {got:?}"
        );
    }

    #[tokio::test]
    async fn aborting_the_supervisor_cancels_the_watch_beneath_it() {
        // Only one owner can await a JoinHandle, and the supervisor needs it to
        // observe a panic — so `WatchHandles` holds the SUPERVISOR's handle.
        // Aborting that must cancel the watch underneath: dropping a JoinHandle
        // DETACHES its task, which would leak exactly the watch a cluster
        // switch is trying to tear down.
        let cancelled = Arc::new(AtomicUsize::new(0));
        let signal = DropSignal(cancelled.clone());
        let watch = tokio::spawn(async move {
            // Held across the await, so it drops only on cancellation.
            let _signal = signal;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;

        let (tx, _rx) = mpsc::unbounded_channel::<AppEvent>();
        let supervisor = supervise("watch", watch, tx);
        tokio::task::yield_now().await;
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            0,
            "nothing should be cancelled yet"
        );

        supervisor.abort();
        for _ in 0..20 {
            if cancelled.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            cancelled.load(Ordering::SeqCst),
            1,
            "aborting the supervisor must cancel its watch, not detach it"
        );
    }

    // --- Task 9: overlays, focus, and input ---

    mod overlay_wiring {
        use super::*;
        use k8s_openapi::api::core::v1::Pod;
        use kube::Client;
        use kube::api::{ApiResource, DynamicObject};
        use kube_tui::app::Overlay;
        use kube_tui::app::event::WatchStatus;
        use kube_tui::cluster::{
            AuthMethod, ClusterEntry, ClusterId, ConnectionState, ContextInfo,
        };
        use kube_tui::ui::hit::HitTarget;
        use kube_tui::ui::theme;
        use kube_tui::ui::views::picker::{Picker, PickerItem};
        use ratatui::backend::TestBackend;

        fn entry(name: &str, state: ConnectionState) -> ClusterEntry {
            ClusterEntry {
                id: ClusterId(name.to_string()),
                context: ContextInfo {
                    name: name.to_string(),
                    cluster: format!("{name}-cluster"),
                    namespace: None,
                    is_current: false,
                    auth: AuthMethod::None,
                },
                state,
            }
        }

        fn pod_in(name: &str, ns: &str) -> Arc<DynamicObject> {
            Arc::new(DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within(ns))
        }

        #[test]
        fn cluster_picker_items_reflect_the_registry_not_a_guess() {
            let entries = vec![
                entry("prod", ConnectionState::Disconnected),
                entry("dev", ConnectionState::Connecting),
                entry(
                    "staging",
                    ConnectionState::Failed {
                        reason: "no route to host".into(),
                    },
                ),
            ];
            let items = cluster_picker_items(&entries);
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].label, "prod");
            assert_eq!(items[0].detail, "");
            assert_eq!(items[1].label, "dev");
            assert!(
                items[1].detail.contains("connecting"),
                "got {:?}",
                items[1].detail
            );
            assert_eq!(items[2].label, "staging");
            assert!(
                items[2].detail.contains("no route to host"),
                "a failure reason must reach the picker, got {:?}",
                items[2].detail
            );
            assert_eq!(items[0].accent, Some(theme::cluster_hue("prod")));
        }

        #[test]
        fn namespace_picker_items_list_distinct_namespaces_plus_all_namespaces() {
            // Namespaces out of alphabetical order and repeated across
            // objects, so dedup and sort are both actually exercised.
            let objects = vec![
                pod_in("a", "zeta"),
                pod_in("b", "alpha"),
                pod_in("c", "zeta"),
                pod_in("d", "prod"),
            ];
            let items = namespace_picker_items(&objects);
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            assert_eq!(
                labels,
                vec![ALL_NAMESPACES_LABEL, "alpha", "prod", "zeta"],
                "all-namespaces sentinel first, then distinct namespaces sorted"
            );
        }

        #[test]
        fn the_all_namespaces_label_maps_to_none() {
            assert_eq!(namespace_choice_from_label(ALL_NAMESPACES_LABEL), None);
        }

        #[test]
        fn a_real_namespace_label_maps_to_itself() {
            assert_eq!(
                namespace_choice_from_label("payments"),
                Some("payments".to_string())
            );
        }

        #[test]
        fn connecting_cluster_name_finds_the_connecting_entry_even_though_a_different_one_is_active()
         {
            // The whole point: the entry actually being switched TO is
            // "dev", not whatever the caller might assume is active. A
            // status bar keyed off `registry.active()` alone would show no
            // sign of this at all.
            let entries = vec![
                entry("prod", ConnectionState::Connected),
                entry("dev", ConnectionState::Connecting),
                entry("staging", ConnectionState::Disconnected),
            ];
            assert_eq!(connecting_cluster_name(&entries), Some("dev".to_string()));
        }

        #[test]
        fn connecting_cluster_name_is_none_when_nothing_is_connecting() {
            let entries = vec![
                entry("prod", ConnectionState::Connected),
                entry("staging", ConnectionState::Disconnected),
            ];
            assert_eq!(connecting_cluster_name(&entries), None);
        }

        #[test]
        fn resolve_picker_choice_maps_the_filtered_index_not_the_raw_one() {
            // Matches picker.rs's own non-vacuous fixture: "wsdc" matches only
            // the item at unfiltered index 4, rendered at filtered position 0.
            // A wrong implementation that skipped filtered_indices would
            // return items[0] ("prod-eu") instead of items[4] ("tst-wsdc").
            let picker = Picker {
                title: "Clusters".into(),
                items: ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]
                    .iter()
                    .map(|n| PickerItem {
                        label: n.to_string(),
                        detail: String::new(),
                        accent: None,
                    })
                    .collect(),
                filter: "wsdc".into(),
                selected: 0,
            };
            assert_eq!(
                resolve_picker_choice(&picker, 0),
                Some("tst-wsdc".to_string())
            );
        }

        #[test]
        fn resolve_picker_choice_out_of_range_is_none_not_a_panic() {
            let picker = Picker {
                title: "T".into(),
                items: vec![PickerItem {
                    label: "only".into(),
                    detail: String::new(),
                    accent: None,
                }],
                filter: String::new(),
                selected: 0,
            };
            assert_eq!(resolve_picker_choice(&picker, 5), None);
        }

        fn offline_client() -> Client {
            let uri: http::Uri = "http://127.0.0.1:1/"
                .parse()
                .expect("a static, well-formed URI");
            Client::try_from(kube::Config::new(uri)).expect("building a client performs no I/O")
        }

        // `Client::try_from` spawns an internal tower buffer task even
        // though it performs no I/O itself, so it needs a Tokio runtime —
        // matching session.rs's own `offline_client` tests.
        #[tokio::test]
        async fn current_client_round_trips_through_the_cell() {
            let cell = StdMutex::new(offline_client());
            set_current_client(&cell, offline_client());
            let _ = get_current_client(&cell); // must not panic
        }

        #[tokio::test]
        async fn get_current_client_recovers_from_a_poisoned_mutex() {
            let cell = StdMutex::new(offline_client());
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _guard = cell.lock().expect("not yet poisoned");
                panic!("poison it");
            }));
            // Must recover the value rather than panicking a second time.
            let _ = get_current_client(&cell);
            set_current_client(&cell, offline_client());
        }

        #[test]
        fn render_frame_paints_the_ribbon_in_the_active_clusters_hue() {
            let pods = vec![pod_in("a", "default")];
            let gvk = GroupVersionKind::gvk("", "v1", "Pod");
            let mut view = TableView::new();
            let mut hits = HitRegistry::new();
            let overlay = Overlay::None;

            let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
            term.draw(|f| {
                render_frame(
                    f,
                    &pods,
                    &gvk,
                    &mut view,
                    "prod-eu",
                    "default",
                    WatchStatus::Synced,
                    None,
                    false,
                    None,
                    &overlay,
                    &mut hits,
                );
            })
            .unwrap();

            let buf = term.backend().buffer();
            assert_eq!(
                buf[(0, 0)].style().fg,
                Some(theme::cluster_hue("prod-eu")),
                "the ribbon must be wired into the real draw sequence"
            );
        }

        #[test]
        fn the_overlay_paints_over_the_table_and_status_drawn_before_it() {
            // Draw order is ribbon, table, status, THEN the overlay last.
            //
            // This must be a VISUAL check, not a hit-resolution one:
            // HitRegistry resolves PickerRow over TableRow by Z-INDEX alone
            // (z=1 beats z=0 regardless of registration order — see
            // picker.rs's own adversarial test), so no hit-test can ever
            // distinguish "overlay drawn last" from "overlay drawn first".
            //
            // It must also target a coordinate PROVEN to actually get
            // overwritten by whichever widget draws second, not merely one
            // that happens to sit inside the overlapping rect: `render_table`
            // sets no background style on its own `Block`, and `Row`/`Cell`
            // rendering writes only the cells its text glyphs occupy — a
            // short label like the picker's own title can survive a wrong
            // draw order purely by chance, landing in a gap between column
            // glyphs, which is a vacuous fixture Task 9 was warned about
            // ("would a wrong implementation give a different answer with
            // this data?"). Empirically dumping the buffer under the
            // reversed order confirmed exactly that for the title text, but
            // also that a data row's STATUS cell ("Unknown", stub pods have
            // no real status) DOES land on and overwrite the picker's own
            // border dashes at (x=48, y=5) for this geometry — 30 pods so
            // the table has enough rows to reach the picker's row, and pod-3
            // (drawn at y=5, the picker's own top border row given
            // `centered` on an 80x24 frame) puts its STATUS column
            // (columns_for: NAME Fill(2), READY 7, STATUS 14 — starting at
            // x=48 after NAME+READY+spacing) squarely inside it.
            let pods: Vec<Arc<DynamicObject>> = (0..30)
                .map(|i| pod_in(&format!("pod-{i}"), "default"))
                .collect();
            let gvk = GroupVersionKind::gvk("", "v1", "Pod");
            let mut view = TableView::new();
            let mut hits = HitRegistry::new();
            let overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items: vec![PickerItem {
                    label: "prod".into(),
                    detail: String::new(),
                    accent: None,
                }],
                filter: String::new(),
                selected: 0,
            });

            let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
            term.draw(|f| {
                render_frame(
                    f,
                    &pods,
                    &gvk,
                    &mut view,
                    "prod",
                    "default",
                    WatchStatus::Synced,
                    None,
                    false,
                    None,
                    &overlay,
                    &mut hits,
                );
            })
            .unwrap();

            let buf = term.backend().buffer();
            let row5: String = (0..80u16)
                .map(|x| buf[(x, 5)].symbol().to_string())
                .collect();
            assert!(
                !row5.contains("Unknown"),
                "a data row's STATUS cell must not bleed through the picker's own \
                 border row — the overlay was not drawn last:\n{row5}"
            );
            assert!(
                row5.contains("Clusters"),
                "the picker's title must still be present:\n{row5}"
            );

            let mut found_picker_row = false;
            for y in 0..24u16 {
                for x in 0..80u16 {
                    if matches!(hits.hit(x, y), Some(HitTarget::PickerRow(_))) {
                        found_picker_row = true;
                    }
                }
            }
            assert!(
                found_picker_row,
                "the picker's hit zones must resolve over the table's"
            );
        }
    }
}
