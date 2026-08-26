use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube_tui::app::Overlay;
use kube_tui::app::event::{AppEvent, Coalesced, WatchStatus, coalesce};
use kube_tui::app::input::{Action, action_for, apply_selection};
use kube_tui::app::session::{
    Session, SessionEvent, SharedSession, is_deliberate_abort, restart_watch, switch_cluster,
};
use kube_tui::cli::{CliOutcome, NamespaceScope, parse_args, should_hint_all_namespaces};
use kube_tui::cluster;
use kube_tui::cluster::{
    AuthMethod, ClusterEntry, ClusterId, ClusterRegistry, ConnectionState, NamespaceListError,
    is_valid_namespace_name, list_namespaces,
};
use kube_tui::store::watch::spawn_watch;
use kube_tui::terminal::{RealTerminal, TerminalGuard, install_panic_hook};
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::ribbon::{render_ribbon, split_ribbon};
use kube_tui::ui::theme;
use kube_tui::ui::views::picker::{
    Picker, PickerItem, centered, clamp_selection, filtered_indices, render_picker,
};
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{TableView, render_table};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::sync::Mutex;
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

/// Merge the namespace picker's three sources — the API's own listing (when
/// permitted), the namespaces seen in objects already loaded, and the
/// namespace currently being watched — into one deduplicated, alphabetically
/// sorted list of names.
///
/// All three matter independently, in either direction: the API can know
/// about a namespace the current watch hasn't loaded a single object from
/// yet (an empty namespace, or one this watch is simply scoped away from),
/// and the current watch can hold objects in a namespace the API listing
/// call didn't return — e.g. it was created after that GET completed.
/// Neither source is trusted over the other; the union is what the picker
/// offers.
fn merge_namespace_names<'a>(
    _api: Option<&'a [String]>,
    loaded: impl Iterator<Item = &'a str>,
    _current: Option<&'a str>,
) -> Vec<String> {
    // STUB (failing-tests commit): uses only the loaded-objects source,
    // ignoring the API listing and the current namespace entirely. Real
    // implementation lands in the next commit.
    let names: BTreeSet<&str> = loaded.collect();
    names.into_iter().map(str::to_string).collect()
}

/// Build the namespace picker's item list.
///
/// `api_namespaces` is the last answer `cluster::namespaces::list_namespaces`
/// gave (`Session::namespaces_from_api`), `objects` is what the current watch
/// has actually loaded, and `current` is the namespace scope in effect right
/// now (`None` for all-namespaces). Listing namespaces is itself
/// cluster-scoped and can be forbidden by the same RBAC that forbids listing
/// pods — exactly the cluster where this picker is needed most, since that
/// RBAC is also why `objects` can be empty. When it is, the "all namespaces"
/// entry (always present, so the picker is never a bare empty box) carries an
/// explanation instead of silently offering a list that looks complete but
/// isn't: typing a name and pressing Enter is the one thing that still works
/// with no listing permission at all (see `main`'s `resolve_confirm`).
fn namespace_picker_items(
    api_namespaces: Option<&Result<Vec<String>, NamespaceListError>>,
    objects: &[Arc<DynamicObject>],
    current: Option<&str>,
) -> Vec<PickerItem> {
    let loaded: BTreeSet<String> = objects
        .iter()
        .filter_map(|o| o.metadata.namespace.clone())
        .collect();

    // STUB (failing-tests commit): an `Err` (including Forbidden) is treated
    // exactly like "nothing fetched yet" — no explanation is ever shown, so
    // a forbidden listing looks identical to one nobody has asked for. Real
    // implementation lands in the next commit.
    let (api_list, forbidden_note): (Option<&[String]>, Option<String>) = match api_namespaces {
        Some(Ok(list)) => (Some(list.as_slice()), None),
        Some(Err(_)) => (None, None),
        None => (None, None),
    };

    let names = merge_namespace_names(api_list, loaded.iter().map(String::as_str), current);

    let mut all_detail = "watch every namespace".to_string();
    if current.is_none() {
        all_detail.push_str("  ·  current");
    }
    if let Some(note) = &forbidden_note {
        all_detail.push_str("  ·  ");
        all_detail.push_str(note);
    }

    let mut items = Vec::with_capacity(names.len() + 1);
    items.push(PickerItem {
        label: ALL_NAMESPACES_LABEL.to_string(),
        detail: all_detail,
        accent: if current.is_none() {
            Some(theme::VIRIDIAN)
        } else {
            None
        },
    });
    items.extend(names.into_iter().map(|n| {
        let is_current = current == Some(n.as_str());
        PickerItem {
            detail: if is_current {
                "current".to_string()
            } else {
                String::new()
            },
            accent: if is_current {
                Some(theme::VIRIDIAN)
            } else {
                None
            },
            label: n,
        }
    }));
    items
}

/// How a namespace scope reads in the status bar.
///
/// The inverse of `namespace_choice_from_label`: `None` is the all-namespaces
/// scope, which has no namespace name to print. Derived from the session's
/// scope on every frame rather than tracked alongside it, so the bar can
/// never name a namespace no watch is actually watching.
fn display_namespace(namespace: Option<&str>) -> &str {
    namespace.unwrap_or(ALL_NAMESPACES_LABEL)
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

/// The filtered-list index a confirmed picker choice refers to, whether it
/// came from a click or from Enter.
///
/// `PickerSelect` already carries the index a click landed on. `PickerConfirm`
/// (Enter) carries none — it confirms whatever the picker currently has
/// highlighted, `Picker::selected`, itself a filtered-list index per
/// `picker.rs`'s own contract (`render_picker` compares it against `row`,
/// enumerated over the FILTERED matches). Any other action, or `PickerConfirm`
/// with no picker open, resolves nothing.
fn confirm_index_for(action: Action, overlay: &Overlay) -> Option<usize> {
    match action {
        Action::PickerSelect(i) => Some(i),
        Action::PickerConfirm => overlay.picker().map(|p| p.selected),
        _ => None,
    }
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

/// What confirming the open picker (Enter, or a click on a specific row)
/// resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerOutcome {
    ClusterChosen(String),
    /// `None` is the all-namespaces scope, same convention as
    /// `namespace_choice_from_label`.
    NamespaceChosen(Option<String>),
    /// The filter matched no item, and the typed text isn't a name a
    /// namespace could ever have — rejected before it becomes a request
    /// that is certain to fail.
    InvalidNamespaceTyped(String),
    NoOp,
}

/// Resolve a confirmed picker index to what it actually means to do.
///
/// For the cluster picker this is unchanged from before: an index that
/// resolves to nothing (nothing was open, or the index is stale) is a no-op.
/// For the namespace picker, a resolved index still wins — an item the user
/// can see and pick always takes priority over whatever they typed to narrow
/// down to it. Only when the filter matches NOTHING is the typed text tried
/// as a namespace name in its own right: this is the one path that needs no
/// API permission at all, so it is what still works on a cluster where
/// listing namespaces (like listing pods) is forbidden — see
/// `namespace_picker_items`'s doc comment for the other half of that fix.
fn resolve_confirm(overlay: &Overlay, index: Option<usize>) -> PickerOutcome {
    // STUB (failing-tests commit): always treats the picker's typed filter as
    // the answer, even when it matches an existing item — the exact
    // regression the "existing behaviour must not regress" test guards
    // against — and never resolves a cluster picker choice at all. Real
    // implementation lands in the next commit.
    let _ = index;
    match overlay {
        Overlay::None | Overlay::ClusterPicker(_) => PickerOutcome::NoOp,
        Overlay::NamespacePicker(p) => {
            let typed = p.filter.trim();
            if typed.is_empty() {
                PickerOutcome::NoOp
            } else {
                PickerOutcome::NamespaceChosen(Some(typed.to_string()))
            }
        }
    }
}

/// Turn a failed connect into something the user can act on.
///
/// Connects made from inside the TUI forbid exec plugins from prompting (see
/// `ConnectOptions::allow_interactive_auth`), so on a cluster behind SSO the
/// usual failure is not "wrong password" but "this credential needed a login
/// and we would not let it ask". The status bar has room for one line, and
/// the plugin's own name is the difference between a mystery and an
/// instruction — the fix is always to run it, or any `kubectl` command, in a
/// real shell and come back.
///
/// A blank command (`exec:` with no `command:`) yields no hint: naming
/// nothing helps nobody.
fn connect_failure_hint(auth: &AuthMethod, error: &str) -> String {
    match auth {
        AuthMethod::Exec { command } if !command.is_empty() => {
            format!("{error} — '{command}' needs to log in; run it in a shell first")
        }
        _ => error.to_string(),
    }
}

/// The longest error text that may reach the status bar.
const MAX_ERROR_CHARS: usize = 200;

/// Cap an error at something a one-line status bar can plausibly carry.
///
/// The bar is one row and already width-truncates, so length costs nothing
/// visually — but it is also what bounds an error we do not control the
/// contents of. `kube::client::auth::Error::AuthExecRun` formats
/// `out: std::process::Output` with `{out:?}`, which includes the credential
/// plugin's stdout — where a partial `ExecCredential` (token and all) sits if
/// the plugin exits non-zero after printing one. It renders as a decimal byte
/// array, and nothing in this app writes logs, so it never leaves the screen;
/// capping it keeps it from being the whole line as well.
///
/// Truncates by CHARACTERS, not bytes: an error containing multi-byte text
/// (a cluster name, a server-supplied message) would panic a byte slice on a
/// character boundary.
fn truncate_error(e: String) -> String {
    if e.chars().count() <= MAX_ERROR_CHARS {
        return e;
    }
    let mut out: String = e.chars().take(MAX_ERROR_CHARS).collect();
    out.push('…');
    out
}

/// The error the status bar should show after this batch of events.
///
/// Nothing used to clear `last_error`, so a single watch blip on prod pinned
/// an error for the rest of the session: switch to dev, watch it connect and
/// stream 250 pods, and the bar still reads `dev · … · 250 items · live`
/// beside prod's dead error — permanently, and permanently suppressing the
/// all-namespaces hint with it (`status.rs`).
///
/// Two things retire an error, both meaning "whatever went wrong is no
/// longer what is happening": a switch that actually connected, and this
/// kind's watch reporting itself synced. The kind is checked because
/// `status_changes` carries every watched kind, and another kind's health
/// says nothing about this one's error.
///
/// Clearing happens BEFORE new errors are applied, so an error arriving in
/// the same batch as a sync still shows. `coalesce` keeps errors and status
/// changes in separate lists and their relative order is lost, so this is a
/// deliberate choice between two risks: showing a resolved error one batch
/// too long, or hiding a live one. Only the first is recoverable.
fn next_error(
    previous: Option<String>,
    batch: &Coalesced,
    gvk: &GroupVersionKind,
) -> Option<String> {
    let mut error = previous;

    let connected = batch
        .session_events
        .iter()
        .any(|e| matches!(e, SessionEvent::Connected(_)));
    let synced = batch
        .status_changes
        .iter()
        .any(|(k, s)| k == gvk && *s == WatchStatus::Synced);
    if connected || synced {
        error = None;
    }

    // Everything that becomes a visible error passes through `truncate_error`
    // here — one choke point, so no future error source can bypass the cap.
    for e in &batch.session_events {
        if let SessionEvent::ConnectFailed { id, reason } = e {
            error = Some(truncate_error(format!("connecting to {}: {reason}", id.0)));
        }
    }
    if let Some(e) = batch.errors.last() {
        error = Some(truncate_error(e.clone()));
    }
    error
}

/// Everything one frame needs out of the store, read in a single lock
/// acquisition.
///
/// Objects and watch health must come from the SAME store, because a cluster
/// switch replaces the store wholesale (`switch_cluster` step 5). Keeping
/// health in a separate local fed by `WatchStatus` events instead means a
/// switch empties the table while the old cluster's "live" survives — an
/// empty table presented as fresh, which `ui/views/status.rs` calls out as
/// the worst failure mode for an ops tool. A replaced store reports
/// `Initialising` by construction, so reading both here makes that state
/// unrepresentable rather than something to remember to reset.
async fn store_snapshot(
    store: &kube_tui::store::watch::SharedStore,
    gvk: &GroupVersionKind,
) -> (Vec<Arc<DynamicObject>>, WatchStatus) {
    let s = store.read().await;
    (s.objects(gvk), s.status(gvk))
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
    // Mutable because the picker owns its scroll offset and advances it
    // during the draw, exactly as `TableView` does — see `render_picker`.
    overlay: &mut Overlay,
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

    if let Some(picker) = overlay.picker_mut() {
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
    // straight to stderr rather than corrupting an alternate screen — and,
    // for the same reason, this is the ONE connect where an exec credential
    // plugin may legitimately take stdin and stderr and walk the user
    // through an SSO login. `opts` itself keeps the safe default, so the
    // clone every cluster switch takes cannot inherit this permission.
    let startup_opts = cluster::ConnectOptions {
        allow_interactive_auth: true,
        ..opts.clone()
    };
    let client = match cluster::connect_with(&startup_opts).await {
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

    // Resolve the CLI scope to the namespace the watch will use and whether
    // that is a fallback. The fallback flag is true when we're watching the
    // "default" namespace because the context didn't specify one (not because
    // the user chose it explicitly or via -n); it is the only condition under
    // which the "try -A" hint applies, and only until the user chooses a
    // scope themselves. The DISPLAY string is not computed here — it is
    // derived from the session on every frame, so it can never name a scope
    // no watch is using.
    let (watch_namespace, is_fallback_namespace) = match cli_scope {
        NamespaceScope::One(ns) => (Some(ns), false),
        NamespaceScope::All => (None, false),
        NamespaceScope::FromContext => {
            let is_fallback = !namespace_from_context && context_namespace == "default";
            (Some(context_namespace), is_fallback)
        }
    };

    // Everything belonging to "the cluster on screen" lives behind one handle
    // so that a switch can replace it wholesale — the store in particular is
    // replaced rather than cleared. See `switch_cluster`. `Session` also
    // owns the Client for whichever cluster is active: a namespace switch
    // reads it from the SAME lock it uses to restart the watch (see
    // `restart_watch`), rather than from a separate cell elsewhere that
    // could go stale relative to a concurrent cluster switch. The namespace
    // scope lives there for the same reason: it describes the watch that is
    // actually running, so it is read back from the session each frame
    // rather than tracked alongside it.
    let session: SharedSession = Arc::new(Mutex::new(Session::new(
        ClusterRegistry::from_contexts(contexts),
        client.clone(),
        watch_namespace.clone(),
        is_fallback_namespace,
    )));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

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
        // Errors are raised AND retired here: see `next_error`. Without the
        // retiring half a single blip pins an error across every subsequent
        // switch.
        let updated_error = next_error(last_error.clone(), &batch, &pod_gvk);
        if updated_error != last_error {
            last_error = updated_error;
            needs_redraw = true;
        }
        // The status itself is read back off the store below, not carried in a
        // local — see `store_snapshot`. The event only says "something
        // changed", which is all a redraw needs.
        if !batch.status_changes.is_empty() {
            needs_redraw = true;
        }
        // A switch changes the ribbon, the status bar and the whole table, and
        // "connecting" must appear before the attempt that produced it
        // finishes — which is the entire reason it is announced as an event.
        if !batch.session_events.is_empty() {
            needs_redraw = true;
        }
        // The answer to a namespace-listing fetch (spawned when the picker
        // opened; see `Action::OpenNamespacePicker` below) arrives here.
        // Written under the session lock, same as `client`/`namespace`
        // themselves — see `Session::namespaces_from_api`'s doc comment for
        // why this must not live in a local the event loop threads through
        // frames on its own. Done BEFORE the snapshot read just below, so an
        // answer that arrives in this batch is what that read actually sees.
        if let Some(result) = batch.namespace_list.clone() {
            session.lock().await.namespaces_from_api = Some(result);
            needs_redraw = true;
        }

        // A cluster switch REPLACES the store and changes which cluster is
        // active, so both are re-read every pass rather than captured once: a
        // clone taken before a switch would keep showing the previous
        // cluster's objects under the previous cluster's name for ever.
        //
        // The session guard is released before the store is locked — holding
        // it across an await would block `switch_cluster`, which needs it to
        // announce "connecting" while this loop is still running.
        // The namespace scope and its fallback flag come from the same guard:
        // a switch replaces all of these together, so reading any of them
        // from a local would show the previous cluster's scope over the new
        // cluster's data.
        let (
            store,
            active_cluster,
            entries,
            namespace,
            namespace_is_fallback,
            client,
            namespaces_from_api,
        ) = {
            let s = session.lock().await;
            (
                s.store.clone(),
                s.registry.active().map(|e| e.id.0.clone()),
                s.registry.entries().to_vec(),
                s.namespace.clone(),
                s.namespace_is_fallback,
                s.client.clone(),
                s.namespaces_from_api.clone(),
            )
        };
        let context_name = active_cluster.unwrap_or_else(|| startup_context_name.clone());
        let scope = display_namespace(namespace.as_deref());
        let connecting_name = connecting_cluster_name(&entries);
        // Read the store snapshot into locals before drawing; the render
        // closure below must be synchronous and must not acquire any locks.
        // Objects and watch health come from one acquisition of one store, so
        // a switch cannot show the previous cluster's health over this
        // cluster's (empty) object list.
        let (objects, status) = store_snapshot(&store, &pod_gvk).await;

        // Keep an open picker's items current: the registry (cluster
        // states) and the object list (namespaces seen) can both change
        // while it's on screen, and it must reflect that rather than a
        // snapshot taken at open time. The filter is untouched; the
        // selection is only clamped, never moved, because a shrinking list
        // can otherwise leave it naming a row that no longer exists — a
        // confirm on which resolves to nothing and closes the picker having
        // silently done nothing at all.
        match &mut overlay {
            Overlay::ClusterPicker(p) => {
                p.items = cluster_picker_items(&entries);
                clamp_selection(p);
            }
            Overlay::NamespacePicker(p) => {
                p.items = namespace_picker_items(
                    namespaces_from_api.as_ref(),
                    &objects,
                    namespace.as_deref(),
                );
                clamp_selection(p);
            }
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
            let confirm_index = confirm_index_for(action, &overlay);
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
                        scroll: 0,
                    });
                    needs_redraw = true;
                }
                Action::OpenNamespacePicker => {
                    // Opens immediately with whatever is already known — the
                    // objects already loaded, the previous fetch if one has
                    // ever completed, and the namespace currently being
                    // watched — never blocking on the network. The listing
                    // itself is I/O, so it runs on a spawned task and its
                    // answer arrives back through `AppEvent::NamespacesListed`
                    // (handled above, before this loop's snapshot read),
                    // which is what lets the picker fill in without the draw
                    // that opened it ever waiting on it.
                    overlay = Overlay::NamespacePicker(Picker {
                        title: "Namespaces".into(),
                        items: namespace_picker_items(
                            namespaces_from_api.as_ref(),
                            &objects,
                            namespace.as_deref(),
                        ),
                        filter: String::new(),
                        selected: 0,
                        scroll: 0,
                    });
                    let fetch_client = client.clone();
                    let tx2 = tx.clone();
                    tokio::spawn(async move {
                        let result = list_namespaces(&fetch_client).await;
                        let _ = tx2.send(AppEvent::NamespacesListed(result));
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
                    match resolve_confirm(&overlay, confirm_index) {
                        PickerOutcome::NoOp => {}
                        PickerOutcome::InvalidNamespaceTyped(typed) => {
                            // Left open rather than closed: the picker
                            // couldn't have done anything with this text
                            // anyway, so closing it would just make the user
                            // reopen it to try again. `is_valid_namespace_name`
                            // rejected it locally — no request was ever sent.
                            let _ = tx.send(AppEvent::Error(format!(
                                "'{typed}' is not a valid namespace name — lowercase \
                                 alphanumerics and '-', 1-63 chars, cannot start or end with '-'"
                            )));
                            needs_redraw = true;
                        }
                        PickerOutcome::ClusterChosen(label) => {
                            overlay = Overlay::None;
                            let target = ClusterId(label);
                            // Inherits `allow_interactive_auth: false`
                            // from `opts`: this connect runs with the
                            // alternate screen live, so no exec plugin
                            // may prompt into it. See
                            // `ConnectOptions::allow_interactive_auth`.
                            let mut switch_opts = opts.clone();
                            switch_opts.context = Some(target.0.clone());
                            // How the target authenticates, so a
                            // refusal to prompt can say which command
                            // to run instead of just failing.
                            let target_auth = entries
                                .iter()
                                .find(|e| e.id == target)
                                .map(|e| e.context.auth.clone())
                                .unwrap_or(AuthMethod::None);
                            let session2 = session.clone();
                            let tx2 = tx.clone();
                            let pod_ar2 = pod_ar.clone();
                            tokio::spawn(async move {
                                switch_cluster(
                                    session2,
                                    target,
                                    // Contexts frequently set no namespace and
                                    // `default` is empty on these clusters — so a
                                    // switch overrides whatever `-A`/`-n` chose for
                                    // the INITIAL connect with all-namespaces.
                                    // `switch_cluster` records this scope on the
                                    // session and hands the SAME value to the closure
                                    // below, so what the status bar reports and what
                                    // the watch actually watches cannot disagree.
                                    None,
                                    tx2.clone(),
                                    move || async move {
                                        cluster::connect_with(&switch_opts).await.map_err(|e| {
                                            anyhow::anyhow!(connect_failure_hint(
                                                &target_auth,
                                                &format!("{e:#}")
                                            ))
                                        })
                                    },
                                    move |client, store, ns| {
                                        supervise(
                                            "watch",
                                            spawn_watch(client, pod_ar2, ns, store, tx2.clone()),
                                            tx2,
                                        )
                                    },
                                )
                                .await;
                            });
                            needs_redraw = true;
                        }
                        PickerOutcome::NamespaceChosen(ns_choice) => {
                            overlay = Overlay::None;
                            let pod_ar2 = pod_ar.clone();
                            let tx2 = tx.clone();
                            // `restart_watch` reads the session's CURRENT client
                            // from the same lock it uses to tear down and replace
                            // the store — not a copy captured earlier, which could
                            // have gone stale if a cluster switch completed in the
                            // gap between capturing it and taking the lock. See
                            // `Session::client`'s doc comment for the interleaving
                            // this closes. It records the scope under that same
                            // guard and passes it on to the closure, so nothing
                            // here has to update a display copy afterwards.
                            restart_watch(session.clone(), ns_choice, move |client, store, ns| {
                                supervise(
                                    "watch",
                                    spawn_watch(client, pod_ar2, ns, store, tx2.clone()),
                                    tx2,
                                )
                            })
                            .await;
                            needs_redraw = true;
                        }
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
        let show_hint = should_hint_all_namespaces(namespace_is_fallback, objects.len());
        term.draw(|f| {
            render_frame(
                f,
                &objects,
                &pod_gvk,
                &mut view,
                &context_name,
                scope,
                status,
                last_error.as_deref(),
                show_hint,
                connecting_name.as_deref(),
                &mut overlay,
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
            // objects, so dedup and sort are both actually exercised. No API
            // listing and no current scope, so this exercises only the
            // loaded-objects source.
            let objects = vec![
                pod_in("a", "zeta"),
                pod_in("b", "alpha"),
                pod_in("c", "zeta"),
                pod_in("d", "prod"),
            ];
            let items = namespace_picker_items(None, &objects, None);
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            assert_eq!(
                labels,
                vec![ALL_NAMESPACES_LABEL, "alpha", "prod", "zeta"],
                "all-namespaces sentinel first, then distinct namespaces sorted"
            );
        }

        // --- The three sources: API listing, loaded objects, current scope ---

        #[test]
        fn merge_namespace_names_deduplicates_sorts_and_keeps_every_sources_own_name() {
            // Each source contributes a name the others don't have, and the
            // API list is handed in already out of sorted order — a wrong
            // implementation that merely concatenated and happened to sort
            // only one source would still diverge from this. "zeta" is
            // shared between the API and loaded sources, so dedup is
            // actually exercised too, not just union.
            let api = vec!["zeta".to_string(), "mercury".to_string()];
            let loaded = vec!["alpha", "zeta"];
            let names = merge_namespace_names(Some(&api), loaded.into_iter(), Some("venus"));
            assert_eq!(
                names,
                vec![
                    "alpha".to_string(),
                    "mercury".to_string(),
                    "venus".to_string(),
                    "zeta".to_string(),
                ],
                "expected the sorted union of all three sources, deduplicated"
            );
        }

        #[test]
        fn namespace_picker_items_includes_a_namespace_seen_only_in_loaded_objects() {
            // The API list is missing "beta" entirely — a watch can see an
            // object in a namespace created after the listing GET completed.
            let api: Result<Vec<String>, NamespaceListError> = Ok(vec!["alpha".to_string()]);
            let objects = vec![pod_in("a", "beta")];
            let items = namespace_picker_items(Some(&api), &objects, None);
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(
                labels.contains(&"beta"),
                "a namespace seen only in loaded objects must still appear; got {labels:?}"
            );
        }

        #[test]
        fn namespace_picker_items_includes_a_namespace_returned_only_by_the_api() {
            // The reverse: nothing has been loaded into the table yet (the
            // exact shape of the reported bug — 0 objects), but the API
            // already knows about "gamma".
            let api: Result<Vec<String>, NamespaceListError> = Ok(vec!["gamma".to_string()]);
            let objects: Vec<Arc<DynamicObject>> = vec![];
            let items = namespace_picker_items(Some(&api), &objects, None);
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            assert!(
                labels.contains(&"gamma"),
                "a namespace known only to the API must still appear; got {labels:?}"
            );
        }

        #[test]
        fn namespace_picker_items_marks_the_current_namespace() {
            let api: Result<Vec<String>, NamespaceListError> =
                Ok(vec!["alpha".to_string(), "beta".to_string()]);
            let objects: Vec<Arc<DynamicObject>> = vec![];
            let items = namespace_picker_items(Some(&api), &objects, Some("beta"));
            let beta = items
                .iter()
                .find(|i| i.label == "beta")
                .expect("beta must be listed");
            let alpha = items
                .iter()
                .find(|i| i.label == "alpha")
                .expect("alpha must be listed");
            assert!(
                beta.detail.contains("current"),
                "the current namespace must be marked as such; got {:?}",
                beta.detail
            );
            assert!(
                !alpha.detail.contains("current"),
                "a namespace that isn't current must not be marked as one; got {:?}",
                alpha.detail
            );
        }

        #[test]
        fn a_forbidden_listing_shows_an_explanation_instead_of_an_empty_picker() {
            // The exact shape of the reported bug: 0 objects loaded (pods
            // forbidden at cluster scope) AND listing namespaces itself
            // forbidden. The picker must not look like an empty box the user
            // can't tell is broken from one that is working correctly.
            let api: Result<Vec<String>, NamespaceListError> =
                Err(NamespaceListError::Forbidden("nope".to_string()));
            let objects: Vec<Arc<DynamicObject>> = vec![];
            let items = namespace_picker_items(Some(&api), &objects, None);
            assert!(
                !items.is_empty(),
                "the picker must never render as a bare empty list"
            );
            assert!(
                items
                    .iter()
                    .any(|i| i.detail.contains("could not be listed")),
                "the picker must explain that listing failed; got {:?}",
                items.iter().map(|i| &i.detail).collect::<Vec<_>>()
            );
            assert!(
                items
                    .iter()
                    .any(|i| i.detail.to_lowercase().contains("type")
                        && i.detail.to_lowercase().contains("enter")),
                "the picker must say a name can be typed directly; got {:?}",
                items.iter().map(|i| &i.detail).collect::<Vec<_>>()
            );
        }

        // --- resolve_confirm: what Enter (or a click) actually does ---

        fn ns_items(labels: &[&str]) -> Vec<PickerItem> {
            labels
                .iter()
                .map(|n| PickerItem {
                    label: n.to_string(),
                    detail: String::new(),
                    accent: None,
                })
                .collect()
        }

        #[test]
        fn resolve_confirm_for_cluster_picker_selects_the_resolved_item() {
            // Unchanged behaviour for the cluster picker, now routed through
            // the shared `resolve_confirm` rather than inline in the event
            // loop.
            let overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items: ns_items(&["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]),
                filter: "wsdc".into(),
                selected: 0,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(
                resolve_confirm(&overlay, index),
                PickerOutcome::ClusterChosen("tst-wsdc".to_string())
            );
        }

        #[test]
        fn resolve_confirm_for_cluster_picker_with_no_match_does_not_invent_a_cluster() {
            // There is no type-to-enter escape hatch for clusters — a
            // kubeconfig either has the context or it doesn't, and a typo
            // must not attempt to connect to whatever was typed.
            let overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items: ns_items(&["prod-eu", "prod-us"]),
                filter: "no-such-cluster".into(),
                selected: 0,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(resolve_confirm(&overlay, index), PickerOutcome::NoOp);
        }

        #[test]
        fn resolve_confirm_selects_the_matching_item_over_the_typed_filter_text() {
            // The existing-behaviour regression guard: filter "e" matches
            // "prod-eu" (filtered position 0) and "dev" (filtered position
            // 1); with `selected = 1` the picker is highlighting "dev". If
            // Enter used the typed text "e" instead — itself a
            // syntactically valid namespace name — this would resolve to
            // "e", a different (and wrong) answer, so the fixture actually
            // discriminates the two implementations.
            let overlay = Overlay::NamespacePicker(Picker {
                title: "Namespaces".into(),
                items: ns_items(&["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]),
                filter: "e".into(),
                selected: 1,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(
                resolve_confirm(&overlay, index),
                PickerOutcome::NamespaceChosen(Some("dev".to_string())),
                "an item the user can see and pick must win over the typed filter"
            );
        }

        #[test]
        fn resolve_confirm_treats_unmatched_valid_filter_text_as_a_namespace_to_switch_to() {
            let overlay = Overlay::NamespacePicker(Picker {
                title: "Namespaces".into(),
                items: ns_items(&[ALL_NAMESPACES_LABEL, "prod-eu", "prod-us"]),
                filter: "my-new-ns".into(),
                selected: 0,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(
                resolve_confirm(&overlay, index),
                PickerOutcome::NamespaceChosen(Some("my-new-ns".to_string())),
                "the only path that needs no listing permission at all must still work"
            );
        }

        #[test]
        fn resolve_confirm_rejects_unmatched_invalid_filter_text() {
            let overlay = Overlay::NamespacePicker(Picker {
                title: "Namespaces".into(),
                items: ns_items(&[ALL_NAMESPACES_LABEL, "prod-eu"]),
                filter: "Not Valid!".into(),
                selected: 0,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(
                resolve_confirm(&overlay, index),
                PickerOutcome::InvalidNamespaceTyped("Not Valid!".to_string()),
                "a name that could never be valid must be rejected, not sent to the apiserver"
            );
        }

        #[test]
        fn resolve_confirm_with_an_empty_filter_selects_whatever_is_highlighted() {
            // An empty filter matches everything (`filtered_indices`'s own
            // contract), so this must never fall into the typed-text branch.
            let overlay = Overlay::NamespacePicker(Picker {
                title: "Namespaces".into(),
                items: ns_items(&[ALL_NAMESPACES_LABEL, "prod-eu"]),
                filter: String::new(),
                selected: 1,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(
                resolve_confirm(&overlay, index),
                PickerOutcome::NamespaceChosen(Some("prod-eu".to_string()))
            );
        }

        // --- A refused exec login must say what to run ---

        #[test]
        fn a_failed_connect_to_an_exec_cluster_names_the_plugin_to_run() {
            // Switching to an SSO cluster with an expired token now fails
            // cleanly rather than printing the plugin's login URL into our
            // alternate screen. That is only an improvement if the user can
            // tell what to do about it, and the command name is the whole
            // instruction.
            let hint = connect_failure_hint(
                &AuthMethod::Exec {
                    command: "kubelogin".to_string(),
                },
                "building config for context 'prod-eu': auth exec command failed",
            );
            assert!(
                hint.contains("kubelogin"),
                "the plugin to run must be named; got {hint}"
            );
            assert!(
                hint.contains("auth exec command failed"),
                "the underlying cause must survive; got {hint}"
            );
        }

        #[test]
        fn a_failed_connect_to_a_non_exec_cluster_is_left_exactly_as_it_was() {
            // A cert or token cluster's failure has nothing to do with a
            // credential plugin, and inventing advice about one would send
            // someone chasing the wrong thing.
            let original = "no route to host";
            for auth in [
                AuthMethod::ClientCert,
                AuthMethod::Token,
                AuthMethod::None,
                AuthMethod::AuthProvider {
                    name: "oidc".to_string(),
                },
            ] {
                assert_eq!(
                    connect_failure_hint(&auth, original),
                    original,
                    "{auth:?} must not gain an exec hint"
                );
            }
        }

        #[test]
        fn an_exec_block_with_no_command_adds_no_empty_hint() {
            // `exec.command` is optional in the schema and `auth_method_for`
            // defaults it to "". Advising the user to run '' is worse than
            // saying nothing.
            let original = "no route to host";
            assert_eq!(
                connect_failure_hint(
                    &AuthMethod::Exec {
                        command: String::new()
                    },
                    original
                ),
                original
            );
        }

        // --- Error text is bounded before it reaches the bar ---

        #[test]
        fn a_short_error_is_passed_through_untouched() {
            let e = "forbidden: pods is denied".to_string();
            assert_eq!(truncate_error(e.clone()), e);
        }

        #[test]
        fn an_error_exactly_at_the_limit_is_not_marked_as_truncated() {
            let e = "x".repeat(MAX_ERROR_CHARS);
            assert_eq!(truncate_error(e.clone()), e, "the boundary is inclusive");
        }

        #[test]
        fn an_exec_plugin_dumping_its_stdout_cannot_fill_the_status_bar() {
            // `Error::AuthExecRun` formats `out: std::process::Output` with
            // `{out:?}`, so a plugin that prints a partial ExecCredential and
            // then exits non-zero puts its token into the error as a decimal
            // byte array. Bound it.
            let dump: String = (0..2000)
                .map(|i| format!("{}, ", i % 256))
                .collect::<String>();
            let e = format!("auth exec command failed: Output {{ stdout: [{dump}] }}");
            let out = truncate_error(e);
            assert_eq!(
                out.chars().count(),
                MAX_ERROR_CHARS + 1,
                "capped at the limit plus the ellipsis that marks it"
            );
            assert!(
                out.ends_with('…'),
                "a truncated error must show that it was truncated; got {out}"
            );
            assert!(
                out.starts_with("auth exec command failed"),
                "the useful part is the front, so that is the part kept; got {out}"
            );
        }

        #[test]
        fn truncating_multibyte_text_does_not_split_a_character() {
            // Cluster names, server messages and a plugin's own output can
            // all be non-ASCII. Slicing by BYTES at 200 lands mid-character
            // in this string and panics; slicing by characters does not.
            let e = "→".repeat(300);
            let out = truncate_error(e);
            assert_eq!(out.chars().count(), MAX_ERROR_CHARS + 1);
            assert!(out.starts_with("→→→"));
        }

        #[test]
        fn the_forbidden_watch_remedy_survives_truncation() {
            // The apiserver's own message (which we always append) can be
            // arbitrarily long — it echoes RBAC rule names, resource names,
            // sometimes the requesting identity. If the remedy were appended
            // after that text instead of leading it, this is exactly the
            // scenario that would silently drop it.
            use kube_tui::store::watch::forbidden_message;
            let long_detail = "x".repeat(500);
            let msg = forbidden_message("pods", None, &long_detail);
            let shown = truncate_error(msg);
            assert!(
                shown.contains("-n <namespace>"),
                "the actionable remedy must survive the status bar's truncation budget; got {shown}"
            );
        }

        // --- Errors are retired as well as raised ---

        fn gvk() -> GroupVersionKind {
            GroupVersionKind::gvk("", "v1", "Pod")
        }

        /// A stale error from a cluster the user has since left.
        fn stale() -> Option<String> {
            Some("watch Pod: connection reset by peer".to_string())
        }

        #[test]
        fn a_successful_switch_retires_the_previous_clusters_error() {
            // The reported failure: a blip on prod pins an error; switch to
            // dev, which connects fine and streams 250 pods, and prod's error
            // is still on the bar — for ever, since nothing ever cleared it.
            let batch = coalesce(vec![AppEvent::Session(SessionEvent::Connected(ClusterId(
                "dev".to_string(),
            )))]);
            assert_eq!(
                next_error(stale(), &batch, &gvk()),
                None,
                "an error raised before a switch must not be visible after it"
            );
        }

        #[test]
        fn a_watch_reporting_itself_synced_retires_a_stale_error() {
            let batch = coalesce(vec![AppEvent::WatchStatus {
                gvk: gvk(),
                status: WatchStatus::Synced,
            }]);
            assert_eq!(next_error(stale(), &batch, &gvk()), None);
        }

        #[test]
        fn another_kinds_sync_does_not_retire_this_kinds_error() {
            // `status_changes` carries every watched kind. A Deployment watch
            // coming up says nothing about why the Pod watch failed, and
            // clearing on it would hide a live error.
            let batch = coalesce(vec![AppEvent::WatchStatus {
                gvk: GroupVersionKind::gvk("apps", "v1", "Deployment"),
                status: WatchStatus::Synced,
            }]);
            assert_eq!(
                next_error(stale(), &batch, &gvk()),
                stale(),
                "only the displayed kind's own health may retire its error"
            );
        }

        #[test]
        fn a_watch_that_is_merely_reconnecting_does_not_retire_the_error() {
            // Reconnecting is not recovery. Clearing here would blank the
            // explanation at precisely the moment it is most wanted.
            let batch = coalesce(vec![AppEvent::WatchStatus {
                gvk: gvk(),
                status: WatchStatus::Reconnecting,
            }]);
            assert_eq!(next_error(stale(), &batch, &gvk()), stale());
        }

        #[test]
        fn an_error_arriving_with_a_sync_still_shows() {
            // `coalesce` loses the relative order of errors and status
            // changes, so this is the deliberate tie-break: never hide a live
            // error, at the cost of possibly showing a resolved one one batch
            // longer.
            let batch = coalesce(vec![
                AppEvent::WatchStatus {
                    gvk: gvk(),
                    status: WatchStatus::Synced,
                },
                AppEvent::Error("forbidden: pods is denied".to_string()),
            ]);
            assert_eq!(
                next_error(None, &batch, &gvk()),
                Some("forbidden: pods is denied".to_string())
            );
        }

        #[test]
        fn a_failed_connect_becomes_the_visible_error_naming_the_cluster() {
            let batch = coalesce(vec![AppEvent::Session(SessionEvent::ConnectFailed {
                id: ClusterId("dev".to_string()),
                reason: "no route to host".to_string(),
            })]);
            let e = next_error(None, &batch, &gvk()).expect("a failed connect must be reported");
            assert!(e.contains("dev"), "must name the cluster; got {e}");
            assert!(e.contains("no route to host"), "got {e}");
        }

        #[test]
        fn a_failed_connect_in_the_same_batch_as_a_sync_still_shows() {
            // A switch that fails while the CURRENT cluster's watch is
            // happily syncing: the sync is the old cluster's, and must not
            // swallow the report that the new one is unreachable.
            let batch = coalesce(vec![
                AppEvent::WatchStatus {
                    gvk: gvk(),
                    status: WatchStatus::Synced,
                },
                AppEvent::Session(SessionEvent::ConnectFailed {
                    id: ClusterId("dev".to_string()),
                    reason: "no route to host".to_string(),
                }),
            ]);
            let e = next_error(None, &batch, &gvk()).expect("a failed connect must be reported");
            assert!(e.contains("no route to host"), "got {e}");
        }

        #[test]
        fn a_batch_with_nothing_relevant_leaves_the_error_alone() {
            // Mouse movement must not clear a real error off the bar.
            let batch = coalesce(vec![AppEvent::StoreChanged { gvk: gvk() }]);
            assert_eq!(next_error(stale(), &batch, &gvk()), stale());
        }

        #[test]
        fn a_connecting_announcement_does_not_retire_the_error() {
            // Only a connect that SUCCEEDED means the problem is behind us.
            let batch = coalesce(vec![AppEvent::Session(SessionEvent::Connecting(
                ClusterId("dev".to_string()),
            ))]);
            assert_eq!(next_error(stale(), &batch, &gvk()), stale());
        }

        #[test]
        fn the_displayed_scope_for_all_namespaces_is_the_sentinel_not_a_namespace() {
            // `None` is the all-namespaces scope. Printing an empty string or
            // "default" here would name a scope nothing is watching.
            assert_eq!(display_namespace(None), ALL_NAMESPACES_LABEL);
        }

        #[test]
        fn the_displayed_scope_for_a_real_namespace_is_that_namespace() {
            assert_eq!(display_namespace(Some("payments")), "payments");
            assert_eq!(display_namespace(Some("kube-system")), "kube-system");
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
                scroll: 0,
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
                scroll: 0,
            };
            assert_eq!(resolve_picker_choice(&picker, 5), None);
        }

        #[test]
        fn confirm_index_for_picker_select_carries_its_own_index() {
            // PickerSelect(i) already IS the answer, regardless of what the
            // picker's own `selected` happens to be — a click at filtered
            // row 7 confirms row 7, not whatever was last highlighted.
            let overlay = Overlay::ClusterPicker(Picker {
                title: "T".into(),
                items: vec![PickerItem {
                    label: "a".into(),
                    detail: String::new(),
                    accent: None,
                }],
                filter: String::new(),
                selected: 3,
                scroll: 0,
            });
            assert_eq!(
                confirm_index_for(Action::PickerSelect(7), &overlay),
                Some(7)
            );
        }

        #[test]
        fn confirm_index_for_picker_confirm_uses_the_pickers_own_selection_not_a_hardcoded_zero() {
            // selected=1 under filter "e": matches "prod-eu" (unfiltered
            // index 0, filtered position 0) and "dev" (unfiltered index 3,
            // filtered position 1). Picker::selected (1), the filtered
            // position it names (1), and the real item it will resolve to
            // (unfiltered index 3) are all different from 0 — a
            // PickerConfirm branch hardcoded to 0 would silently confirm
            // "prod-eu" (index 0) instead of "dev" (index 3), and this is
            // the only kind of fixture that can catch that: with
            // selected=0, or a filter that left filtered and unfiltered
            // positions coincident, the mutation would be invisible.
            let items: Vec<PickerItem> = ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]
                .iter()
                .map(|n| PickerItem {
                    label: n.to_string(),
                    detail: String::new(),
                    accent: None,
                })
                .collect();
            let overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items,
                filter: "e".into(),
                selected: 1,
                scroll: 0,
            });
            let index = confirm_index_for(Action::PickerConfirm, &overlay);
            assert_eq!(index, Some(1), "must be the picker's own selection, not 0");

            // Full pipeline, end to end: that index must resolve to "dev",
            // not "prod-eu".
            let picker = overlay.picker().expect("cluster picker is open");
            assert_eq!(
                resolve_picker_choice(picker, index.expect("checked above")),
                Some("dev".to_string())
            );
        }

        #[test]
        fn confirm_index_for_picker_confirm_with_no_overlay_open_is_none() {
            assert_eq!(
                confirm_index_for(Action::PickerConfirm, &Overlay::None),
                None
            );
        }

        #[test]
        fn confirm_index_for_unrelated_actions_is_none() {
            let overlay = Overlay::ClusterPicker(Picker {
                title: "T".into(),
                items: vec![],
                filter: String::new(),
                selected: 0,
                scroll: 0,
            });
            assert_eq!(confirm_index_for(Action::ClosePicker, &overlay), None);
            assert_eq!(confirm_index_for(Action::Quit, &overlay), None);
        }

        #[tokio::test]
        async fn the_status_shown_comes_from_the_store_the_objects_came_from() {
            use kube::runtime::watcher;
            use kube_tui::store::watch::ResourceStore;
            use tokio::sync::RwLock;

            let gvk = GroupVersionKind::gvk("", "v1", "Pod");
            let ar = ApiResource::erase::<Pod>(&());

            // The cluster the user is on: three pods, watch synced.
            let live: kube_tui::store::watch::SharedStore =
                Arc::new(RwLock::new(ResourceStore::new()));
            {
                let mut s = live.write().await;
                s.set_status(gvk.clone(), WatchStatus::Synced);
                for i in 0..3 {
                    s.apply(
                        &gvk,
                        &ar,
                        watcher::Event::Apply(
                            DynamicObject::new(&format!("pod-{i}"), &ar).within("default"),
                        ),
                    );
                }
            }
            let (objects, status) = store_snapshot(&live, &gvk).await;
            assert_eq!(objects.len(), 3);
            assert_eq!(
                status,
                WatchStatus::Synced,
                "a live watch's own store must report live"
            );

            // What a cluster switch does: replace the store wholesale, so the
            // new cluster starts empty with nothing yet synced. Health read
            // from anywhere ELSE — a local carried across the switch, fed by
            // the previous cluster's WatchStatus events — would still say
            // "live" here, labelling an empty table as fresh data.
            let fresh: kube_tui::store::watch::SharedStore =
                Arc::new(RwLock::new(ResourceStore::new()));
            let (objects, status) = store_snapshot(&fresh, &gvk).await;
            assert!(objects.is_empty(), "the new cluster starts with no objects");
            assert_eq!(
                status,
                WatchStatus::Initialising,
                "zero items must never be labelled 'live': the status must come \
                 from the same store the (empty) object list did"
            );
        }

        #[test]
        fn a_click_on_a_scrolled_picker_row_resolves_to_the_cluster_drawn_there() {
            // End to end for the Critical: render a 20-cluster picker scrolled
            // to the bottom, take the hit zone a click on the LAST visible row
            // would resolve to, and push that index back through the same
            // `resolve_picker_choice` the event loop uses. The whole chain —
            // draw, register, hit-test, map filtered index to item — must name
            // the cluster actually printed on that line.
            //
            // Before the fix nothing was registered below the first
            // screenful at all, so `hits.hit` returned None here and the
            // click did nothing.
            let mut picker = Picker {
                title: "Clusters".into(),
                items: (0..20)
                    .map(|i| PickerItem {
                        label: format!("cluster-{i:02}"),
                        detail: String::new(),
                        accent: None,
                    })
                    .collect(),
                filter: String::new(),
                selected: 19,
                scroll: 0,
            };

            let mut hits = HitRegistry::new();
            let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
            term.draw(|f| {
                kube_tui::ui::views::picker::render_picker(f, f.area(), &mut picker, &mut hits);
            })
            .unwrap();

            // The last list row of a 14-line viewport (two borders, one
            // filter line) is y=12 — verified against a real buffer dump.
            let buf = term.backend().buffer();
            let drawn: String = (0..60u16).map(|x| buf[(x, 12)].symbol()).collect();
            assert!(
                drawn.contains("cluster-19"),
                "expected cluster-19 drawn on the last list row; got: {drawn}"
            );

            let Some(HitTarget::PickerRow(i)) = hits.hit(5, 12) else {
                panic!("a click on the last visible row must land on a picker row");
            };
            assert_eq!(
                resolve_picker_choice(&picker, *i),
                Some("cluster-19".to_string()),
                "clicking the row showing cluster-19 must connect to cluster-19, \
                 not to whatever shares that screen position in the unscrolled list"
            );
        }

        #[test]
        fn render_frame_paints_the_ribbon_in_the_active_clusters_hue() {
            let pods = vec![pod_in("a", "default")];
            let gvk = GroupVersionKind::gvk("", "v1", "Pod");
            let mut view = TableView::new();
            let mut hits = HitRegistry::new();
            let mut overlay = Overlay::None;

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
                    &mut overlay,
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
            let mut overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items: vec![PickerItem {
                    label: "prod".into(),
                    detail: String::new(),
                    accent: None,
                }],
                filter: String::new(),
                selected: 0,
                scroll: 0,
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
                    &mut overlay,
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
