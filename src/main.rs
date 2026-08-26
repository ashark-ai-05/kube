use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, ApiResource, DynamicObject, GroupVersionKind};
use kube::{Client, ResourceExt};
use kube_tui::app::Overlay;
use kube_tui::app::event::{AppEvent, Coalesced, FetchedEvents, WatchStatus, coalesce};
use kube_tui::app::input::{Action, Focus, action_for, apply_selection};
use kube_tui::app::session::{
    Session, SessionEvent, SharedSession, is_deliberate_abort, restart_watch, switch_cluster,
};
use kube_tui::cli::{CliOutcome, NamespaceScope, parse_args, should_hint_all_namespaces};
use kube_tui::cluster;
use kube_tui::cluster::discovery::{KindInfo, discover_kinds, group_label_for};
use kube_tui::cluster::{
    AuthMethod, ClusterEntry, ClusterId, ClusterRegistry, ConnectionState, NamespaceListError,
    is_valid_namespace_name, list_namespaces,
};
use kube_tui::store::columns::columns_for;
use kube_tui::store::events::{EventRow, fetch_events};
use kube_tui::store::multi::{
    DEFAULT_MAX_EAGER_WATCHES, KindAvailability, kinds_to_watch, prioritise,
};
use kube_tui::store::table::{
    SortState, TABLE_REFETCH_DEBOUNCE, TableData, fetch_table, refetch_is_due, row_identity,
    sort_table_rows, sorted_indices,
};
use kube_tui::store::watch::spawn_watch;
use kube_tui::terminal::{RealTerminal, TerminalGuard, install_panic_hook};
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::ribbon::{render_ribbon, split_ribbon};
use kube_tui::ui::theme;
use kube_tui::ui::tree::{KindTree, TreeGroup, TreeKind, TreeRow, flatten};
use kube_tui::ui::views::detail::{DetailPane, DetailTab, render_detail};
use kube_tui::ui::views::picker::{
    Picker, PickerItem, centered, clamp_selection, filtered_indices, render_picker,
};
use kube_tui::ui::views::sidebar::render_sidebar;
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{TableView, render_table_with_data};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    api: Option<&'a [String]>,
    loaded: impl Iterator<Item = &'a str>,
    current: Option<&'a str>,
) -> Vec<String> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    if let Some(api) = api {
        names.extend(api.iter().map(String::as_str));
    }
    names.extend(loaded);
    if let Some(c) = current {
        names.insert(c);
    }
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

    let (api_list, forbidden_note): (Option<&[String]>, Option<String>) = match api_namespaces {
        Some(Ok(list)) => (Some(list.as_slice()), None),
        Some(Err(e)) => (None, Some(e.explanation())),
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
    let Some(i) = index else {
        return PickerOutcome::NoOp;
    };
    match overlay {
        Overlay::None => PickerOutcome::NoOp,
        Overlay::ClusterPicker(p) => match resolve_picker_choice(p, i) {
            Some(label) => PickerOutcome::ClusterChosen(label),
            None => PickerOutcome::NoOp,
        },
        Overlay::NamespacePicker(p) => match resolve_picker_choice(p, i) {
            Some(label) => PickerOutcome::NamespaceChosen(namespace_choice_from_label(&label)),
            None => {
                let typed = p.filter.trim();
                if typed.is_empty() {
                    PickerOutcome::NoOp
                } else if is_valid_namespace_name(typed) {
                    PickerOutcome::NamespaceChosen(Some(typed.to_string()))
                } else {
                    PickerOutcome::InvalidNamespaceTyped(typed.to_string())
                }
            }
        },
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
/// **This is a legibility bound, not a security one.** An earlier version of
/// this comment claimed the cap was what kept a credential plugin's stdout off
/// the screen, on the basis that `std::process::Output`'s `Debug` renders it as
/// a decimal byte array. That has been false since Rust 1.66, which added a
/// manual `Debug` printing stdout and stderr as strings whenever they are valid
/// UTF-8 — so a plugin that prints an `ExecCredential` and exits non-zero puts a
/// readable bearer token in the message, behind a ~100-character prefix that a
/// 200-character cap comfortably clears. Redaction is done by TYPE instead, in
/// `cluster::redact`, and every path that formats an error goes through it
/// FIRST; this function then bounds whatever survives.
///
/// What it is still for: server-supplied text (a `Status.message` quoting a
/// whole admission-webhook rejection) is unbounded and not ours, and one status
/// row is one row.
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

/// How wide the kind tree is, in columns. Wide enough for `PersistentVolume`
/// plus a count and the two-cell indent `render_sidebar` draws kinds with.
const SIDEBAR_WIDTH: u16 = 28;

/// How close together two clicks on the same row must be to count as a
/// double-click. Crossterm reports only individual button presses — there is
/// no double-click event to subscribe to — so this is measured here.
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

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
///
/// The sidebar's per-kind counts and availability come from this same read,
/// for the same reason and one more: availability is a fact the store already
/// records (`ResourceStore::availability`, written by the watch that failed),
/// so deriving it here — by matching on an error string, or by mapping
/// `WatchStatus::Failed` back onto a reason — would be a second answer to a
/// question that already has one, and the two would disagree for the kinds
/// the cap never watched at all.
struct Snapshot {
    objects: Vec<Arc<DynamicObject>>,
    status: WatchStatus,
    /// Server-rendered columns for the active kind, if a fetch has landed.
    table: Option<TableData>,
    /// Count and availability for each kind in the session's `kinds`, in the
    /// same order — parallel to that slice, so the sidebar never has to key
    /// back into a map that might be missing an entry.
    kind_facts: Vec<(usize, KindAvailability)>,
    /// Refetch bookkeeping for the ACTIVE kind only: nothing else is fetched.
    last_change: Option<Instant>,
    last_table_fetch: Option<Instant>,
}

async fn store_snapshot(
    store: &kube_tui::store::watch::SharedStore,
    gvk: &GroupVersionKind,
    kinds: &[KindInfo],
) -> Snapshot {
    let s = store.read().await;
    Snapshot {
        objects: s.objects(gvk),
        status: s.status(gvk),
        table: s.table_data(gvk),
        kind_facts: kinds
            .iter()
            .map(|k| (s.count(&k.gvk), s.availability(&k.gvk)))
            .collect(),
        last_change: s.last_change(gvk),
        last_table_fetch: s.last_table_fetch(gvk),
    }
}

/// What the sidebar's selection points at, in terms that survive a rebuild.
enum TreeAnchor {
    Group(String),
    Kind(GroupVersionKind),
}

fn selection_anchor(tree: &KindTree) -> Option<TreeAnchor> {
    match flatten(tree).get(tree.selected)? {
        TreeRow::Group { group, .. } => Some(TreeAnchor::Group(group.label.clone())),
        TreeRow::Kind { kind, .. } => Some(TreeAnchor::Kind(kind.gvk.clone())),
    }
}

/// Point `selected` back at whatever `anchor` names, or clamp if it is gone.
fn restore_selection(tree: &mut KindTree, anchor: Option<TreeAnchor>) {
    if let Some(anchor) = anchor {
        let found = flatten(tree).iter().position(|row| match (row, &anchor) {
            (TreeRow::Group { group, .. }, TreeAnchor::Group(label)) => group.label == *label,
            (TreeRow::Kind { kind, .. }, TreeAnchor::Kind(gvk)) => kind.gvk == *gvk,
            _ => false,
        });
        if let Some(i) = found {
            tree.selected = i;
        }
    }
    tree.clamp_selected();
}

/// Rebuild the sidebar tree from the session's kinds and this frame's store
/// facts, keeping whatever the user has done to it.
///
/// The tree is derived data — kinds come from discovery, counts and
/// availability from the store — but expansion state and the selection are
/// the user's, and there is nowhere else for them to live. So this rebuilds
/// the rows and carries those two forward:
///
/// - **Expansion** is carried by group LABEL. A group the user collapsed
///   stays collapsed when a count changes underneath it. A group that did not
///   exist before starts collapsed, except the one holding the active kind:
///   opening onto twenty collapsed groups with no sign of where the table's
///   own kind lives is worse than useless.
/// - **The selection** is carried by IDENTITY, not by row number. Row numbers
///   are meaningless across a rebuild — a group above the selection gaining
///   one kind moves every row below it — so the selected group label or GVK
///   is looked up in the new rows and the index that now points at it is
///   restored. Only when the thing selected is gone entirely does this fall
///   back to `clamp_selected`.
fn refresh_tree(
    tree: &mut KindTree,
    kinds: &[KindInfo],
    facts: &[(usize, KindAvailability)],
    active: &GroupVersionKind,
) {
    let anchor = selection_anchor(tree);
    let was_expanded: std::collections::HashMap<String, bool> = tree
        .groups
        .iter()
        .map(|g| (g.label.clone(), g.expanded))
        .collect();

    let mut groups: Vec<TreeGroup> = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        // A missing fact is "nothing known yet", not a panic: `facts` is
        // built from the same `kinds` slice one lock acquisition earlier, so
        // this cannot happen in the event loop, but a shorter slice must not
        // take the whole frame down.
        let (count, availability) = facts
            .get(i)
            .cloned()
            .unwrap_or((0, KindAvailability::Watching));
        let row = TreeKind {
            gvk: kind.gvk.clone(),
            label: kind.gvk.kind.clone(),
            count: Some(count),
            availability,
        };
        match groups.iter_mut().find(|g| g.label == kind.group_label) {
            Some(group) => group.kinds.push(row),
            None => groups.push(TreeGroup {
                label: kind.group_label.clone(),
                // A group that already existed keeps whatever the user did to
                // it; a brand-new one starts shut unless it holds the kind
                // the table is showing (applied below, once its kinds are
                // known).
                expanded: was_expanded
                    .get(&kind.group_label)
                    .copied()
                    .unwrap_or(false),
                kinds: vec![row],
            }),
        }
    }

    for group in &mut groups {
        if !was_expanded.contains_key(&group.label) && group.kinds.iter().any(|k| k.gvk == *active)
        {
            group.expanded = true;
        }
    }
    // Sorted here as well as by `discovery::sort_kinds` upstream: group order
    // is what the sidebar's stability across restarts depends on, and making
    // it a property of this function rather than of its caller's input means
    // a caller that hands over unsorted kinds still gets a stable sidebar.
    groups.sort_by(|a, b| a.label.cmp(&b.label));

    tree.groups = groups;
    restore_selection(tree, anchor);
}

/// Resolve the table's selected row to the object it is showing.
///
/// Never `objects[selected]`. The table on screen is one of two things, and
/// neither is the live object list in the order the store happens to hold it:
///
/// - **Server-rendered columns.** The `TableData` came from a point-in-time
///   `fetch_table`; the object list is continuously updated by the watch. The
///   two are not guaranteed to agree on order or even count at any instant,
///   so the row's OWN identity (`row_identity`, from the
///   `includeObject=Metadata` metadata the fetch asked for) is the only
///   correct answer.
/// - **Builtin columns**, before the first fetch lands or after one failed.
///   Here the rows really are extracted from `objects` in order — but only
///   until a column header is clicked, after which `render_table_with_data`
///   sorts them. `sorted_indices` reproduces exactly that ordering so the
///   row the user clicked is the object they get.
///
/// Returns `(namespace, name)` — the identity, not the object, because the
/// object must be re-resolved against the live list on every frame rather
/// than held as a snapshot that stops updating.
fn selected_object(
    objects: &[Arc<DynamicObject>],
    gvk: &GroupVersionKind,
    table: Option<&TableData>,
    sort: Option<&SortState>,
    selected: usize,
) -> Option<(Option<String>, String)> {
    match table {
        Some(t) => match sort {
            Some(sort) => {
                // `sort_table_rows` moves each row's identity with its cells
                // (that is why `TableRow` bundles them), so the identity read
                // out at `selected` is the one belonging to the row drawn
                // there.
                let mut rows = t.rows.clone();
                sort_table_rows(&mut rows, sort);
                row_identity(
                    &TableData {
                        columns: t.columns.clone(),
                        rows,
                    },
                    selected,
                )
            }
            None => row_identity(t, selected),
        },
        None => {
            let index = match sort {
                Some(sort) => {
                    let columns = columns_for(gvk);
                    let cells: Vec<Vec<String>> = objects
                        .iter()
                        .map(|obj| columns.iter().map(|c| (c.extract)(obj)).collect())
                        .collect();
                    *sorted_indices(&cells, sort).get(selected)?
                }
                None => selected,
            };
            let obj = objects.get(index)?;
            Some((obj.metadata.namespace.clone(), obj.name_any()))
        }
    }
}

/// The live object matching an identity, or `None` if it has left the store.
fn find_object<'a>(
    objects: &'a [Arc<DynamicObject>],
    namespace: Option<&str>,
    name: &str,
) -> Option<&'a Arc<DynamicObject>> {
    objects
        .iter()
        .find(|o| o.metadata.namespace.as_deref() == namespace && o.name_any() == name)
}

/// Whether a batch's store changes should force the table body to redraw.
///
/// `coalesce` only tracks WHICH kinds changed in a batch — it has no notion
/// of `active_kind`, since that lives in `Session`, not in an `AppEvent`.
/// This is the one place that comparison happens: a batch full of
/// `coordination.k8s.io/Lease`, `core/Event` or `discovery.k8s.io/
/// EndpointSlice` deltas (all of which churn continuously on a stock
/// cluster, none of which the table is showing) must not force a redraw
/// whose output is pixel-identical to the last frame — the cost Plan 3's
/// eager, uncapped-by-kind watching of up to 40 kinds would otherwise pay
/// on every idle tick.
fn table_body_is_dirty(
    changed: &HashSet<GroupVersionKind>,
    active_kind: &GroupVersionKind,
) -> bool {
    changed.contains(active_kind)
}

/// Whether the sidebar's per-kind counts actually changed since the last
/// frame that read them.
///
/// A kind's watch firing does not mean its OBJECT COUNT changed: a Lease
/// renewal, an EndpointSlice reshuffling backends, or a status-only Pod
/// update are all in-place changes to objects that already existed, and
/// recomputing counts after one produces the exact numbers already on
/// screen. Comparing the settled result of a batch against what was last
/// drawn — rather than redrawing on every delta that produced it — is the
/// "coarser condition" that lets the sidebar's counts stay live without
/// costing a frame per Lease heartbeat. Deliberately not a timer: this only
/// runs when a batch of real events already woke the loop.
fn counts_changed(
    kinds: &[KindInfo],
    facts: &[(usize, KindAvailability)],
    last_counts: &HashMap<GroupVersionKind, usize>,
) -> bool {
    kinds
        .iter()
        .zip(facts)
        .any(|(k, (count, _))| last_counts.get(&k.gvk) != Some(count))
}

/// Snapshot `facts` into the map `counts_changed` compares the NEXT batch
/// against.
fn record_counts(
    kinds: &[KindInfo],
    facts: &[(usize, KindAvailability)],
) -> HashMap<GroupVersionKind, usize> {
    kinds
        .iter()
        .zip(facts)
        .map(|(k, (count, _))| (k.gvk.clone(), *count))
        .collect()
}

/// Whether to issue a Table fetch for the active kind now.
///
/// Delegates the real decision to `store::table::refetch_is_due`, which is
/// where "stale AND settled" is defined; this only covers the one case that
/// function has no opinion on — a kind whose watch has never delivered
/// anything, so there is no change to debounce against. That is not an idle
/// kind to leave alone: it is a kind the user just selected, or one that is
/// genuinely empty, and either way its columns have never been fetched.
fn table_fetch_due(
    last_fetch: Option<Instant>,
    last_change: Option<Instant>,
    now: Instant,
    debounce: Duration,
) -> bool {
    match last_change {
        Some(changed) => refetch_is_due(last_fetch, changed, now, debounce),
        None => last_fetch.is_none(),
    }
}

/// Whether this click on `row` completes a double-click.
///
/// Crossterm reports button presses, not clicks — there is no double-click
/// event — so it is measured here. The row must match as well as the timing:
/// two fast clicks on two different rows are two selections, not an open.
fn is_double_click(previous: Option<(usize, Instant)>, row: usize, now: Instant) -> bool {
    match previous {
        Some((previous_row, at)) => {
            previous_row == row && now.saturating_duration_since(at) <= DOUBLE_CLICK_WINDOW
        }
        None => false,
    }
}

/// The detail pane's subject, and everything fetched FOR that subject.
///
/// Events live here, beside the identity they belong to, rather than in a
/// `Vec<EventRow>` of their own next to an `Option<DetailPane>`. That is what
/// makes them unable to go stale: a fetch reply carries the identity it was
/// issued for (`app::event::FetchedEvents`) and is applied only if it matches
/// the identity in THIS struct, so a reply that arrives after the user has
/// moved the pane to another object — or closed it — is dropped rather than
/// displayed. Two parallel fields could be written independently; one struct
/// cannot be.
///
/// The object itself is deliberately NOT stored: it is re-resolved from the
/// live object list every frame (`find_object`), so the YAML and Overview
/// tabs track the object as it changes rather than freezing at open time.
struct OpenDetail {
    gvk: GroupVersionKind,
    namespace: Option<String>,
    name: String,
    events: Vec<EventRow>,
    events_error: Option<String>,
}

impl OpenDetail {
    /// Whether a fetch reply belongs to the object this pane is open on.
    ///
    /// All three parts are compared. A name alone is not an identity: the
    /// same name exists in every namespace, and a Deployment and its Pods
    /// routinely share one.
    fn is_for(&self, gvk: &GroupVersionKind, namespace: Option<&str>, name: &str) -> bool {
        self.gvk == *gvk && self.namespace.as_deref() == namespace && self.name == name
    }
}

/// Everything a frame draws that is read-only for the duration of the draw.
///
/// A struct rather than more positional parameters: `render_frame` reads a
/// dozen unrelated values and four mutable views, and at that width a
/// transposed pair of `&str`s or `bool`s compiles silently.
struct FrameArgs<'a> {
    objects: &'a [Arc<DynamicObject>],
    gvk: &'a GroupVersionKind,
    table_data: Option<TableData>,
    context_name: &'a str,
    display_namespace: &'a str,
    status: WatchStatus,
    last_error: Option<&'a str>,
    show_hint: bool,
    connecting: Option<&'a str>,
    /// `Some` opens the detail pane over the table. The object is passed in
    /// rather than looked up here so the draw closure stays synchronous and
    /// does no work it could get wrong.
    detail_object: Option<&'a DynamicObject>,
    events: &'a [EventRow],
    events_error: Option<&'a str>,
}

/// Draw one frame: ribbon, sidebar, table, status bar, then the detail pane
/// over the table, then any picker over everything.
///
/// The order is the z-order. Each overlay's hit zones (`render_detail` at
/// z=1, `render_picker` at z=1) resolve above the table's z=0 zones wherever
/// they overlap, and each is drawn behind a `Clear` so its own content paints
/// over whatever the layer below left there. The picker is last because it is
/// modal over the detail pane too — a cluster switch is reachable from
/// anywhere.
#[allow(clippy::too_many_arguments)]
fn render_frame(
    f: &mut Frame,
    args: FrameArgs<'_>,
    view: &mut TableView,
    tree: &mut KindTree,
    // Mutable because each of these owns a scroll offset it advances during
    // the draw — see `render_picker`, `render_sidebar`, `render_detail`.
    pane: &mut DetailPane,
    overlay: &mut Overlay,
    hits: &mut HitRegistry,
) {
    let full = f.area();
    let (ribbon_area, rest) = split_ribbon(full);
    render_ribbon(f, ribbon_area, Some(args.context_name), hits);

    let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).split(rest);
    // The sidebar takes a fixed width and the table whatever is left. On a
    // terminal too narrow to hold both, `Layout` shrinks the sidebar rather
    // than overlapping them, and every view here already guards a zero-sized
    // area.
    let body = Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)])
        .split(chunks[0]);
    render_sidebar(f, body[0], tree, hits);
    render_table_with_data(
        f,
        body[1],
        args.objects,
        args.gvk,
        args.table_data,
        view,
        hits,
    );
    render_status(
        f,
        chunks[1],
        args.context_name,
        args.display_namespace,
        args.status,
        args.objects.len(),
        args.last_error,
        args.show_hint,
        args.connecting,
        hits,
    );

    // Over the table's rect exactly, not the whole frame: the sidebar and the
    // ribbon stay visible and usable with the pane open, so switching kind or
    // cluster does not require closing it first.
    if let Some(obj) = args.detail_object {
        render_detail(f, body[1], obj, pane, hits, args.events, args.events_error);
    }

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

/// Watch a set of sibling tasks, reporting the FIRST one that fails at the
/// moment it fails.
///
/// The `supervise` above does this for a single task; this is its plural, and
/// the difference is the whole point. `futures::future::join_all` — what this
/// used to be — yields nothing at all until EVERY future it holds has
/// completed, and a watch task loops forever by design. With forty of them,
/// one panicking child was therefore never observed: reporting it had to wait
/// for the other thirty-nine to stop too, which on a healthy cluster never
/// happens. The dead kind's `WatchStatus` stayed `Synced` from its last delta,
/// so the status bar went on reading `live` over a table nothing was updating
/// — precisely the state this supervision exists to make impossible.
///
/// `FuturesUnordered` yields each result as that child finishes, so a panic is
/// visible immediately whatever its siblings are doing. It does not change
/// cancellation: the caller holds the `AbortOnDrop` guards, and dropping this
/// future drops them with it.
async fn supervise_children(
    children: Vec<tokio::task::JoinHandle<()>>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    let mut running: futures::stream::FuturesUnordered<_> = children.into_iter().collect();
    while let Some(result) = running.next().await {
        match result {
            Ok(()) => {}
            // Same reasoning as `supervise`: a cluster switch aborts the
            // outgoing cluster's watches, and that is a teardown, not a crash.
            Err(e) if is_deliberate_abort(&e) => {}
            Err(e) => {
                let _ = tx.send(AppEvent::Error(join_failure_detail("watch", e)));
                let _ = tx.send(AppEvent::Quit);
            }
        }
    }
}

/// The kind list to fall back on when discovery itself fails.
///
/// A cluster we cannot enumerate is still a cluster we can watch pods on —
/// `core/v1 Pod` needs no discovery to address. Degrading to Plan 2's
/// behaviour beats showing an empty sidebar over an empty table with only a
/// status-bar line to explain it.
fn pod_kind() -> KindInfo {
    let resource = ApiResource::erase::<Pod>(&());
    KindInfo {
        gvk: GroupVersionKind::gvk("", "v1", "Pod"),
        namespaced: true,
        group_label: group_label_for(&resource.group),
        resource,
    }
}

/// Discover what this cluster can browse, record it on the session, and watch
/// every kind that fits under the cap — all under ONE `JoinHandle`.
///
/// One handle is what lets `switch_cluster`/`restart_watch` keep their
/// existing contract (`FnOnce(Client, SharedStore, Option<String>) ->
/// JoinHandle<()>`) while forty watches run beneath it. The task holds an
/// `AbortOnDrop` per child across its own await, so aborting it — which is
/// all `WatchHandles::abort_all` does — cancels the discovery if it is still
/// in flight and every watch it has started, rather than detaching them.
///
/// **Staleness.** Discovery is slow enough that a cluster switch or a
/// namespace change can complete while it is running, and its answer would
/// then belong to a cluster nobody is looking at. The guard is the store:
/// both of those operations replace it wholesale, and this task was handed
/// the store belonging to the generation that started it, so
/// `Arc::ptr_eq(&s.store, &store)` IS the identity check — no second counter
/// to keep in step with the one `Session::generation` already maintains for
/// connects.
fn spawn_discovery_and_watches(
    session: SharedSession,
    client: Client,
    store: kube_tui::store::watch::SharedStore,
    namespace: Option<String>,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let kinds: Vec<KindInfo> = match discover_kinds(&client).await {
            Ok(kinds) if !kinds.is_empty() => kinds,
            Ok(_) => vec![pod_kind()],
            Err(e) => {
                let _ = tx.send(AppEvent::Error(truncate_error(format!(
                    "discovering kinds: {} — showing pods only",
                    cluster::safe_error_text(&e)
                ))));
                vec![pod_kind()]
            }
        };

        // Which kinds survive the cap is a question of importance; which
        // order they are DRAWN in is a question of stability. `prioritise`
        // answers the first, on a copy, so `kinds` keeps `sort_kinds`' stable
        // group-then-kind order for the second. Ranking the list the sidebar
        // draws would reorder it by importance and scatter each group's kinds
        // across the tree.
        let mut ranked = kinds.clone();
        prioritise(&mut ranked);
        let (watched, skipped) = kinds_to_watch(&ranked, DEFAULT_MAX_EAGER_WATCHES);
        let watched_gvks: HashSet<GroupVersionKind> =
            watched.iter().map(|k| k.gvk.clone()).collect();
        let resources: Vec<ApiResource> = watched.iter().map(|k| k.resource.clone()).collect();

        {
            let mut s = session.lock().await;
            if !Arc::ptr_eq(&s.store, &store) {
                // Superseded: this answer describes a cluster (or a scope)
                // that is no longer on screen.
                return;
            }
            s.kinds = kinds.clone();
        }

        // A kind the cap left out is not an empty kind, and the sidebar must
        // not draw it as one. Recorded as availability in the store — the one
        // place the sidebar reads per-kind facts from — rather than as a
        // second list that only the sidebar knows about and only a cluster
        // switch would clear.
        {
            let mut s = store.write().await;
            for kind in &kinds {
                if !watched_gvks.contains(&kind.gvk) {
                    s.set_availability(kind.gvk.clone(), KindAvailability::NotWatched);
                }
            }
        }
        if skipped > 0 {
            let _ = tx.send(AppEvent::Error(format!(
                "{skipped} of {} kinds are not being watched (cap {DEFAULT_MAX_EAGER_WATCHES}) \
                 — they show as 'not watched' in the sidebar",
                kinds.len()
            )));
        }
        let _ = tx.send(AppEvent::KindsDiscovered);

        let mut children = Vec::with_capacity(resources.len());
        let mut cancel_children = Vec::with_capacity(resources.len());
        for resource in resources {
            let handle = spawn_watch(
                client.clone(),
                resource,
                namespace.clone(),
                store.clone(),
                tx.clone(),
            );
            cancel_children.push(AbortOnDrop(handle.abort_handle()));
            children.push(handle);
        }

        supervise_children(children, tx).await;
        // Held until here so that cancelling THIS task drops them and cancels
        // the children with it. Dropping a `JoinHandle` only detaches.
        drop(cancel_children);
    })
}

/// Ask the API server for the active kind's own columns, kubectl-style.
///
/// Writes the answer straight into the store it was issued against, keyed by
/// the kind it was issued for, and only then wakes the loop. A reply for a
/// kind the user has since left updates that kind's (unread) entry; a reply
/// for a cluster the user has since left updates a store nobody reads. Both
/// are harmless by construction rather than by timing.
fn spawn_table_fetch(
    client: Client,
    resource: ApiResource,
    namespace: Option<String>,
    gvk: GroupVersionKind,
    store: kube_tui::store::watch::SharedStore,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let api: Api<DynamicObject> = match namespace.as_deref() {
            Some(ns) => Api::namespaced_with(client.clone(), ns, &resource),
            None => Api::all_with(client.clone(), &resource),
        };
        let url = api.resource_url().to_string();
        match fetch_table(&client, &url).await {
            Ok(data) => {
                store.write().await.set_table_data(gvk.clone(), data);
                let _ = tx.send(AppEvent::StoreChanged { gvk });
            }
            Err(e) => {
                // Not fatal: `column_source` falls back to the builtin
                // registry, so the table keeps rendering with NAME/AGE.
                let _ = tx.send(AppEvent::Error(truncate_error(format!(
                    "fetching {} columns: {}",
                    gvk.kind,
                    cluster::safe_error_text(&e)
                ))));
            }
        }
    });
}

/// Fetch one object's events for the detail pane.
///
/// The reply carries the identity it was issued for so the loop can drop it
/// if the pane has moved on — see `app::event::FetchedEvents`.
fn spawn_events_fetch(
    client: Client,
    gvk: GroupVersionKind,
    namespace: Option<String>,
    name: String,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let result = fetch_events(&client, namespace.as_deref().unwrap_or(""), &name)
            .await
            .map_err(|e| truncate_error(cluster::safe_error_text(&e)));
        let _ = tx.send(AppEvent::EventsFetched(FetchedEvents {
            gvk,
            namespace,
            name,
            result,
        }));
    });
}

/// Point the detail pane at whatever the table has selected, and ask for that
/// object's events.
///
/// Does nothing if the selection resolves to no object — an empty table, or a
/// server row with no identity attached. Opening a pane on nothing and
/// showing an empty frame would be worse than not opening at all.
#[allow(clippy::too_many_arguments)]
fn open_detail(
    detail: &mut Option<OpenDetail>,
    pane: &mut DetailPane,
    snapshot: &Snapshot,
    objects: &[Arc<DynamicObject>],
    active_kind: &GroupVersionKind,
    view: &TableView,
    client: &Client,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let Some((namespace, name)) = selected_object(
        objects,
        active_kind,
        snapshot.table.as_ref(),
        view.sort.as_ref(),
        view.selected,
    ) else {
        return;
    };
    // A fresh pane per object: both scroll offsets and the YAML cache belong
    // to the object that was open, not to the pane, and carrying an offset
    // from a 900-line Deployment onto a 20-line ConfigMap opens it scrolled
    // past its own end.
    *pane = DetailPane::new();
    *detail = Some(OpenDetail {
        gvk: active_kind.clone(),
        namespace: namespace.clone(),
        name: name.clone(),
        events: Vec::new(),
        events_error: None,
    });
    spawn_events_fetch(
        client.clone(),
        active_kind.clone(),
        namespace,
        name,
        tx.clone(),
    );
}

/// Re-ask for the open object's events when the Events tab becomes visible.
///
/// Events are a one-shot list rather than a watch (`store::events`' own
/// documented choice), so switching to the tab is the moment to ask again:
/// someone opening it is asking what is happening *now*, and a list fetched
/// when the pane opened may be minutes old by then.
fn refresh_events_if_needed(
    pane: &DetailPane,
    detail: Option<&OpenDetail>,
    client: &Client,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    if pane.tab != DetailTab::Events {
        return;
    }
    let Some(open) = detail else {
        return;
    };
    spawn_events_fetch(
        client.clone(),
        open.gvk.clone(),
        open.namespace.clone(),
        open.name.clone(),
        tx.clone(),
    );
}

/// Move the active tab's scroll offset. Overview fits by construction and has
/// none.
///
/// Only the lower bound is enforced here. The upper one depends on how the
/// content wraps at the pane's current width, which is not known until the
/// draw — so `render_yaml`/`render_events` clamp it there, against the real
/// wrapped height.
fn scroll_detail(pane: &mut DetailPane, delta: i32) {
    let current = match pane.tab {
        DetailTab::Overview => return,
        DetailTab::Yaml => pane.yaml_scroll,
        DetailTab::Events => pane.events_scroll,
    };
    let next = (i32::from(current) + delta).clamp(0, i32::from(u16::MAX)) as u16;
    match pane.tab {
        DetailTab::Overview => {}
        DetailTab::Yaml => pane.yaml_scroll = next,
        DetailTab::Events => pane.events_scroll = next,
    }
}

/// Wake the loop once, after `after`, so a debounced refetch actually
/// happens.
///
/// The watch is the trigger, but a debounce means the moment a fetch becomes
/// due is a moment when, by definition, nothing is arriving to wake us. One
/// sleeper per dirty batch closes that gap without becoming a timer: a
/// `Wake` marks nothing dirty, so it arms no successor and a quiet cluster
/// settles back to zero events and zero CPU.
fn spawn_refetch_wake(tx: mpsc::UnboundedSender<AppEvent>, after: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let _ = tx.send(AppEvent::Wake);
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
            // Through `safe_error_text`, not `{e:#}`. This is the ONE failure
            // path that always reaches the user's real terminal — the
            // alternate screen has not been entered yet — so it is also the
            // one that lands in shell scrollback, `script` captures, CI logs
            // and anything piping stderr. An exec plugin that printed an
            // `ExecCredential` before failing would otherwise put a live
            // bearer token in every one of them. See `cluster::redact`.
            eprintln!(
                "kube: could not connect to a cluster: {}",
                cluster::safe_error_text(&e)
            );
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

    // Discovery, and a watch per discovered kind, all under one handle — see
    // `spawn_discovery_and_watches`. Nothing is watched until discovery
    // answers, so there is no throwaway Pod watch to tear down a moment later
    // and no window in which Pods are watched twice.
    let discovery_handle = spawn_discovery_and_watches(
        session.clone(),
        client.clone(),
        session.lock().await.store.clone(),
        watch_namespace,
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
        .push(supervise("watch", discovery_handle, tx.clone()));

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
    let mut tree = KindTree {
        groups: Vec::new(),
        selected: 0,
        scroll: 0,
    };
    let mut pane = DetailPane::new();
    let mut hits = HitRegistry::new();
    let mut last_error: Option<String> = None;
    // At most one picker is ever open; opening one replaces whatever was
    // open before. Neither cluster nor namespace picking touched a network
    // before this task — this is what makes them reachable.
    let mut overlay = Overlay::None;
    // Which of the two always-visible panes has the keyboard. The modal
    // layers (a picker, the detail pane) override this while they are open
    // rather than being tracked here as well — see `focus` below, which
    // derives the real answer from all three each pass so a picker and a
    // detail pane can never both believe they have it.
    let mut pane_focus = Focus::Table;
    // `Some` only while the detail pane is open, and it owns the identity the
    // pane is open on together with the events fetched for it. See
    // `OpenDetail`.
    let mut detail: Option<OpenDetail> = None;
    // The last table row clicked and when — the only state double-click
    // detection needs (`is_double_click`); crossterm reports presses, not
    // clicks.
    let mut last_click: Option<(usize, Instant)> = None;
    // What `counts_changed` compares the next batch's counts against, so a
    // redraw is forced only when a kind's count actually differs from what
    // is already on screen — not on every delta an in-place update (a Lease
    // renewal, an EndpointSlice reshuffle) produces. See `counts_changed`.
    let mut last_counts: HashMap<GroupVersionKind, usize> = HashMap::new();
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
        //
        // Store changes are NOT unconditionally dirty here — see
        // `table_body_is_dirty`, applied below once `active_kind` is known.
        // Discovery landing changes the whole sidebar.
        needs_redraw |= batch.kinds_discovered;
        // A wake means state read at the TOP of a pass may have changed since
        // the last frame was drawn — most importantly `active_kind`, which a
        // sidebar click writes to the session and then wakes the loop to pick
        // up. Without this the table would keep showing the previous kind
        // until something unrelated happened to force a repaint. Wakes only
        // ever follow real activity, so this costs one extra repaint per
        // settled burst and nothing at all on an idle cluster.
        needs_redraw |= batch.wake;
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
            kinds,
            active_kind,
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
                s.kinds.clone(),
                s.active_kind.clone(),
            )
        };
        let context_name = active_cluster.unwrap_or_else(|| startup_context_name.clone());
        let scope = display_namespace(namespace.as_deref());
        let connecting_name = connecting_cluster_name(&entries);

        // Store changes only redraw the table body when they name the kind
        // actually on screen — see `table_body_is_dirty`.
        needs_redraw |= table_body_is_dirty(&batch.changed_kinds, &active_kind);

        // Errors are raised AND retired here: see `next_error`. Without the
        // retiring half a single blip pins an error across every subsequent
        // switch. Done after the session read because which kind's sync
        // retires an error depends on which kind is on screen — a Deployment
        // watch coming up says nothing about why the Pod watch failed.
        let updated_error = next_error(last_error.clone(), &batch, &active_kind);
        if updated_error != last_error {
            last_error = updated_error;
            needs_redraw = true;
        }

        // Read the store snapshot into locals before drawing; the render
        // closure below must be synchronous and must not acquire any locks.
        // Objects, watch health, the fetched columns AND every kind's count
        // and availability come from one acquisition of one store, so a
        // switch cannot show the previous cluster's health over this
        // cluster's (empty) object list, nor one kind's count beside
        // another's availability.
        let snapshot = store_snapshot(&store, &active_kind, &kinds).await;
        // Borrowed, not cloned: this is the live object list, which on a busy
        // namespace is thousands of `Arc`s, and a clone per event batch — mouse
        // moves included — is thousands of refcount bumps for nothing.
        let objects: &[Arc<DynamicObject>] = &snapshot.objects;
        let status = snapshot.status;

        // The tree is rebuilt from that same snapshot every pass — counts
        // change constantly — carrying the user's expansion state and
        // selection across each rebuild. See `refresh_tree`.
        refresh_tree(&mut tree, &kinds, &snapshot.kind_facts, &active_kind);

        // The sidebar's counts redraw only when one of them actually
        // changed value, not on every in-place update (a Lease renewal, an
        // EndpointSlice reshuffle) that left every count exactly as it was
        // — see `counts_changed`. Computed and recorded every pass,
        // regardless of `needs_redraw`, so the comparison is always against
        // what was truly last observed rather than what was last DRAWN.
        needs_redraw |= counts_changed(&kinds, &snapshot.kind_facts, &last_counts);
        last_counts = record_counts(&kinds, &snapshot.kind_facts);

        // Apply any events that came back, but only to the object they were
        // fetched for. A reply for an object the pane has moved off is
        // dropped here rather than shown under this one's name.
        for fetched in &batch.events_fetched {
            if let Some(open) = detail.as_mut()
                && open.is_for(&fetched.gvk, fetched.namespace.as_deref(), &fetched.name)
            {
                match &fetched.result {
                    Ok(rows) => {
                        open.events = rows.clone();
                        open.events_error = None;
                    }
                    Err(e) => {
                        open.events.clear();
                        open.events_error = Some(e.clone());
                    }
                }
                needs_redraw = true;
            }
        }

        // The active kind's columns: fetched when the watch says something
        // changed and the change has settled (`refetch_is_due`, via
        // `table_fetch_due`), never on a timer. When a change has NOT settled
        // yet, one sleeper is armed for the moment it will have — the one
        // moment nothing else would wake us. See `spawn_refetch_wake`.
        let now = Instant::now();
        if table_fetch_due(
            snapshot.last_table_fetch,
            snapshot.last_change,
            now,
            TABLE_REFETCH_DEBOUNCE,
        ) {
            if let Some(kind) = kinds.iter().find(|k| k.gvk == active_kind) {
                // Recorded before the request goes out, not after it returns:
                // see `ResourceStore::note_table_fetch`.
                store
                    .write()
                    .await
                    .note_table_fetch(active_kind.clone(), now);
                spawn_table_fetch(
                    client.clone(),
                    kind.resource.clone(),
                    namespace.clone(),
                    active_kind.clone(),
                    store.clone(),
                    tx.clone(),
                );
            }
        } else if let Some(changed) = snapshot.last_change
            && snapshot
                .last_table_fetch
                .map(|f| f < changed)
                .unwrap_or(true)
        {
            // Stale, but the change has not settled yet — so a fetch WILL
            // become due, at a moment when by definition nothing is arriving
            // to wake us for it. Armed on this condition alone rather than on
            // "the store changed in this batch": selecting a kind whose watch
            // happened to deliver something a moment ago lands here with
            // nothing dirty, and gating on dirtiness would leave that kind on
            // the built-in NAME/AGE fallback until its watch next moved.
            //
            // This cannot become a timer. A `Wake` marks nothing dirty and
            // does not move `last_change`, so by the time one arrives the
            // debounce has elapsed and the branch above fires instead; the
            // fetch it issues records `last_table_fetch`, after which nothing
            // here is stale and no successor is armed.
            let settles_in =
                TABLE_REFETCH_DEBOUNCE.saturating_sub(now.saturating_duration_since(changed));
            // A hair past the debounce so the wake cannot land in the same
            // millisecond and find itself one tick short.
            spawn_refetch_wake(tx.clone(), settles_in + Duration::from_millis(10));
        }

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
                    objects,
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
            // Focus is derived, not tracked: the modal layers win over
            // whichever always-visible pane the keyboard was last left on, in
            // the same order they are drawn in.
            let focus = if overlay.is_open() {
                Focus::Picker
            } else if detail.is_some() {
                Focus::Detail
            } else {
                pane_focus
            };
            let action = action_for(input, &hits, focus);
            let confirm_index = confirm_index_for(action, &overlay);
            // How many rows the table actually has: the server's, when it
            // rendered them, and the object list's otherwise — the same
            // choice `render_table_with_data` makes, so a selection can never
            // be clamped against a different length than it is drawn against.
            let table_rows = snapshot
                .table
                .as_ref()
                .map(|t| t.rows.len())
                .unwrap_or(objects.len());
            match action {
                Action::Quit => quit = true,
                Action::SelectRow(i) => {
                    view.selected = i.min(table_rows.saturating_sub(1));
                    let clicked_at = Instant::now();
                    if is_double_click(last_click, i, clicked_at) {
                        open_detail(
                            &mut detail,
                            &mut pane,
                            &snapshot,
                            objects,
                            &active_kind,
                            &view,
                            &client,
                            &tx,
                        );
                        // Cleared so a third click is a fresh first click
                        // rather than re-opening the pane.
                        last_click = None;
                    } else {
                        last_click = Some((i, clicked_at));
                    }
                    pane_focus = Focus::Table;
                    needs_redraw = true;
                }
                Action::ScrollBy(d) => {
                    match &mut overlay {
                        Overlay::None => {
                            view.selected = apply_selection(view.selected, d, table_rows);
                        }
                        Overlay::ClusterPicker(p) | Overlay::NamespacePicker(p) => {
                            let n = filtered_indices(&p.items, &p.filter).len();
                            p.selected = apply_selection(p.selected, d, n);
                        }
                    }
                    needs_redraw = true;
                }
                Action::SortByColumn(i) => {
                    // Previously only armed a redraw, which drew the same
                    // order again: `toggle_sort` is what `render_table_with_data`
                    // reads.
                    view.toggle_sort(i);
                    needs_redraw = true;
                }
                Action::ToggleFocus => {
                    pane_focus = match pane_focus {
                        Focus::Sidebar => Focus::Table,
                        _ => Focus::Sidebar,
                    };
                    needs_redraw = true;
                }
                Action::ScrollTree(d) => {
                    tree.selected = apply_selection(tree.selected, d, flatten(&tree).len());
                    needs_redraw = true;
                }
                // A click and Enter do the same thing to a sidebar row —
                // expand a group, or make a kind active — so one arm serves
                // both and they cannot drift apart.
                Action::SelectTreeRow(_) | Action::ActivateTreeRow => {
                    if let Action::SelectTreeRow(i) = action {
                        tree.selected = i;
                        pane_focus = Focus::Sidebar;
                    }
                    match tree.selected_kind().map(|k| k.gvk.clone()) {
                        None => {
                            let row = tree.selected;
                            tree.toggle(row);
                        }
                        Some(gvk) if gvk != active_kind => {
                            session.lock().await.active_kind = gvk;
                            // The table is about to show a different kind
                            // with different columns, so nothing about the
                            // old one survives: not the selection, not the
                            // sort (column 3 of Pods is not column 3 of
                            // ConfigMaps), and not a detail pane opened on an
                            // object of the previous kind.
                            view.selected = 0;
                            view.offset = 0;
                            view.sort = None;
                            detail = None;
                            // The new kind's columns have never been fetched,
                            // and its watch may be perfectly quiet — so
                            // nothing else would wake this loop to notice.
                            let _ = tx.send(AppEvent::Wake);
                        }
                        Some(_) => {}
                    }
                    needs_redraw = true;
                }
                Action::OpenDetail => {
                    open_detail(
                        &mut detail,
                        &mut pane,
                        &snapshot,
                        objects,
                        &active_kind,
                        &view,
                        &client,
                        &tx,
                    );
                    needs_redraw = true;
                }
                Action::CloseDetail => {
                    detail = None;
                    needs_redraw = true;
                }
                Action::SelectDetailTab(i) => {
                    if let Some(tab) = DetailTab::at(i) {
                        pane.tab = tab;
                        refresh_events_if_needed(&pane, detail.as_ref(), &client, &tx);
                    }
                    needs_redraw = true;
                }
                Action::CycleDetailTab(d) => {
                    pane.tab = pane.tab.cycled(d);
                    refresh_events_if_needed(&pane, detail.as_ref(), &client, &tx);
                    needs_redraw = true;
                }
                Action::ScrollDetail(d) => {
                    scroll_detail(&mut pane, d);
                    needs_redraw = true;
                }
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
                            objects,
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
                            let session3 = session.clone();
                            let tx2 = tx.clone();
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
                                            // `safe_error_text`, not `{e:#}`:
                                            // this text becomes
                                            // `SessionEvent::ConnectFailed`'s
                                            // reason and is drawn in the status
                                            // bar. Switching to a cluster whose
                                            // plugin fails after printing an
                                            // ExecCredential is exactly the
                                            // shape that leaks a token there.
                                            // The plugin's NAME still reaches
                                            // the user, from the kubeconfig via
                                            // `connect_failure_hint` — never
                                            // from the error, whose own `cmd`
                                            // field carries the process
                                            // environment. See `cluster::redact`.
                                            anyhow::anyhow!(connect_failure_hint(
                                                &target_auth,
                                                &cluster::safe_error_text(&e)
                                            ))
                                        })
                                    },
                                    // The new cluster's kinds are its own:
                                    // discovery runs against the client this
                                    // switch just obtained, into the store it
                                    // just minted, and stands down if either
                                    // is superseded before it answers. See
                                    // `spawn_discovery_and_watches`.
                                    move |client, store, ns| {
                                        supervise(
                                            "watch",
                                            spawn_discovery_and_watches(
                                                session3,
                                                client,
                                                store,
                                                ns,
                                                tx2.clone(),
                                            ),
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
                            let session3 = session.clone();
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
                            //
                            // Discovery re-runs here too. The kinds have not
                            // changed — it is the same cluster — but the
                            // watches have all just been torn down and every
                            // one of them has to be restarted against the new
                            // store and scope, and the alternative is reading
                            // `session.kinds` in a SEPARATE lock acquisition
                            // before this one, which is precisely the stale-
                            // read shape `restart_watch`'s own doc comment
                            // exists to close. One extra discovery round-trip
                            // per namespace change is the cheaper mistake.
                            restart_watch(session.clone(), ns_choice, move |client, store, ns| {
                                supervise(
                                    "watch",
                                    spawn_discovery_and_watches(
                                        session3,
                                        client,
                                        store,
                                        ns,
                                        tx2.clone(),
                                    ),
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
        // The object the pane is open on is re-resolved here, from THIS
        // frame's object list, rather than held on `OpenDetail`: the YAML and
        // Overview tabs must track the object as the watch updates it. An
        // object that has left the store resolves to nothing and the pane
        // closes with it — a pod that was deleted is not a pod whose YAML we
        // should keep showing as if it were live.
        let detail_object = detail
            .as_ref()
            .and_then(|d| find_object(objects, d.namespace.as_deref(), &d.name))
            .cloned();
        if detail.is_some() && detail_object.is_none() {
            detail = None;
        }
        let (events, events_error) = match detail.as_ref() {
            Some(d) => (d.events.as_slice(), d.events_error.as_deref()),
            None => (&[][..], None),
        };
        term.draw(|f| {
            render_frame(
                f,
                FrameArgs {
                    objects,
                    gvk: &active_kind,
                    table_data: snapshot.table.clone(),
                    context_name: &context_name,
                    display_namespace: scope,
                    status,
                    last_error: last_error.as_deref(),
                    show_hint,
                    connecting: connecting_name.as_deref(),
                    detail_object: detail_object.as_deref(),
                    events,
                    events_error,
                },
                &mut view,
                &mut tree,
                &mut pane,
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
    async fn one_panicking_watch_is_reported_while_its_siblings_still_run() {
        // Plan 3 watches up to forty kinds at once, and a watch task loops
        // forever by design — so "report the failures once they have all
        // finished" reports nothing, ever. This is the case `join_all` cannot
        // pass: it yields no results at all until EVERY future completes, so
        // with a sibling that never returns it hangs here until the timeout
        // below fires. The panic must be visible while the sibling is still
        // running, which is the only state a real cluster is ever in.
        let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
        let forever = tokio::spawn(async { std::future::pending::<()>().await });
        let doomed = tokio::spawn(async { panic!("watcher exploded") });
        // Both orders, because `join_all` polls in order and a panic in the
        // FIRST position could be mistaken for "it works" by a weaker test
        // that only looked at whether any event arrived.
        let children = vec![forever, doomed];

        // The supervisor is left running — it must keep watching the sibling,
        // so it does not return. What is under test is that it EMITS promptly,
        // which is why the timeout is on the channel rather than on the
        // supervisor. Under `join_all` nothing is ever sent and this times out.
        let supervisor = tokio::spawn(supervise_children(children, tx));
        let mut got = Vec::new();
        for _ in 0..2 {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(e)) => got.push(e),
                Ok(None) => break,
                Err(_) => panic!(
                    "a panicking watch was not reported within 5s while a sibling \
                     still ran; got {got:?}"
                ),
            }
        }
        supervisor.abort();

        assert!(
            got.iter()
                .any(|e| matches!(e, AppEvent::Error(m) if m.contains("watcher exploded"))),
            "the panic payload must reach the user, got {got:?}"
        );
        assert!(
            got.iter().any(|e| matches!(e, AppEvent::Quit)),
            "a dead watch must end the app rather than let the bar read `live` \
             over a table nothing updates. got {got:?}"
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
        fn an_error_that_dumps_unbounded_text_cannot_fill_the_status_bar() {
            // Length-bounding is about legibility, not secrecy: an
            // admission-webhook rejection echoed back in a `Status.message` is
            // unbounded and not ours to shorten at the source. Credential
            // material is handled by TYPE in `cluster::redact`, upstream of
            // this — see `truncate_error`'s own comment.
            let dump: String = (0..2000)
                .map(|i| format!("{}, ", i % 256))
                .collect::<String>();
            let e = format!("admission webhook denied the request: {dump}");
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
                out.starts_with("admission webhook denied the request"),
                "the useful part is the front, so that is the part kept; got {out}"
            );
        }

        // --- Credential-plugin output never reaches stderr or the bar ---

        /// The token a plugin would have printed to stdout before failing.
        const LEAKED_TOKEN: &str = "SUPER-SECRET-TOKEN-abc123";

        /// A secret in the process environment. `AuthExecRun.cmd` is
        /// `format!("{cmd:?}")` over a `std::process::Command`, and `Command`'s
        /// `Debug` prints the environment ahead of the program — so this sits
        /// at roughly character 12 of the message, comfortably INSIDE the
        /// 200-character cap. It is the part of this error that the cap never
        /// protected at all.
        const LEAKED_ENV: &str = "hunter2";

        /// The exact error a credential plugin produces when it prints an
        /// `ExecCredential` and then exits non-zero. `Service`, not `Auth`,
        /// because that is the shape the lazy per-request refresh takes; the
        /// `redact` module's own tests cover both.
        fn credential_plugin_failure() -> anyhow::Error {
            use std::os::unix::process::ExitStatusExt;
            let out = std::process::Output {
                status: std::process::ExitStatus::from_raw(256),
                stdout: format!(
                    r#"{{"kind":"ExecCredential","status":{{"token":"{LEAKED_TOKEN}"}}}}"#
                )
                .into_bytes(),
                stderr: Vec::new(),
            };
            anyhow::Error::new(kube::Error::Service(Box::new(
                kube::client::AuthError::AuthExecRun {
                    cmd: format!(
                        "AWS_SECRET_ACCESS_KEY=\"{LEAKED_ENV}\" \"kubelogin\" \"get-token\""
                    ),
                    status: out.status,
                    out,
                },
            )))
        }

        #[test]
        fn the_startup_connect_failure_never_prints_the_token_to_stderr() {
            // `run_with_scope`'s `eprintln!` is the ONE failure path that
            // always reaches the user's real terminal: it runs before the
            // alternate screen exists, so its output lands in shell
            // scrollback, `script` captures and CI logs. It had no truncation
            // at all — the whole token went out verbatim.
            let e = credential_plugin_failure();
            assert!(
                format!("{e:#}").contains(LEAKED_TOKEN),
                "the fixture must actually leak, or this test guards nothing"
            );

            let line = format!(
                "kube: could not connect to a cluster: {}",
                cluster::safe_error_text(&e)
            );
            assert!(
                !line.contains(LEAKED_TOKEN),
                "a bearer token reached stderr: {line}"
            );
        }

        #[test]
        fn every_in_tui_error_path_that_can_carry_a_token_redacts_it() {
            // Plan 3 tripled these: discovery, the per-kind column fetch and
            // the events fetch all format a `kube::Error` into `last_error`.
            // Each is composed here exactly as its call site composes it,
            // `truncate_error` included — because the cap is not what stops
            // this. `AuthExecRun`'s `cmd` field is `Command`'s `Debug`, which
            // prints the process environment at the very FRONT of the message,
            // inside any cap; and the plugin's stdout follows it.
            let composed = [
                truncate_error(format!(
                    "discovering kinds: {} — showing pods only",
                    cluster::safe_error_text(&credential_plugin_failure())
                )),
                truncate_error(format!(
                    "fetching {} columns: {}",
                    "Pod",
                    cluster::safe_error_text(&credential_plugin_failure())
                )),
                truncate_error(cluster::safe_error_text(&credential_plugin_failure())),
                truncate_error(format!(
                    "connecting to {}: {}",
                    "prod",
                    connect_failure_hint(
                        &AuthMethod::Exec {
                            command: "kubelogin".to_string()
                        },
                        &cluster::safe_error_text(&credential_plugin_failure())
                    )
                )),
            ];
            for line in &composed {
                assert!(
                    !line.contains(LEAKED_TOKEN),
                    "a bearer token reached the status bar: {line}"
                );
                assert!(
                    !line.contains(LEAKED_ENV),
                    "a secret from the plugin's environment reached the status bar: {line}"
                );
            }
            // The switch path must still name the plugin — from the
            // kubeconfig, never from the error, whose own `cmd` field carries
            // the process environment.
            assert!(
                composed[3].contains("kubelogin"),
                "redaction must not cost the user the plugin's name: {}",
                composed[3]
            );
        }

        #[test]
        fn truncation_alone_would_not_have_stopped_the_leak() {
            // The mutation this guards against is "keep the length cap, drop
            // the redaction". Two independent reasons the cap is not a
            // mitigation, asserted separately so neither can be argued away.
            //
            // 1. The startup `eprintln!` never went through `truncate_error`
            //    at all — and it is the path that reaches the real terminal.
            let stderr_line = format!(
                "kube: could not connect to a cluster: {:#}",
                credential_plugin_failure()
            );
            assert!(
                stderr_line.contains(LEAKED_TOKEN),
                "the whole token used to go to stderr verbatim; got {stderr_line}"
            );

            // 2. Where the cap DID apply, it bounds LENGTH over content nobody
            //    controls — and the most sensitive part of this message is at
            //    the front, not the tail. `AuthExecRun.cmd` is `Command`'s
            //    `Debug`, which prints the process environment before the
            //    program name, so it lands around character 12 and no budget
            //    that leaves the message readable can exclude it.
            let capped = truncate_error(format!(
                "discovering kinds: {:#} — showing pods only",
                credential_plugin_failure()
            ));
            assert!(
                capped.contains(LEAKED_ENV),
                "the plugin's environment survives the 200-char cap, so the cap is \
                 not the mitigation; got {capped}"
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
            let snap = store_snapshot(&live, &gvk, &[]).await;
            assert_eq!(snap.objects.len(), 3);
            assert_eq!(
                snap.status,
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
            let snap = store_snapshot(&fresh, &gvk, &[]).await;
            assert!(
                snap.objects.is_empty(),
                "the new cluster starts with no objects"
            );
            assert_eq!(
                snap.status,
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

        /// The default `FrameArgs` these render tests vary one field of: one
        /// pod, no fetched table, nothing wrong, no overlay.
        fn frame_args<'a>(
            objects: &'a [Arc<DynamicObject>],
            gvk: &'a GroupVersionKind,
            context_name: &'a str,
        ) -> FrameArgs<'a> {
            FrameArgs {
                objects,
                gvk,
                table_data: None,
                context_name,
                display_namespace: "default",
                status: WatchStatus::Synced,
                last_error: None,
                show_hint: false,
                connecting: None,
                detail_object: None,
                events: &[],
                events_error: None,
            }
        }

        /// An empty tree, as the sidebar looks before discovery lands.
        fn empty_tree() -> KindTree {
            KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            }
        }

        #[test]
        fn render_frame_paints_the_ribbon_in_the_active_clusters_hue() {
            let pods = vec![pod_in("a", "default")];
            let gvk = GroupVersionKind::gvk("", "v1", "Pod");
            let mut view = TableView::new();
            let mut tree = empty_tree();
            let mut pane = DetailPane::new();
            let mut hits = HitRegistry::new();
            let mut overlay = Overlay::None;

            let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
            term.draw(|f| {
                render_frame(
                    f,
                    frame_args(&pods, &gvk, "prod-eu"),
                    &mut view,
                    &mut tree,
                    &mut pane,
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

            let mut tree = empty_tree();
            let mut pane = DetailPane::new();
            let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
            term.draw(|f| {
                render_frame(
                    f,
                    frame_args(&pods, &gvk, "prod"),
                    &mut view,
                    &mut tree,
                    &mut pane,
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

    // --- Task 10: sidebar, detail pane, and refetch wiring ---

    mod wiring {
        use super::*;
        use kube::ResourceExt;
        use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
        use kube_tui::cluster::discovery::KindInfo;
        use kube_tui::store::multi::KindAvailability;
        use kube_tui::store::table::{RowIdentity, SortState, TableColumn, TableData, TableRow};
        use kube_tui::store::watch::{ResourceStore, SharedStore};
        use kube_tui::ui::tree::{TreeGroup, TreeKind, TreeRow, flatten};
        use kube_tui::ui::views::picker::{Picker, PickerItem};
        use ratatui::backend::TestBackend;
        use std::time::{Duration, Instant};
        use tokio::sync::RwLock;

        fn pod_gvk() -> GroupVersionKind {
            GroupVersionKind::gvk("", "v1", "Pod")
        }

        fn pod_ar() -> ApiResource {
            ApiResource::erase::<Pod>(&())
        }

        fn pod_named(name: &str) -> Arc<DynamicObject> {
            Arc::new(DynamicObject::new(name, &pod_ar()).within("demo"))
        }

        fn kind_info(group: &str, kind: &str) -> KindInfo {
            KindInfo {
                gvk: GroupVersionKind::gvk(group, "v1", kind),
                resource: ApiResource {
                    group: group.to_string(),
                    api_version: if group.is_empty() {
                        "v1".to_string()
                    } else {
                        format!("{group}/v1")
                    },
                    kind: kind.to_string(),
                    version: "v1".to_string(),
                    plural: format!("{}s", kind.to_lowercase()),
                },
                namespaced: true,
                group_label: if group.is_empty() {
                    "core".to_string()
                } else {
                    group.to_string()
                },
            }
        }

        /// A server-rendered Table whose row ORDER deliberately disagrees
        /// with the object list below it, and whose NAME cells deliberately
        /// disagree with the identities attached to the rows.
        ///
        /// Both are what a positional resolution gets wrong, and each on its
        /// own would let a different wrong implementation pass: matching by
        /// position picks the wrong row, and "read the NAME cell" (the
        /// obvious shortcut `row_identity` exists to forbid, since a CRD's
        /// NAME column need not be `metadata.name`) picks the wrong object
        /// from the right row.
        fn disagreeing_table() -> TableData {
            TableData {
                columns: vec![
                    TableColumn {
                        name: "Name".to_string(),
                        priority: 0,
                    },
                    TableColumn {
                        name: "Restarts".to_string(),
                        priority: 0,
                    },
                ],
                rows: vec![
                    row("displayed-as-zulu", "10", "web-zulu"),
                    row("displayed-as-alpha", "2", "web-alpha"),
                    row("displayed-as-mike", "9", "web-mike"),
                ],
            }
        }

        fn row(cell_name: &str, restarts: &str, real_name: &str) -> TableRow {
            TableRow {
                cells: vec![cell_name.to_string(), restarts.to_string()],
                identity: Some(RowIdentity {
                    namespace: Some("demo".to_string()),
                    name: real_name.to_string(),
                }),
            }
        }

        #[test]
        fn a_selected_server_row_resolves_through_its_own_identity() {
            // The object list is in a different order from the table's rows
            // (the watch and the fetch are refreshed at different moments),
            // and the NAME cell of every row names something that is not the
            // object. Only `row_identity` gives the right answer here.
            let objects = vec![
                pod_named("web-alpha"),
                pod_named("web-mike"),
                pod_named("web-zulu"),
            ];
            let table = disagreeing_table();
            assert_eq!(
                selected_object(&objects, &pod_gvk(), Some(&table), None, 2),
                Some((Some("demo".to_string()), "web-mike".to_string())),
                "row 2 displays web-mike; indexing `objects` positionally \
                 would answer web-zulu, and reading the NAME cell would \
                 answer 'displayed-as-mike', which is no object at all"
            );
        }

        #[test]
        fn a_selected_server_row_follows_the_sort_the_table_is_drawn_in() {
            // `render_table_with_data` sorts a copy of the rows before
            // drawing, and registers hit zones against the SORTED order — so
            // selection is an index into the sorted list. Resolving against
            // the unsorted one opens the pane on a different object.
            //
            // Sorted ascending by Restarts (numeric): 2 (web-alpha), 9
            // (web-mike), 10 (web-zulu). Row 0 is therefore web-alpha, which
            // is a DIFFERENT object from the unsorted row 0 (web-zulu) — and
            // also, deliberately, from the object at `objects[0]`.
            let objects = vec![
                pod_named("web-zulu"),
                pod_named("web-mike"),
                pod_named("web-alpha"),
            ];
            let table = disagreeing_table();
            let sort = SortState {
                column: 1,
                descending: false,
            };
            assert_eq!(
                selected_object(&objects, &pod_gvk(), Some(&table), Some(&sort), 0),
                Some((Some("demo".to_string()), "web-alpha".to_string())),
                "the first row of the SORTED table is web-alpha"
            );
            assert_eq!(
                selected_object(&objects, &pod_gvk(), Some(&table), Some(&sort), 2),
                Some((Some("demo".to_string()), "web-zulu".to_string())),
                "and the last is web-zulu — 10 restarts sorts after 9 \
                 numerically, not before it lexically"
            );
        }

        #[test]
        fn a_selected_builtin_column_row_follows_the_sort_too() {
            // Before the first Table fetch lands there is no server row to
            // carry an identity, and the rows really are `objects` in order —
            // right up until a column header is clicked. Sorted by NAME
            // ascending, `objects[0]` (web-zulu) is drawn LAST, so a resolver
            // that ignored the sort would answer web-zulu for row 0.
            let objects = vec![
                pod_named("web-zulu"),
                pod_named("web-mike"),
                pod_named("web-alpha"),
            ];
            let sort = SortState {
                column: 0,
                descending: false,
            };
            assert_eq!(
                selected_object(&objects, &pod_gvk(), None, Some(&sort), 0),
                Some((Some("demo".to_string()), "web-alpha".to_string()))
            );
            assert_eq!(
                selected_object(&objects, &pod_gvk(), None, None, 0),
                Some((Some("demo".to_string()), "web-zulu".to_string())),
                "and unsorted, row 0 is the first object as loaded"
            );
        }

        #[test]
        fn a_row_with_no_identity_resolves_to_nothing_rather_than_a_guess() {
            // An apiserver that ignored `includeObject=Metadata`. A guess
            // from the NAME cell would be wrong for any CRD whose NAME column
            // is not `metadata.name`.
            let objects = vec![pod_named("web-alpha")];
            let table = TableData {
                columns: vec![TableColumn {
                    name: "Name".to_string(),
                    priority: 0,
                }],
                rows: vec![TableRow {
                    cells: vec!["web-alpha".to_string()],
                    identity: None,
                }],
            };
            assert_eq!(
                selected_object(&objects, &pod_gvk(), Some(&table), None, 0),
                None
            );
        }

        #[test]
        fn a_selection_past_the_end_resolves_to_nothing() {
            let objects = vec![pod_named("web-alpha")];
            assert_eq!(selected_object(&objects, &pod_gvk(), None, None, 9), None);
            assert_eq!(
                selected_object(&[], &pod_gvk(), None, None, 0),
                None,
                "an empty table has nothing to open"
            );
        }

        #[test]
        fn the_detail_panes_object_is_re_resolved_from_the_live_list() {
            let objects = vec![pod_named("web-alpha"), pod_named("web-mike")];
            assert_eq!(
                find_object(&objects, Some("demo"), "web-mike").map(|o| o.name_any()),
                Some("web-mike".to_string())
            );
            assert!(
                find_object(&objects, Some("other"), "web-mike").is_none(),
                "a name alone must not match across namespaces"
            );
            assert!(
                find_object(&objects, Some("demo"), "web-gone").is_none(),
                "an object that has left the store must resolve to nothing"
            );
        }

        // --- the store snapshot the sidebar reads ---

        async fn store_with_facts() -> SharedStore {
            let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
            {
                let mut s = store.write().await;
                for n in ["a", "b", "c"] {
                    s.apply(
                        &pod_gvk(),
                        &pod_ar(),
                        kube::runtime::watcher::Event::Apply(
                            DynamicObject::new(n, &pod_ar()).within("demo"),
                        ),
                    );
                }
                // Pods are watched and healthy, but the cap left them out of
                // the eager watch set — so `status` says nothing is wrong
                // while `availability` says they are not being watched.
                s.set_status(pod_gvk(), WatchStatus::Synced);
                s.set_availability(pod_gvk(), KindAvailability::NotWatched);

                // Secrets are watched and forbidden: the apiserver's own
                // reason is recorded, and `WatchStatus::Failed` alone cannot
                // reproduce it.
                let secret = GroupVersionKind::gvk("", "v1", "Secret");
                s.set_status(secret.clone(), WatchStatus::Failed);
                s.set_availability(
                    secret,
                    KindAvailability::Unavailable {
                        reason: "secrets is forbidden".to_string(),
                    },
                );
            }
            store
        }

        #[tokio::test]
        async fn the_snapshot_reads_availability_from_the_store_not_from_watch_status() {
            // The mutation this guards: deriving each kind's availability in
            // the render path — from `WatchStatus`, or worse by matching on
            // an error string — instead of reading the answer the store
            // already holds. Both kinds below are chosen so that derivation
            // gives a DIFFERENT answer: Pod is `Synced` yet not watched at
            // all, and Secret's reason exists nowhere in its status.
            let store = store_with_facts().await;
            let kinds = vec![kind_info("", "Pod"), kind_info("", "Secret")];
            let snap = store_snapshot(&store, &pod_gvk(), &kinds).await;

            assert_eq!(snap.kind_facts.len(), 2, "one fact per kind, in order");
            assert_eq!(
                snap.kind_facts[0],
                (3, KindAvailability::NotWatched),
                "a kind cut by the cap is not 'watching': deriving from \
                 WatchStatus::Synced would say it was"
            );
            assert_eq!(
                snap.kind_facts[1],
                (
                    0,
                    KindAvailability::Unavailable {
                        reason: "secrets is forbidden".to_string()
                    }
                ),
                "the apiserver's own reason must survive to the sidebar; \
                 WatchStatus::Failed carries no reason at all"
            );
        }

        #[tokio::test]
        async fn the_snapshot_carries_the_refetch_bookkeeping_for_the_active_kind() {
            let store = store_with_facts().await;
            let kinds = vec![kind_info("", "Pod")];
            let snap = store_snapshot(&store, &pod_gvk(), &kinds).await;
            assert!(
                snap.last_change.is_some(),
                "three pods were applied, so this kind has changed"
            );
            assert_eq!(
                snap.last_table_fetch, None,
                "nothing has been fetched for it yet"
            );
        }

        // --- the sidebar tree ---

        fn facts_for(kinds: &[KindInfo]) -> Vec<(usize, KindAvailability)> {
            kinds
                .iter()
                .enumerate()
                .map(|(i, _)| (i, KindAvailability::Watching))
                .collect()
        }

        fn tree_labels(tree: &KindTree) -> Vec<String> {
            flatten(tree)
                .iter()
                .map(|r| match r {
                    TreeRow::Group { group, .. } => group.label.clone(),
                    TreeRow::Kind { kind, .. } => format!("  {}", kind.label),
                })
                .collect()
        }

        #[test]
        fn the_tree_groups_kinds_and_carries_each_ones_count_and_availability() {
            let kinds = vec![
                kind_info("", "Pod"),
                kind_info("", "Service"),
                kind_info("apps", "Deployment"),
            ];
            let facts = vec![
                (7, KindAvailability::Watching),
                (
                    0,
                    KindAvailability::Unavailable {
                        reason: "forbidden".to_string(),
                    },
                ),
                (2, KindAvailability::NotWatched),
            ];
            let mut tree = KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            };
            refresh_tree(&mut tree, &kinds, &facts, &pod_gvk());

            let core = tree
                .groups
                .iter()
                .find(|g| g.label == "core")
                .expect("the core group must exist");
            assert_eq!(
                core.kinds
                    .iter()
                    .map(|k| k.label.as_str())
                    .collect::<Vec<_>>(),
                vec!["Pod", "Service"],
                "kinds keep discovery's stable order within their group"
            );
            assert_eq!(core.kinds[0].count, Some(7));
            assert_eq!(core.kinds[0].availability, KindAvailability::Watching);
            assert_eq!(
                core.kinds[1].availability,
                KindAvailability::Unavailable {
                    reason: "forbidden".to_string()
                },
                "the store's reason must reach the row the sidebar draws"
            );
            let apps = tree
                .groups
                .iter()
                .find(|g| g.label == "apps")
                .expect("the apps group must exist");
            assert_eq!(apps.kinds[0].availability, KindAvailability::NotWatched);
        }

        #[test]
        fn the_group_holding_the_active_kind_opens_and_the_rest_stay_shut() {
            // Twenty API groups is normal. Expanding all of them buries the
            // one kind the table is actually showing; expanding none leaves
            // no sign of where it is.
            let kinds = vec![
                kind_info("acme.io", "Widget"),
                kind_info("apps", "Deployment"),
                kind_info("", "Pod"),
            ];
            let mut tree = KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            };
            refresh_tree(&mut tree, &kinds, &facts_for(&kinds), &pod_gvk());

            let expanded: Vec<&str> = tree
                .groups
                .iter()
                .filter(|g| g.expanded)
                .map(|g| g.label.as_str())
                .collect();
            assert_eq!(
                expanded,
                vec!["core"],
                "only the group holding the active kind (core/v1 Pod) opens"
            );
        }

        #[test]
        fn a_rebuild_keeps_what_the_user_expanded_and_what_they_selected() {
            // Counts change constantly, so the tree is rebuilt constantly.
            // If a rebuild reset expansion, a collapsed group would spring
            // open the moment a pod restarted somewhere; if it reset the
            // selection, the sidebar highlight would jump under the user's
            // hand several times a second.
            let kinds = vec![
                kind_info("", "Pod"),
                kind_info("", "Service"),
                kind_info("apps", "Deployment"),
            ];
            let mut tree = KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            };
            refresh_tree(&mut tree, &kinds, &facts_for(&kinds), &pod_gvk());

            // The user opens `apps` and collapses `core`, then selects the
            // Deployment row. `apps` sorts before `core`, so with core
            // collapsed the rows are: apps, Deployment, core.
            for g in &mut tree.groups {
                g.expanded = g.label == "apps";
            }
            assert_eq!(tree_labels(&tree), vec!["apps", "  Deployment", "core"]);
            tree.selected = 1;

            // A count arrives for every kind — a rebuild with the same kinds
            // but different facts.
            let facts = vec![
                (99, KindAvailability::Watching),
                (5, KindAvailability::Watching),
                (1, KindAvailability::Watching),
            ];
            refresh_tree(&mut tree, &kinds, &facts, &pod_gvk());

            assert_eq!(
                tree_labels(&tree),
                vec!["apps", "  Deployment", "core"],
                "the user's expansion state must survive a rebuild"
            );
            assert_eq!(
                tree.selected_kind().map(|k| k.label.clone()),
                Some("Deployment".to_string()),
                "and so must the selection"
            );
            assert_eq!(
                tree.selected_kind().and_then(|k| k.count),
                Some(1),
                "with the new count, not the old one"
            );
        }

        #[test]
        fn a_rebuild_keeps_the_selection_on_the_same_kind_when_rows_shift_above_it() {
            // The case a row-index-preserving implementation gets wrong, and
            // the reason the anchor is an identity: a group ABOVE the
            // selection gains a kind, so every row below it moves down by
            // one. Keeping `selected` numerically would silently re-point it
            // at whatever now occupies that row.
            let before = vec![kind_info("apps", "Deployment"), kind_info("zoo.io", "Ape")];
            let mut tree = KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            };
            refresh_tree(&mut tree, &before, &facts_for(&before), &pod_gvk());
            for g in &mut tree.groups {
                g.expanded = true;
            }
            assert_eq!(
                tree_labels(&tree),
                vec!["apps", "  Deployment", "zoo.io", "  Ape"]
            );
            tree.selected = 3; // "Ape"

            let after = vec![
                kind_info("apps", "DaemonSet"),
                kind_info("apps", "Deployment"),
                kind_info("zoo.io", "Ape"),
            ];
            refresh_tree(&mut tree, &after, &facts_for(&after), &pod_gvk());

            assert_eq!(
                tree_labels(&tree),
                vec!["apps", "  DaemonSet", "  Deployment", "zoo.io", "  Ape"]
            );
            assert_eq!(
                tree.selected_kind().map(|k| k.label.clone()),
                Some("Ape".to_string()),
                "the selection must follow the kind, not the row number — \
                 row 3 is now Deployment"
            );
        }

        #[test]
        fn a_rebuild_that_removes_the_selected_kind_clamps_rather_than_dangling() {
            // A cluster switch, or a CRD uninstalled mid-session.
            let before = vec![kind_info("acme.io", "Widget")];
            let mut tree = KindTree {
                groups: Vec::new(),
                selected: 0,
                scroll: 0,
            };
            refresh_tree(&mut tree, &before, &facts_for(&before), &pod_gvk());
            tree.groups[0].expanded = true;
            tree.selected = 1;

            let after = vec![kind_info("", "Pod")];
            refresh_tree(&mut tree, &after, &facts_for(&after), &pod_gvk());
            assert!(
                tree.selected < flatten(&tree).len(),
                "the selection must stay inside the tree"
            );
        }

        // --- the Table refetch trigger ---

        #[test]
        fn a_refetch_waits_for_the_burst_to_settle() {
            // The mutation this guards: firing as soon as anything changed,
            // ignoring the debounce. A rollout touching fifty pods produces a
            // burst of deltas; one Table GET per delta hammers the apiserver
            // on exactly the namespace someone is watching a rollout in.
            let now = Instant::now();
            let debounce = Duration::from_millis(750);
            let changed_just_now = now - Duration::from_millis(10);
            assert!(
                !table_fetch_due(None, Some(changed_just_now), now, debounce),
                "a change 10ms old has not settled; a fetch now would be one \
                 of fifty"
            );
            let settled = now - Duration::from_millis(800);
            assert!(
                table_fetch_due(None, Some(settled), now, debounce),
                "once the burst has been quiet for the debounce, fetch"
            );
        }

        #[test]
        fn a_kind_already_fetched_since_its_last_change_is_not_fetched_again() {
            // What keeps a static namespace from being refetched merely
            // because time passed — the other half of idle CPU staying at
            // zero.
            let now = Instant::now();
            let debounce = Duration::from_millis(750);
            let changed = now - Duration::from_secs(60);
            let fetched = now - Duration::from_secs(30);
            assert!(!table_fetch_due(
                Some(fetched),
                Some(changed),
                now,
                debounce
            ));
            assert!(
                table_fetch_due(
                    Some(changed - Duration::from_secs(1)),
                    Some(changed),
                    now,
                    debounce
                ),
                "a fetch that predates the change is stale and must be redone"
            );
        }

        #[test]
        fn a_kind_that_has_never_changed_is_fetched_exactly_once() {
            // Selecting a kind whose watch has delivered nothing — an empty
            // namespace, or one whose watch has not synced yet. There is no
            // change to debounce against, but its columns have never been
            // fetched, and without this the table would sit on the builtin
            // NAME/AGE fallback for ever.
            let now = Instant::now();
            let debounce = Duration::from_millis(750);
            assert!(
                table_fetch_due(None, None, now, debounce),
                "never fetched, never changed: fetch it once"
            );
            assert!(
                !table_fetch_due(Some(now - Duration::from_secs(1)), None, now, debounce),
                "and having fetched it, do not fetch it again on every wake"
            );
        }

        // --- kind-aware redraw: 40 eagerly-watched kinds must not repaint
        // the table body just because one of them changed ---

        #[test]
        fn a_batch_of_only_non_active_kind_changes_does_not_dirty_the_table_body() {
            // Lease, Event and EndpointSlice all churn continuously on a
            // stock cluster with no user activity at all. A batch made
            // entirely of their deltas — none of them the kind on screen —
            // must not force a redraw whose output would be pixel-identical
            // to the last frame.
            let active = pod_gvk();
            let lease = GroupVersionKind::gvk("coordination.k8s.io", "v1", "Lease");
            let event = GroupVersionKind::gvk("events.k8s.io", "v1", "Event");
            let changed: std::collections::HashSet<GroupVersionKind> =
                [lease, event].into_iter().collect();
            assert!(
                !table_body_is_dirty(&changed, &active),
                "none of the changed kinds is the one on screen"
            );
        }

        #[test]
        fn a_batch_containing_the_active_kind_does_dirty_the_table_body() {
            let active = pod_gvk();
            let lease = GroupVersionKind::gvk("coordination.k8s.io", "v1", "Lease");
            let changed: std::collections::HashSet<GroupVersionKind> =
                [lease, active.clone()].into_iter().collect();
            assert!(
                table_body_is_dirty(&changed, &active),
                "the active kind is in the batch, alongside unrelated churn"
            );
        }

        #[test]
        fn an_empty_changed_set_does_not_dirty_the_table_body() {
            let active = pod_gvk();
            assert!(!table_body_is_dirty(
                &std::collections::HashSet::new(),
                &active
            ));
        }

        // --- the sidebar's counts redraw only on a genuine count change ---

        fn deployment_gvk() -> GroupVersionKind {
            GroupVersionKind::gvk("apps", "v1", "Deployment")
        }

        #[test]
        fn an_in_place_update_that_leaves_every_count_unchanged_does_not_redraw() {
            // A Lease renewal, an EndpointSlice reshuffle: the object count
            // for that kind is exactly what it was. Recomputing it would
            // draw the identical sidebar.
            let kinds = vec![kind_info("", "Pod"), kind_info("apps", "Deployment")];
            let facts = vec![
                (3, KindAvailability::Watching),
                (2, KindAvailability::Watching),
            ];
            let last_counts: HashMap<GroupVersionKind, usize> =
                [(pod_gvk(), 3), (deployment_gvk(), 2)]
                    .into_iter()
                    .collect();
            assert!(
                !counts_changed(&kinds, &facts, &last_counts),
                "identical counts to what was last recorded must not redraw"
            );
        }

        #[test]
        fn a_genuine_count_change_does_redraw() {
            let kinds = vec![kind_info("", "Pod"), kind_info("apps", "Deployment")];
            let facts = vec![
                (4, KindAvailability::Watching), // Pod count went 3 -> 4
                (2, KindAvailability::Watching),
            ];
            let last_counts: HashMap<GroupVersionKind, usize> =
                [(pod_gvk(), 3), (deployment_gvk(), 2)]
                    .into_iter()
                    .collect();
            assert!(
                counts_changed(&kinds, &facts, &last_counts),
                "a real create/delete must still reach the sidebar"
            );
        }

        #[test]
        fn a_kind_with_no_prior_recorded_count_counts_as_changed() {
            // Newly discovered, or the very first pass — there is nothing to
            // compare against, so treat it as a change rather than silently
            // never drawing it.
            let kinds = vec![kind_info("", "Pod")];
            let facts = vec![(1, KindAvailability::Watching)];
            assert!(counts_changed(&kinds, &facts, &HashMap::new()));
        }

        // --- opening the pane by mouse ---

        #[test]
        fn two_quick_clicks_on_the_same_row_are_a_double_click() {
            let t0 = Instant::now();
            assert!(is_double_click(
                Some((3, t0)),
                3,
                t0 + Duration::from_millis(120)
            ));
        }

        #[test]
        fn two_slow_clicks_on_the_same_row_are_two_selections() {
            let t0 = Instant::now();
            assert!(!is_double_click(
                Some((3, t0)),
                3,
                t0 + Duration::from_millis(900)
            ));
        }

        #[test]
        fn two_quick_clicks_on_different_rows_are_two_selections() {
            // Fast selection of two adjacent rows must not open a pane on the
            // second one.
            let t0 = Instant::now();
            assert!(!is_double_click(
                Some((3, t0)),
                4,
                t0 + Duration::from_millis(50)
            ));
        }

        #[test]
        fn the_first_click_of_the_session_is_never_a_double_click() {
            assert!(!is_double_click(None, 0, Instant::now()));
        }

        // --- events cannot be shown under the wrong object ---

        fn open_on(name: &str) -> OpenDetail {
            OpenDetail {
                gvk: pod_gvk(),
                namespace: Some("demo".to_string()),
                name: name.to_string(),
                events: Vec::new(),
                events_error: None,
            }
        }

        #[test]
        fn an_events_reply_is_accepted_only_for_the_object_the_pane_is_open_on() {
            let open = open_on("web-alpha");
            assert!(open.is_for(&pod_gvk(), Some("demo"), "web-alpha"));
            assert!(
                !open.is_for(&pod_gvk(), Some("demo"), "web-mike"),
                "a reply for the object the user just moved away from must \
                 not be shown under this one's name"
            );
            assert!(
                !open.is_for(&pod_gvk(), Some("kube-system"), "web-alpha"),
                "the same name in another namespace is another object"
            );
            assert!(
                !open.is_for(
                    &GroupVersionKind::gvk("apps", "v1", "Deployment"),
                    Some("demo"),
                    "web-alpha"
                ),
                "a Deployment and a Pod can share a name"
            );
        }

        // --- draw order ---

        fn thirty_pods() -> Vec<Arc<DynamicObject>> {
            (0..30).map(|i| pod_named(&format!("pod-{i:02}"))).collect()
        }

        fn screen(term: &Terminal<TestBackend>) -> String {
            let buf = term.backend().buffer();
            let area = buf.area;
            let mut out = String::new();
            for y in 0..area.height {
                for x in 0..area.width {
                    out.push_str(buf[(x, y)].symbol());
                }
                out.push('\n');
            }
            out
        }

        fn draw(
            detail_object: Option<&DynamicObject>,
            overlay: &mut Overlay,
            tree: &mut KindTree,
        ) -> Terminal<TestBackend> {
            let pods = thirty_pods();
            let gvk = pod_gvk();
            let mut view = TableView::new();
            let mut pane = DetailPane::new();
            let mut hits = HitRegistry::new();
            let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
            term.draw(|f| {
                render_frame(
                    f,
                    FrameArgs {
                        objects: &pods,
                        gvk: &gvk,
                        table_data: None,
                        context_name: "prod",
                        display_namespace: "demo",
                        status: WatchStatus::Synced,
                        last_error: None,
                        show_hint: false,
                        connecting: None,
                        detail_object,
                        events: &[],
                        events_error: None,
                    },
                    &mut view,
                    tree,
                    &mut pane,
                    overlay,
                    &mut hits,
                );
            })
            .unwrap();
            term
        }

        fn sidebar_tree() -> KindTree {
            KindTree {
                groups: vec![TreeGroup {
                    label: "core".to_string(),
                    expanded: true,
                    kinds: vec![TreeKind {
                        gvk: pod_gvk(),
                        label: "Pod".to_string(),
                        count: Some(30),
                        availability: KindAvailability::Watching,
                    }],
                }],
                selected: 1,
                scroll: 0,
            }
        }

        #[test]
        fn the_sidebar_is_drawn_beside_the_table_not_instead_of_it() {
            let mut overlay = Overlay::None;
            let mut tree = sidebar_tree();
            let term = draw(None, &mut overlay, &mut tree);
            let text = screen(&term);
            assert!(
                text.contains("Kinds"),
                "the sidebar's own frame must be on screen:\n{text}"
            );
            assert!(text.contains("Pod"), "and the kind it lists:\n{text}");
            assert!(
                text.contains("pod-00"),
                "the table must still be drawn alongside it:\n{text}"
            );
        }

        #[test]
        fn the_detail_pane_paints_over_the_table_it_covers() {
            // The control first: with no pane open, every stub pod renders
            // "Unknown" in its STATUS cell, so the string is definitely on
            // screen and this assertion is not vacuous.
            let mut overlay = Overlay::None;
            let mut tree = sidebar_tree();
            let without = screen(&draw(None, &mut overlay, &mut tree));
            assert!(
                without.contains("Unknown"),
                "control: the table's STATUS cells must be visible with no \
                 pane open:\n{without}"
            );

            let obj = pod_named("pod-07");
            let with = screen(&draw(Some(&obj), &mut overlay, &mut tree));
            assert!(
                with.contains("Overview"),
                "the pane's tab bar must be drawn:\n{with}"
            );
            assert!(
                !with.contains("Unknown"),
                "the pane covers the whole table area, so no table row may \
                 bleed through it — it was not drawn last:\n{with}"
            );
        }

        #[test]
        fn a_picker_paints_over_the_detail_pane_as_well_as_the_table() {
            // The picker is modal over everything: a cluster switch stays
            // reachable with a detail pane open. The label is long on purpose
            // — a short one is drawn entirely to the LEFT of the detail
            // pane's own region, where a wrong draw order could not overwrite
            // it, which would make this fixture vacuous.
            let long = "prod-eu-west-1-platform-cluster";
            let mut overlay = Overlay::ClusterPicker(Picker {
                title: "Clusters".into(),
                items: vec![PickerItem {
                    label: long.to_string(),
                    detail: String::new(),
                    accent: None,
                }],
                filter: String::new(),
                selected: 0,
                scroll: 0,
            });
            let mut tree = sidebar_tree();
            let obj = pod_named("pod-07");
            let text = screen(&draw(Some(&obj), &mut overlay, &mut tree));
            assert!(
                text.contains(long),
                "the picker must survive the detail pane drawn beneath it:\n{text}"
            );
        }
    }
}
