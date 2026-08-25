# kube — a Lens-class Kubernetes TUI

**Date:** 2026-08-25
**Status:** Design approved, pending implementation plan

## Purpose

A terminal UI for Kubernetes that is fast enough to feel instant, discoverable enough
to use without memorising commands, and fully driveable by mouse. It targets the
workflow people actually open Lens for: find a resource, understand its state, read
its logs, get the logs out.

Non-goal for v1: replacing `kubectl` for mutation. See [Scope](#scope).

## Scope

### v1

- Multi-cluster: list contexts from kubeconfig, switch context at runtime.
- Namespace selection, including all-namespaces.
- Resource browsing across core kinds: pods, deployments, replicasets, statefulsets,
  daemonsets, jobs, cronjobs, services, ingresses, configmaps, secrets, PVs, PVCs,
  nodes, namespaces, events, service accounts.
- CRDs discovered dynamically and browsable.
- Detail view per resource: overview, full YAML, related events, logs.
- Logs: live tail, multi-pod and multi-container aggregation, search and filter,
  time-range selection, export in four forms.
- Query: fuzzy name filter, label selectors, field selectors, jq/JSONPath against a
  selected object.
- Mouse throughout: click, scroll, drag-resize, click-to-sort, double-click-to-open.

### Deferred to v2 (each gets its own spec)

- YAML editing, apply, patch, scale, delete, rollout restart.
- Exec into containers (PTY over the exec websocket).
- Port-forwarding.
- Metrics and charts (metrics-server / Prometheus).
- Cross-resource queries ("services with no endpoints").
- Helm release management.

v1 is deliberately read-only. This removes the entire confirmation/rollback/RBAC-error
design surface from the first milestone, and means an early build cannot damage a
cluster.

## Success criteria

1. Cold start to an interactive, populated pod list on a 1000-pod cluster: under 2s.
2. Any navigation action already in cache renders in under 16ms (one frame).
3. Idle CPU at 0% — no polling, no fixed-rate redraw.
4. Every action reachable by mouse alone; every action also reachable by keyboard.
5. A panic or API failure never leaves the terminal in an unusable state.
6. Memory bounded and predictable: log buffers capped, object cache proportional to
   cluster size.

## Architecture

Three layers communicating over channels.

```
UI layer (sync, single-threaded)
  render loop · widget draw · hit-test registry · focus management
        ▲ Event                          │ Command
Store layer (async, tokio)
  watch-driven cache · subscriptions · query engine
        ▲ watch deltas                   │ list / watch / log
Cluster layer
  kube-rs: kubeconfig · contexts · client · API discovery
```

### Invariant: the UI never performs I/O

The UI thread never `await`s and never touches the network. It reads a snapshot of the
store and draws. All I/O happens in tokio tasks that push `Event`s into a single
channel. No network latency can stall input handling.

### Invariant: render on event, not on a tick

The event loop drains the channel completely, coalescing bursts, then renders once.
A watch storm delivering 10,000 deltas produces one repaint. Idle produces none.

### Crate selection (versions verified 2026-08-25)

| Crate | Version | Role |
|---|---|---|
| `kube` | 4.2.0 | Client, config, discovery, `runtime::watcher` |
| `k8s-openapi` | 0.28.0 | Typed API objects |
| `ratatui` | 0.30.2 | Terminal rendering |
| `crossterm` | 0.29.0 | Terminal backend, mouse capture, `EventStream` |
| `tokio` | 1.53.1 | Async runtime |

jq and JSONPath crate selection is deferred to the implementation plan, which will
evaluate candidates against: pure Rust, no C dependency, maintained, and supporting
the subset of jq needed for field extraction.

## Store layer

### Structure

```rust
struct ResourceStore {
    kinds: HashMap<GroupVersionKind, KindCache>,
}

struct KindCache {
    objects: IndexMap<ObjectRef, Arc<DynamicObject>>,
    resource_version: String,
    status: WatchStatus,  // Initialising | Synced | Reconnecting | Failed
}
```

`IndexMap` preserves insertion order for stable rendering while giving O(1) lookup by
reference. `Arc` means the UI clones pointers rather than objects; rendering 5,000 rows
copies 5,000 pointers.

### Watch strategy

Built on `kube::runtime::watcher`, which handles relist-on-`410 Gone` and resync.
Watches are started per `(kind, namespace)` **on demand** when a view first needs them,
and are reference-counted so they stop when nothing is watching.

The target is small-to-medium clusters (up to a few thousand pods), where holding
everything in memory is cheap. The on-demand per-(kind, namespace) design is the seam
that lets large clusters work without a rewrite: scoping already exists, only the
eviction policy would need adding.

### Dynamic objects

All kinds go through `DynamicObject` + `ApiResource` rather than typed structs. One
code path serves every built-in kind and every CRD. Typed access happens only where a
view needs specific semantics, by deserialising the relevant subtree.

### Column rendering

Two-tier:

1. A built-in column registry for hot kinds (pods, deployments, nodes, services…),
   giving control over formatting, colouring, and derived columns like READY and
   RESTARTS.
2. Fallback to server-side table rendering — `Accept: application/json;as=Table;v=1;g=meta.k8s.io`
   — for any kind without a registry entry. This is the same mechanism `kubectl` uses,
   so unknown kinds and CRDs render with their declared `additionalPrinterColumns`
   without any per-kind code.

### Subscriptions

Views subscribe to `(kind, namespace)`. The store notifies subscribers on change; the
UI marks itself dirty and re-renders on the next loop iteration. Subscription is the
only coupling between store and UI.

## UI layer

### Layout

Three panes: a persistent sidebar tree of resource kinds with live counts, a main
table, and a detail pane that opens over the table with tabs (Overview / YAML /
Events / Logs). A header carries context and namespace pickers plus the filter bar;
a footer carries status, watch health, and contextual actions.

Panes are resizable by dragging borders. Sidebar is collapsible.

### Mouse: hit-test registry

Ratatui is immediate-mode and has no widget tree, so a click at `(col, row)` carries no
meaning by itself. The solution is a hit-test registry rebuilt every frame:

```rust
struct HitRegistry { zones: Vec<(Rect, ZIndex, HitTarget)> }

enum HitTarget {
    SidebarItem(KindRef),
    TableRow(usize),
    ColumnHeader(usize),
    PaneBorder(PaneId, Edge),
    Tab(TabId),
    CloseButton(PaneId),
    LogLine(usize),
    ContextPicker,
    NamespacePicker,
}
```

Widgets register their `Rect` as they draw. Mouse events walk the registry in reverse
z-order and dispatch to the topmost hit. This is the approach immediate-mode GUIs such
as egui use. It is a pure function of registry and coordinates, so it is testable
without a terminal, and it makes every drawn element clickable by construction rather
than as a per-widget special case.

### Mouse behaviours

- Scroll wheel targets the region **under the cursor**, not the focused pane.
- Drag pane borders to resize; drag column edges to resize columns.
- Click a column header to sort; click again to reverse.
- Single-click selects a row; double-click opens the detail pane.
- Click sidebar entries to switch kind; click pickers to open dropdowns.

### Terminal text selection

Enabling mouse capture takes over the terminal's native click-drag copy. This is the
most common way TUIs frustrate users. Mitigation: most terminals pass events through
when Shift is held, and a dedicated toggle key disables capture entirely for a
copy-paste session. Both are in v1, and the toggle state is shown in the footer.

### Keyboard

Every mouse action has a keyboard equivalent. A command palette provides fuzzy access
to all actions. Vim-style navigation keys where they are unambiguous.

## Logs

### Streaming

One tokio task per `(pod, container)` calling `Api::log_stream`, all feeding a single
channel with a source tag. Tasks are cancelled when the view closes.

### Buffering

A bounded ring buffer, default 100,000 lines, shared across the aggregated view. When
full, oldest lines drop. History beyond the buffer is fetched from the API on demand
via "load older".

### Ordering

Two modes, both legitimate:

- **Arrival order** (default): zero added latency, correct for tailing.
- **Timestamp-sorted within a reorder window**: buffers briefly to interleave sources
  correctly. Correct for incident forensics where cross-pod ordering matters.

### Filtering

Filtering maintains a vector of matching indices rather than copying lines. Filtering
100k lines is a scan over indices; clearing a filter is instant. Regex is compiled once
on change, not per line. Literal and regex modes, plus exclude patterns.

### Follow mode

Auto-scroll follows the tail. Scrolling up disengages it; returning to the bottom
re-engages it. This matches terminal behaviour and avoids the common bug of yanking the
viewport away while the user is reading.

### Export

Four forms, all writing to a user-confirmed path with a sensible default name
(`{namespace}_{workload}_{timestamp}.log`):

1. **Current view**: exactly what is on screen, including active filters and
   aggregation.
2. **Raw per-container**: unfiltered, one file per container, including previous-container
   logs from crashed instances, into a timestamped directory.
3. **Time-range**: a selected window, mapping to `sinceTime` / `sinceSeconds` /
   `tailLines`.
4. **NDJSON**: one JSON object per line, `{pod, container, ts, msg}`, for downstream
   tooling.

Time-range selection composes with the other three rather than being a separate mode.

## Query

### v1

- Fuzzy name filter, applied live as typed.
- Label selectors using the standard grammar: `app=api,tier!=web`, `in`, `notin`,
  existence.
- Field selectors using the standard grammar.
- jq/JSONPath expressions evaluated against the selected object's JSON, with results
  shown in the detail pane.

Filters apply against the in-memory cache, so they are local and instant, and they
compose with the current kind and namespace scope.

### Seam for v2

The store exposes:

```rust
fn query(&self, selector: &Selector) -> Vec<Arc<DynamicObject>>
```

Cross-resource query in v2 is a layer above this, not a refactor of it. Because the
cache is in memory, a cross-resource query is a local join — "services with no
endpoints" is a hash-join across two cached collections, not an API fan-out.

## Error handling

### Terminal safety

A panic hook that restores the terminal — leaves alternate screen, disables raw mode,
disables mouse capture — **before** printing the panic. Without this, any panic leaves
the user in a dead shell with no echo. This is implemented first, before any feature.

The same restoration runs on normal exit and on `SIGTERM`/`SIGINT`.

### Surfacing failures

- Transient errors appear as toasts in the footer.
- All errors accumulate in a scrollable error view with timestamps and context.
- Nothing writes to stderr while the alternate screen is active.
- Nothing is silently swallowed.

### Stale data

Watch reconnection is shown in the status bar (`⟳ reconnecting`) and the affected
view is visually marked. Presenting stale data as live is the worst failure mode for an
operations tool, so freshness state is always visible rather than implied.

### RBAC

Forbidden kinds are shown in the sidebar as unavailable with the reason, rather than
being hidden or erroring on click. Partial cluster access is the normal case, not an
error.

## Module layout

```
src/
  main.rs          entry, terminal setup/teardown, panic hook
  app/             event loop, state machine, command dispatch
  cluster/         kubeconfig, contexts, client, discovery
  store/           cache, watchers, subscriptions, columns
  query/           selectors, fuzzy, jq
  logs/            stream, aggregate, ringbuf, export
  ui/
    layout.rs      pane geometry, resizing
    hit.rs         hit-test registry
    theme.rs       colours, styles
    focus.rs       focus and keyboard routing
    views/         sidebar, table, detail, logs, palette
```

Files stay focused on one responsibility. A file growing large is treated as a signal
that its boundaries need revisiting.

## Testing

Development follows TDD. The layering makes most of the system testable without a
cluster:

| Layer | Method | Cluster needed |
|---|---|---|
| Store | Synthetic watch deltas in, asserted cache state out | No |
| Query | Pure functions; property tests for selector parsing | No |
| Logs | Fake streams; ring buffer and ordering assertions | No |
| Hit-testing | Pure function of registry + coordinates | No |
| Rendering | `ratatui::TestBackend`, assert on character buffer | No |
| Integration | Real watches, log streams, reconnects, RBAC | Yes (`kind`) |

Development and integration testing run against a local `kind` cluster. `kind` and
`kubectl` are not currently installed on the development machine and are a setup
prerequisite.

### Specific cases worth testing

- Watch `410 Gone` triggers relist and the cache converges.
- Deleting an object while its detail pane is open does not panic.
- Log follow-mode disengagement and re-engagement at buffer boundaries.
- Ring buffer wraparound preserves filter index correctness.
- Terminal restoration runs on panic.
- A context switch cancels all in-flight watches and log streams.

## Open questions for the implementation plan

- jq/JSONPath crate selection against the criteria above.
- Whether the reorder window for timestamp-sorted logs is fixed or adaptive.
- Config file format and location for persisted preferences (pane sizes, theme,
  default namespace).
