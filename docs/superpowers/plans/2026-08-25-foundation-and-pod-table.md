# kube — Plan 1: Foundation and First Vertical Slice

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a runnable TUI that connects to a Kubernetes cluster, watches pods, and renders them in a mouse-navigable table that stays live as the cluster changes.

**Architecture:** Three layers joined by channels. A `cluster` layer wraps kube-rs config and client construction. A `store` layer owns a watch-driven in-memory cache and is a pure state machine over watcher deltas. A `ui` layer renders immediate-mode with ratatui and resolves mouse coordinates through a per-frame hit-test registry. The UI thread never performs I/O and never awaits; all network work happens in tokio tasks that push `Event`s into a single channel.

**Tech Stack:** Rust (edition 2024), `kube` 4.2, `k8s-openapi` 0.28, `ratatui` 0.30, `crossterm` 0.29, `tokio` 1.x.

This is plan 1 of 4 for v1. Plans 2-4 (full browsing UI, logs, query) build on the foundation established here. Source spec: `docs/superpowers/specs/2026-08-25-kube-tui-design.md`.

## Global Constraints

These apply to every task. Every task's requirements implicitly include this section.

- **Rust edition 2024.** Minimum toolchain 1.85.
- **Exact dependency versions:** `kube = "4.2"` (features `runtime`, `client`, `derive`), `k8s-openapi = "0.28"` (feature `latest`), `ratatui = "0.30"`, `crossterm = "0.29"` (feature `event-stream`), `tokio = "1"` (feature `full`), `futures = "0.3"`, `serde_json = "1"`, `indexmap = "2"`, `chrono = "0.4"`, `anyhow = "1"`, `thiserror = "2"`.
- **The render closure passed to `term.draw` must be synchronous.** It must not perform I/O and must not acquire a lock. The event loop may `.await` on the event channel and on the store lock, but must never hold a lock across a draw. Reading a snapshot out of the store *before* calling `term.draw`, and drawing from that snapshot, is the intended pattern.
- **Never render on a fixed tick.** Rendering happens only after the event channel drains.
- **Never write to stdout/stderr while the alternate screen is active.** Use the error channel.
- **Every panic path must restore the terminal first.**
- **No `unwrap()` or `expect()` outside tests** except where a comment justifies the invariant.
- **Tests must not require a cluster** except in Task 11, which is explicitly gated.
- **TDD:** every task writes a failing test first and watches it fail before implementing.
- Run `cargo fmt` and `cargo clippy -- -D warnings` before every commit.

## Verified API Reference

These signatures were verified by compiling against the real crates on 2026-08-25. Do not substitute remembered APIs — several differ from older versions.

| Need | Verified API |
|---|---|
| Terminal setup | `ratatui::init() -> Terminal<...>`, `ratatui::restore()` |
| Frame area | `Frame::area()` — **not** `size()` |
| Table | `Table::new(rows, widths)`, `.header(Row)`, `.row_highlight_style(Style)` |
| Table state | `frame.render_stateful_widget(table, area, &mut TableState)` |
| Layout | `Layout::horizontal([Constraint; N]).split(area) -> Rc<[Rect]>` |
| Async input | `crossterm::event::EventStream::new()`, yields `Result<Event>` via `StreamExt::next` |
| Mouse fields | `MouseEvent { column: u16, row: u16, kind: MouseEventKind }` |
| Client | `Config::infer().await?`, `Client::try_from(cfg)?` |
| Kubeconfig | `Kubeconfig::read()`, `Kubeconfig::from_yaml(&str)`, `.contexts`, `.current_context` |
| Watcher | `kube::runtime::watcher::watcher(api, watcher::Config::default())` — module and fn share a name; call it as `watcher::watcher(..)` |
| **Watcher events** | `watcher::Event::{Apply, Delete, Init, InitApply, InitDone}` — **not** the old `Applied`/`Deleted`/`Restarted` |
| Dynamic API | `ApiResource::erase::<Pod>(&())`, `Api::all_with(client, &ar)`, `Api::namespaced_with(client, ns, &ar)` |
| GVK → resource | `GroupVersionKind::gvk("", "v1", "Pod")`, `ApiResource::from_gvk(&gvk)` |
| Fixtures | `DynamicObject::new("name", &ar).within("ns")`; set `.data` to `serde_json::Value` |
| Cache key | `ObjectRef::from_obj_with(&obj, ar.clone())` |
| Discovery | `Discovery::new(client).run().await?`, `group.recommended_resources()` |
| Log stream | `api.log_stream(name, &lp).await?` returns **futures-io** `AsyncBufRead`; use `futures::AsyncBufReadExt::lines()`, consume with `StreamExt::next` — **not** tokio's `AsyncBufReadExt`/`next_line` |
| Test rendering | `TestBackend::new(w,h)`, `Terminal::new(backend)`, `term.backend().buffer()`, index with `buf[(x, y)]` |

## File Structure

| File | Responsibility |
|---|---|
| `Cargo.toml` | Dependencies, pinned per Global Constraints |
| `src/lib.rs` | Library root; declares every module (integration tests and the binary both link against it) |
| `src/main.rs` | Binary entry point; wires terminal guard, event loop, store |
| `src/terminal.rs` | Terminal lifecycle: raw mode, alt screen, mouse capture, panic hook, restore guard |
| `src/app/mod.rs` | `App` state struct; owns store handle and UI state |
| `src/app/event.rs` | `Event` enum, channel plumbing, drain-and-coalesce logic |
| `src/cluster/mod.rs` | Re-exports |
| `src/cluster/config.rs` | Kubeconfig parsing, context listing, client construction |
| `src/store/mod.rs` | `ResourceStore`, subscription API |
| `src/store/cache.rs` | `KindCache` — pure state machine over watcher deltas |
| `src/store/watch.rs` | Tokio task that drives a watcher into a cache |
| `src/store/columns.rs` | Per-kind column registry and cell extraction |
| `src/ui/mod.rs` | Root render function |
| `src/ui/hit.rs` | Hit-test registry — pure coordinate resolution |
| `src/ui/theme.rs` | Colors and styles |
| `src/ui/views/table.rs` | Resource table widget |
| `src/ui/views/status.rs` | Status bar, watch health, toasts |
| `tests/integration_kind.rs` | Cluster-backed integration tests (gated) |

---

### Task 1: Project scaffold and terminal safety

The terminal guard comes first because every later task runs the binary. Without restoration on panic, a single crash leaves the developer in a dead shell with no echo — which makes every subsequent task painful to work on.

The design separates *policy* (when to restore) from *I/O* (how to restore) behind a trait, so restoration is testable without a real terminal.

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/terminal.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `trait TerminalControl { fn restore(&self) -> std::io::Result<()>; }`
  - `struct RealTerminal;` implementing `TerminalControl`
  - `struct TerminalGuard<T: TerminalControl> { control: T, active: bool }` with `TerminalGuard::new(control) -> Self`, `fn disarm(&mut self)`, and a `Drop` impl calling `restore` exactly once
  - `fn install_panic_hook()`

- [ ] **Step 1: Create the Cargo project and pin dependencies**

```bash
cargo init --name kube-tui
```

Replace `Cargo.toml` with:

```toml
[package]
name = "kube-tui"
version = "0.1.0"
edition = "2024"

[lib]
name = "kube_tui"
path = "src/lib.rs"

[[bin]]
name = "kube"
path = "src/main.rs"

[dependencies]
kube = { version = "4.2", features = ["runtime", "client", "derive"] }
k8s-openapi = { version = "0.28", features = ["latest"] }
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
tokio = { version = "1", features = ["full"] }
futures = "0.3"
serde_json = "1"
indexmap = "2"
chrono = "0.4"
anyhow = "1"
thiserror = "2"
```

Note the package is `kube-tui` because the crate name `kube` collides with the `kube` dependency; the produced binary is still named `kube`.

- [ ] **Step 2: Write the failing test**

Create `src/terminal.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
            assert_eq!(calls.load(Ordering::SeqCst), 0, "must not restore while alive");
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
        assert_eq!(calls.load(Ordering::SeqCst), 0, "disarmed guard must not restore");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib terminal`
Expected: FAIL — `cannot find type TerminalGuard in this scope`.

- [ ] **Step 4: Write the minimal implementation**

Prepend to `src/terminal.rs`:

```rust
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
        use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
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
        Self { control, active: true }
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
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib terminal`
Expected: PASS — 2 tests.

- [ ] **Step 6: Wire up main.rs**

Create `src/lib.rs`:

```rust
pub mod terminal;
```

The binary links against this library rather than re-declaring modules, so each
module is compiled exactly once and integration tests (Task 11) can reach them.

Replace `src/main.rs`:

```rust
use kube_tui::terminal::{install_panic_hook, RealTerminal, TerminalGuard};

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
```

- [ ] **Step 7: Verify it runs and leaves the terminal usable**

Run: `cargo run`
Expected: a message appears on an alternate screen; pressing a key exits and the shell is fully functional (echo works, prompt normal).

Then verify the panic path. Temporarily add `panic!("test");` immediately after the `term.draw` call, run `cargo run`, and confirm the shell is still usable afterwards. Remove the line.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/terminal.rs
git commit -m "feat: project scaffold with panic-safe terminal restoration"
```

---

### Task 2: Event types and drain-and-coalesce loop

The performance property "a watch storm of 10,000 deltas produces one repaint" lives here. The coalescing decision is extracted as a pure function so it can be tested without a runtime.

**Files:**
- Create: `src/app/mod.rs`
- Create: `src/app/event.rs`
- Modify: `src/lib.rs` (add `pub mod app;`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `enum AppEvent { Input(crossterm::event::Event), StoreChanged { gvk: GroupVersionKind }, WatchStatus { gvk: GroupVersionKind, status: WatchStatus }, Error(String), Quit }`
  - `enum WatchStatus { Initialising, Synced, Reconnecting, Failed }`
  - `struct Coalesced { pub inputs: Vec<crossterm::event::Event>, pub store_dirty: bool, pub status_changes: Vec<(GroupVersionKind, WatchStatus)>, pub errors: Vec<String>, pub quit: bool }`
  - `fn coalesce(events: Vec<AppEvent>) -> Coalesced`

- [ ] **Step 1: Write the failing test**

Create `src/app/event.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event as CtEvent, KeyCode, KeyEvent, KeyModifiers, KeyEventKind, KeyEventState};
    use kube::api::GroupVersionKind;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    fn key(c: char) -> CtEvent {
        CtEvent::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn many_store_changes_collapse_to_one_dirty_flag() {
        let events: Vec<AppEvent> = (0..10_000)
            .map(|_| AppEvent::StoreChanged { gvk: pod_gvk() })
            .collect();
        let out = coalesce(events);
        assert!(out.store_dirty, "store changes must mark the view dirty");
        assert!(out.inputs.is_empty());
        assert!(!out.quit);
    }

    #[test]
    fn input_events_are_preserved_in_order_and_never_dropped() {
        let events = vec![
            AppEvent::Input(key('a')),
            AppEvent::StoreChanged { gvk: pod_gvk() },
            AppEvent::Input(key('b')),
        ];
        let out = coalesce(events);
        assert_eq!(out.inputs.len(), 2, "input must never be coalesced away");
        assert_eq!(out.inputs[0], key('a'));
        assert_eq!(out.inputs[1], key('b'));
        assert!(out.store_dirty);
    }

    #[test]
    fn quit_is_sticky() {
        let out = coalesce(vec![
            AppEvent::Quit,
            AppEvent::StoreChanged { gvk: pod_gvk() },
        ]);
        assert!(out.quit);
    }

    #[test]
    fn latest_status_per_gvk_wins() {
        let out = coalesce(vec![
            AppEvent::WatchStatus { gvk: pod_gvk(), status: WatchStatus::Initialising },
            AppEvent::WatchStatus { gvk: pod_gvk(), status: WatchStatus::Synced },
        ]);
        assert_eq!(out.status_changes.len(), 1, "only the newest status per kind matters");
        assert_eq!(out.status_changes[0].1, WatchStatus::Synced);
    }

    #[test]
    fn errors_are_all_retained() {
        let out = coalesce(vec![
            AppEvent::Error("first".into()),
            AppEvent::Error("second".into()),
        ]);
        assert_eq!(out.errors, vec!["first".to_string(), "second".to_string()],
                   "errors must never be silently swallowed");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib event`
Expected: FAIL — `cannot find function coalesce`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/app/event.rs`:

```rust
use crossterm::event::Event as CtEvent;
use indexmap::IndexMap;
use kube::api::GroupVersionKind;

/// Health of a single kind's watch. Shown in the status bar so stale data is
/// never presented as live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStatus {
    Initialising,
    Synced,
    Reconnecting,
    Failed,
}

/// Everything that can wake the event loop.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Input(CtEvent),
    StoreChanged { gvk: GroupVersionKind },
    WatchStatus { gvk: GroupVersionKind, status: WatchStatus },
    Error(String),
    Quit,
}

/// The result of collapsing a batch of events into a single render's worth of work.
#[derive(Debug, Default)]
pub struct Coalesced {
    pub inputs: Vec<CtEvent>,
    pub store_dirty: bool,
    pub status_changes: Vec<(GroupVersionKind, WatchStatus)>,
    pub errors: Vec<String>,
    pub quit: bool,
}

/// Collapse a drained batch into one render's work.
///
/// Store changes coalesce to a single dirty flag: 10,000 deltas cost one repaint.
/// Input is never coalesced — dropping keystrokes is always wrong. Errors are
/// never dropped. Only the newest status per kind is kept.
pub fn coalesce(events: Vec<AppEvent>) -> Coalesced {
    let mut out = Coalesced::default();
    let mut statuses: IndexMap<GroupVersionKind, WatchStatus> = IndexMap::new();

    for event in events {
        match event {
            AppEvent::Input(e) => out.inputs.push(e),
            AppEvent::StoreChanged { .. } => out.store_dirty = true,
            AppEvent::WatchStatus { gvk, status } => {
                statuses.insert(gvk, status);
            }
            AppEvent::Error(e) => out.errors.push(e),
            AppEvent::Quit => out.quit = true,
        }
    }

    out.status_changes = statuses.into_iter().collect();
    out
}
```

Create `src/app/mod.rs`:

```rust
pub mod event;
```

Add `pub mod app;` to `src/lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib event`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/app src/main.rs
git commit -m "feat: event types with drain-and-coalesce for burst absorption"
```

---

### Task 3: Cluster layer — contexts and client

**Files:**
- Create: `src/cluster/mod.rs`
- Create: `src/cluster/config.rs`
- Modify: `src/lib.rs` (add `pub mod cluster;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `struct ContextInfo { pub name: String, pub cluster: String, pub namespace: Option<String>, pub is_current: bool }`
  - `fn contexts_from_yaml(yaml: &str) -> anyhow::Result<Vec<ContextInfo>>`
  - `fn load_contexts() -> anyhow::Result<Vec<ContextInfo>>`
  - `async fn connect() -> anyhow::Result<kube::Client>`

- [ ] **Step 1: Write the failing test**

Create `src/cluster/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
current-context: prod-eu
clusters:
- name: prod-cluster
  cluster:
    server: https://prod.example.com
- name: dev-cluster
  cluster:
    server: https://dev.example.com
contexts:
- name: prod-eu
  context:
    cluster: prod-cluster
    user: prod-user
    namespace: payments
- name: dev
  context:
    cluster: dev-cluster
    user: dev-user
- name: empty-ns
  context:
    cluster: dev-cluster
    user: dev-user
    namespace: ""
users: []
"#;

    #[test]
    fn parses_all_contexts() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert_eq!(ctxs.len(), 3);
        assert_eq!(ctxs[0].name, "prod-eu");
        assert_eq!(ctxs[1].name, "dev");
        assert_eq!(ctxs[2].name, "empty-ns");
    }

    #[test]
    fn marks_the_current_context() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert!(ctxs[0].is_current, "prod-eu is current-context");
        assert!(!ctxs[1].is_current);
    }

    #[test]
    fn captures_cluster_and_default_namespace() {
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        assert_eq!(ctxs[0].cluster, "prod-cluster");
        assert_eq!(ctxs[0].namespace.as_deref(), Some("payments"));
        assert_eq!(ctxs[1].namespace, None, "absent namespace stays None, not empty string");
    }

    #[test]
    fn an_explicitly_empty_namespace_becomes_none() {
        // An explicit `namespace: ""` deserializes to Some("") — the filter in
        // flatten() is what normalises it. Without this case that filter is
        // unguarded and can be deleted without failing any test.
        let ctxs = contexts_from_yaml(SAMPLE).unwrap();
        let empty = ctxs.iter().find(|c| c.name == "empty-ns").expect("empty-ns context");
        assert_eq!(empty.namespace, None, "empty string must normalise to None, not Some(\"\")");
    }

    #[test]
    fn malformed_yaml_is_an_error_not_a_panic() {
        assert!(contexts_from_yaml("this: is: not: valid: kubeconfig").is_err());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib cluster`
Expected: FAIL — `cannot find function contexts_from_yaml`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/cluster/config.rs`:

```rust
use anyhow::Context as _;
use kube::config::Kubeconfig;

/// A selectable kubeconfig context, flattened for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub namespace: Option<String>,
    pub is_current: bool,
}

fn flatten(kc: Kubeconfig) -> Vec<ContextInfo> {
    let current = kc.current_context.clone().unwrap_or_default();
    kc.contexts
        .into_iter()
        .map(|named| {
            let ctx = named.context;
            ContextInfo {
                is_current: named.name == current,
                name: named.name,
                cluster: ctx.as_ref().map(|c| c.cluster.clone()).unwrap_or_default(),
                namespace: ctx.and_then(|c| c.namespace).filter(|n| !n.is_empty()),
            }
        })
        .collect()
}

/// Parse contexts from a kubeconfig string. Separated from file loading so
/// context handling is testable without touching the filesystem.
pub fn contexts_from_yaml(yaml: &str) -> anyhow::Result<Vec<ContextInfo>> {
    let kc = Kubeconfig::from_yaml(yaml).context("parsing kubeconfig")?;
    Ok(flatten(kc))
}

/// Load contexts from the standard kubeconfig location(s).
pub fn load_contexts() -> anyhow::Result<Vec<ContextInfo>> {
    let kc = Kubeconfig::read().context("reading kubeconfig")?;
    Ok(flatten(kc))
}

/// Build a client from the current context.
pub async fn connect() -> anyhow::Result<kube::Client> {
    let cfg = kube::Config::infer()
        .await
        .context("inferring cluster config — is a kubeconfig present?")?;
    kube::Client::try_from(cfg).context("building Kubernetes client")
}
```

Create `src/cluster/mod.rs`:

```rust
pub mod config;
pub use config::{connect, contexts_from_yaml, load_contexts, ContextInfo};
```

Add `pub mod cluster;` to `src/lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib cluster`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/cluster src/main.rs
git commit -m "feat: kubeconfig context parsing and client construction"
```

---

### Task 4: Store cache as a pure state machine

This is the correctness core. The subtle part is the **Init sequence**: kube-rs emits `Init`, then zero or more `InitApply`, then `InitDone` when a watch (re)synchronises. Objects deleted while disconnected appear in neither `InitApply` nor any `Delete`. Naively applying `InitApply` into the live map leaves those objects visible forever — a ghost-row bug that only shows up after a reconnect.

The fix is to accumulate init objects into a staging buffer and swap atomically on `InitDone`.

**Files:**
- Create: `src/store/mod.rs`
- Create: `src/store/cache.rs`
- Modify: `src/lib.rs` (add `pub mod store;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `struct KindCache` with `KindCache::new(ApiResource) -> Self`, `fn apply(&mut self, event: watcher::Event<DynamicObject>)`, `fn objects(&self) -> Vec<Arc<DynamicObject>>`, `fn len(&self) -> usize`, `fn is_empty(&self) -> bool`
  - `type ObjKey = (Option<String>, String)` — (namespace, name)
  - `fn key_of(obj: &DynamicObject) -> ObjKey`

- [ ] **Step 1: Write the failing test**

Create `src/store/cache.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;
    use kube::runtime::watcher;

    fn pod_ar() -> ApiResource {
        ApiResource::erase::<Pod>(&())
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &pod_ar()).within("default")
    }

    fn names(cache: &KindCache) -> Vec<String> {
        let mut n: Vec<String> = cache.objects().iter().map(|o| o.name_any()).collect();
        n.sort();
        n
    }

    #[test]
    fn apply_inserts_an_object() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        assert_eq!(names(&c), vec!["a"]);
    }

    #[test]
    fn apply_twice_updates_rather_than_duplicates() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("a")));
        assert_eq!(c.len(), 1, "same namespace+name must replace, not duplicate");
    }

    #[test]
    fn delete_removes_an_object() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("b")));
        c.apply(watcher::Event::Delete(pod("a")));
        assert_eq!(names(&c), vec!["b"]);
    }

    #[test]
    fn deleting_an_unknown_object_is_a_no_op() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Delete(pod("ghost")));
        assert!(c.is_empty());
    }

    #[test]
    fn objects_stay_visible_during_a_resync() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        assert_eq!(names(&c), vec!["a"], "must not blank the view mid-resync");
    }

    #[test]
    fn resync_drops_objects_deleted_while_disconnected() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Apply(pod("stale")));

        // Reconnect: the server reports only "a" still exists.
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        c.apply(watcher::Event::InitDone);

        assert_eq!(names(&c), vec!["a"], "'stale' was deleted while disconnected");
    }

    #[test]
    fn resync_adds_objects_created_while_disconnected() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitApply(pod("a")));
        c.apply(watcher::Event::InitApply(pod("new")));
        c.apply(watcher::Event::InitDone);
        assert_eq!(names(&c), vec!["a", "new"]);
    }

    #[test]
    fn empty_resync_clears_everything() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(pod("a")));
        c.apply(watcher::Event::Init);
        c.apply(watcher::Event::InitDone);
        assert!(c.is_empty(), "server reported no objects, so cache must be empty");
    }

    #[test]
    fn objects_in_different_namespaces_do_not_collide() {
        let mut c = KindCache::new(pod_ar());
        c.apply(watcher::Event::Apply(DynamicObject::new("a", &pod_ar()).within("ns1")));
        c.apply(watcher::Event::Apply(DynamicObject::new("a", &pod_ar()).within("ns2")));
        assert_eq!(c.len(), 2, "namespace is part of identity");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib cache`
Expected: FAIL — `cannot find type KindCache`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/store/cache.rs`:

```rust
use indexmap::IndexMap;
use kube::api::{ApiResource, DynamicObject, ResourceExt};
use kube::runtime::watcher;
use std::sync::Arc;

/// Identity of an object within a kind: (namespace, name).
pub type ObjKey = (Option<String>, String);

pub fn key_of(obj: &DynamicObject) -> ObjKey {
    (obj.namespace(), obj.name_any())
}

/// In-memory cache of one kind, driven by watcher deltas.
///
/// `Arc` lets the UI clone pointers rather than objects; rendering 5,000 rows
/// copies 5,000 pointers. `IndexMap` gives stable iteration order for rendering
/// plus O(1) keyed lookup.
pub struct KindCache {
    resource: ApiResource,
    objects: IndexMap<ObjKey, Arc<DynamicObject>>,
    /// Staging buffer for an in-progress resync. `Some` between Init and InitDone.
    init_buffer: Option<IndexMap<ObjKey, Arc<DynamicObject>>>,
}

impl KindCache {
    pub fn new(resource: ApiResource) -> Self {
        Self { resource, objects: IndexMap::new(), init_buffer: None }
    }

    pub fn resource(&self) -> &ApiResource {
        &self.resource
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn objects(&self) -> Vec<Arc<DynamicObject>> {
        self.objects.values().cloned().collect()
    }

    pub fn get(&self, key: &ObjKey) -> Option<&Arc<DynamicObject>> {
        self.objects.get(key)
    }

    /// Fold one watcher delta into the cache.
    ///
    /// Init/InitApply/InitDone form a resync: objects accumulate in a staging
    /// buffer and replace the live map atomically on InitDone. Applying them
    /// directly would leave objects that were deleted while disconnected
    /// visible forever, since no Delete is ever emitted for them.
    pub fn apply(&mut self, event: watcher::Event<DynamicObject>) {
        match event {
            watcher::Event::Apply(obj) => {
                self.objects.insert(key_of(&obj), Arc::new(obj));
            }
            watcher::Event::Delete(obj) => {
                self.objects.shift_remove(&key_of(&obj));
            }
            watcher::Event::Init => {
                self.init_buffer = Some(IndexMap::new());
            }
            watcher::Event::InitApply(obj) => {
                if let Some(buf) = self.init_buffer.as_mut() {
                    buf.insert(key_of(&obj), Arc::new(obj));
                } else {
                    // InitApply without Init: tolerate rather than lose data.
                    self.objects.insert(key_of(&obj), Arc::new(obj));
                }
            }
            watcher::Event::InitDone => {
                if let Some(buf) = self.init_buffer.take() {
                    self.objects = buf;
                }
            }
        }
    }
}
```

Create `src/store/mod.rs`:

```rust
pub mod cache;
pub use cache::{key_of, KindCache, ObjKey};
```

Add `pub mod store;` to `src/lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib cache`
Expected: PASS — 9 tests.

- [ ] **Step 5: Commit**

```bash
git add src/store src/main.rs
git commit -m "feat: kind cache with atomic resync semantics"
```

---

### Task 5: ResourceStore and the watch task

**Files:**
- Create: `src/store/watch.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: `KindCache` (Task 4), `AppEvent`/`WatchStatus` (Task 2).
- Produces:
  - `struct ResourceStore { kinds: HashMap<GroupVersionKind, KindCache>, statuses: HashMap<GroupVersionKind, WatchStatus> }`
  - `ResourceStore::new() -> Self`, `fn apply(&mut self, gvk: &GroupVersionKind, resource: &ApiResource, event: watcher::Event<DynamicObject>)`, `fn objects(&self, gvk: &GroupVersionKind) -> Vec<Arc<DynamicObject>>`, `fn set_status(&mut self, gvk: GroupVersionKind, s: WatchStatus)`, `fn status(&self, gvk: &GroupVersionKind) -> WatchStatus`
  - `type SharedStore = Arc<RwLock<ResourceStore>>`
  - `fn spawn_watch(client: Client, ar: ApiResource, namespace: Option<String>, store: SharedStore, tx: UnboundedSender<AppEvent>) -> JoinHandle<()>`

- [ ] **Step 1: Write the failing test**

Create `src/store/watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
    use kube::runtime::watcher;

    fn pod_gvk() -> GroupVersionKind {
        GroupVersionKind::gvk("", "v1", "Pod")
    }

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("default")
    }

    #[test]
    fn store_routes_events_to_the_right_kind() {
        let mut store = ResourceStore::new();
        let ar = ApiResource::erase::<Pod>(&());
        store.apply(&pod_gvk(), &ar, watcher::Event::Apply(pod("a")));
        assert_eq!(store.objects(&pod_gvk()).len(), 1);
    }

    #[test]
    fn unknown_kind_returns_empty_not_panic() {
        let store = ResourceStore::new();
        let unknown = GroupVersionKind::gvk("apps", "v1", "Deployment");
        assert!(store.objects(&unknown).is_empty());
    }

    #[test]
    fn status_defaults_to_initialising() {
        let store = ResourceStore::new();
        assert_eq!(store.status(&pod_gvk()), WatchStatus::Initialising);
    }

    #[test]
    fn status_is_recorded_per_kind() {
        let mut store = ResourceStore::new();
        store.set_status(pod_gvk(), WatchStatus::Synced);
        assert_eq!(store.status(&pod_gvk()), WatchStatus::Synced);
        let other = GroupVersionKind::gvk("apps", "v1", "Deployment");
        assert_eq!(store.status(&other), WatchStatus::Initialising,
                   "one kind's health must not mask another's");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib watch`
Expected: FAIL — `cannot find type ResourceStore`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/store/watch.rs`:

```rust
use crate::app::event::{AppEvent, WatchStatus};
use crate::store::cache::KindCache;
use futures::StreamExt;
use kube::api::{ApiResource, DynamicObject, GroupVersionKind};
use kube::runtime::watcher;
use kube::{Api, Client};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

/// All cached kinds plus their watch health.
pub struct ResourceStore {
    kinds: HashMap<GroupVersionKind, KindCache>,
    statuses: HashMap<GroupVersionKind, WatchStatus>,
}

impl Default for ResourceStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceStore {
    pub fn new() -> Self {
        Self { kinds: HashMap::new(), statuses: HashMap::new() }
    }

    pub fn apply(
        &mut self,
        gvk: &GroupVersionKind,
        resource: &ApiResource,
        event: watcher::Event<DynamicObject>,
    ) {
        self.kinds
            .entry(gvk.clone())
            .or_insert_with(|| KindCache::new(resource.clone()))
            .apply(event);
    }

    pub fn objects(&self, gvk: &GroupVersionKind) -> Vec<Arc<DynamicObject>> {
        self.kinds.get(gvk).map(|c| c.objects()).unwrap_or_default()
    }

    pub fn set_status(&mut self, gvk: GroupVersionKind, status: WatchStatus) {
        self.statuses.insert(gvk, status);
    }

    pub fn status(&self, gvk: &GroupVersionKind) -> WatchStatus {
        self.statuses.get(gvk).copied().unwrap_or(WatchStatus::Initialising)
    }
}

pub type SharedStore = Arc<RwLock<ResourceStore>>;

/// Drive a watcher for one kind into the store, emitting an event after each delta.
///
/// `watcher` already handles relist-on-410-Gone internally, so this loop only
/// has to translate errors into visible status rather than reconnect by hand.
pub fn spawn_watch(
    client: Client,
    ar: ApiResource,
    namespace: Option<String>,
    store: SharedStore,
    tx: UnboundedSender<AppEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let gvk = GroupVersionKind::gvk(&ar.group, &ar.version, &ar.kind);
        let api: Api<DynamicObject> = match namespace.as_deref() {
            Some(ns) => Api::namespaced_with(client, ns, &ar),
            None => Api::all_with(client, &ar),
        };

        let stream = watcher::watcher(api, watcher::Config::default());
        futures::pin_mut!(stream);

        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    let synced = matches!(event, watcher::Event::InitDone | watcher::Event::Apply(_));
                    store.write().await.apply(&gvk, &ar, event);
                    if synced {
                        store.write().await.set_status(gvk.clone(), WatchStatus::Synced);
                        let _ = tx.send(AppEvent::WatchStatus {
                            gvk: gvk.clone(),
                            status: WatchStatus::Synced,
                        });
                    }
                    let _ = tx.send(AppEvent::StoreChanged { gvk: gvk.clone() });
                }
                Err(e) => {
                    store.write().await.set_status(gvk.clone(), WatchStatus::Reconnecting);
                    let _ = tx.send(AppEvent::WatchStatus {
                        gvk: gvk.clone(),
                        status: WatchStatus::Reconnecting,
                    });
                    let _ = tx.send(AppEvent::Error(format!("watch {}: {e}", ar.kind)));
                }
            }
        }
    })
}
```

Update `src/store/mod.rs`:

```rust
pub mod cache;
pub mod watch;
pub use cache::{key_of, KindCache, ObjKey};
pub use watch::{spawn_watch, ResourceStore, SharedStore};
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib watch`
Expected: PASS — 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src/store
git commit -m "feat: resource store and watch task with per-kind health"
```

---

### Task 6: Column registry and cell extraction

Pods need derived columns that do not exist as plain fields: READY is a ratio computed from `containerStatuses`, RESTARTS is a sum, and AGE is a duration formatted from a timestamp. These are pure functions over JSON and are where most display bugs live, so they get thorough tests.

**Files:**
- Create: `src/store/columns.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `struct Column { pub header: &'static str, pub width: Constraint, pub extract: fn(&DynamicObject) -> String }`
  - `fn columns_for(gvk: &GroupVersionKind) -> Vec<Column>`
  - `fn pod_ready(obj: &DynamicObject) -> String`
  - `fn pod_restarts(obj: &DynamicObject) -> String`
  - `fn pod_phase(obj: &DynamicObject) -> String`
  - `fn format_age(created: &str, now: DateTime<Utc>) -> String`

`chrono = "0.4"` is already in `Cargo.toml` from Task 1; no dependency change is needed here.

- [ ] **Step 1: Write the failing test**

Create `src/store/columns.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;

    fn pod_with(status: serde_json::Value) -> DynamicObject {
        let mut o = DynamicObject::new("p", &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({ "status": status });
        o
    }

    #[test]
    fn ready_counts_ready_containers() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [
                {"ready": true, "restartCount": 0},
                {"ready": false, "restartCount": 0}
            ]
        }));
        assert_eq!(pod_ready(&o), "1/2");
    }

    #[test]
    fn ready_handles_all_ready() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [{"ready": true, "restartCount": 0}]
        }));
        assert_eq!(pod_ready(&o), "1/1");
    }

    #[test]
    fn ready_on_a_pending_pod_with_no_statuses_is_zero_of_zero() {
        let o = pod_with(serde_json::json!({"phase": "Pending"}));
        assert_eq!(pod_ready(&o), "0/0", "a scheduled-but-not-started pod must not panic");
    }

    #[test]
    fn restarts_sums_across_containers() {
        let o = pod_with(serde_json::json!({
            "containerStatuses": [
                {"ready": true, "restartCount": 3},
                {"ready": true, "restartCount": 4}
            ]
        }));
        assert_eq!(pod_restarts(&o), "7");
    }

    #[test]
    fn phase_falls_back_to_unknown_when_absent() {
        let o = pod_with(serde_json::json!({}));
        assert_eq!(pod_phase(&o), "Unknown");
    }

    #[test]
    fn phase_reads_the_status_phase() {
        let o = pod_with(serde_json::json!({"phase": "Running"}));
        assert_eq!(pod_phase(&o), "Running");
    }

    #[test]
    fn age_formats_compactly_by_magnitude() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap();
        assert_eq!(format_age("2026-08-25T11:59:30Z", now), "30s");
        assert_eq!(format_age("2026-08-25T11:45:00Z", now), "15m");
        assert_eq!(format_age("2026-08-25T08:00:00Z", now), "4h");
        assert_eq!(format_age("2026-08-21T12:00:00Z", now), "4d");
    }

    #[test]
    fn age_of_an_unparseable_timestamp_is_unknown() {
        let now = chrono::Utc.with_ymd_and_hms(2026, 8, 25, 12, 0, 0).unwrap();
        assert_eq!(format_age("not-a-timestamp", now), "?");
    }

    #[test]
    fn pod_columns_are_the_expected_five() {
        let cols = columns_for(&GroupVersionKind::gvk("", "v1", "Pod"));
        let headers: Vec<&str> = cols.iter().map(|c| c.header).collect();
        assert_eq!(headers, vec!["NAME", "READY", "STATUS", "RESTARTS", "AGE"]);
    }

    #[test]
    fn unknown_kind_falls_back_to_name_and_age() {
        let cols = columns_for(&GroupVersionKind::gvk("example.com", "v1", "Widget"));
        let headers: Vec<&str> = cols.iter().map(|c| c.header).collect();
        assert_eq!(headers, vec!["NAME", "AGE"], "unknown kinds still render something useful");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib columns`
Expected: FAIL — `cannot find function pod_ready`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/store/columns.rs`:

```rust
use chrono::{DateTime, Utc};
use kube::api::{DynamicObject, GroupVersionKind, ResourceExt};
use ratatui::layout::Constraint;

/// One table column: a header plus a pure extraction from an object.
pub struct Column {
    pub header: &'static str,
    pub width: Constraint,
    pub extract: fn(&DynamicObject) -> String,
}

fn container_statuses(obj: &DynamicObject) -> &[serde_json::Value] {
    obj.data
        .get("status")
        .and_then(|s| s.get("containerStatuses"))
        .and_then(|c| c.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
}

/// "ready/total" across containers. A pod that has been scheduled but whose
/// containers have not started reports no statuses at all, so this must not
/// assume the array exists.
pub fn pod_ready(obj: &DynamicObject) -> String {
    let statuses = container_statuses(obj);
    let ready = statuses
        .iter()
        .filter(|c| c.get("ready").and_then(|r| r.as_bool()).unwrap_or(false))
        .count();
    format!("{}/{}", ready, statuses.len())
}

pub fn pod_restarts(obj: &DynamicObject) -> String {
    let total: i64 = container_statuses(obj)
        .iter()
        .filter_map(|c| c.get("restartCount").and_then(|r| r.as_i64()))
        .sum();
    total.to_string()
}

pub fn pod_phase(obj: &DynamicObject) -> String {
    obj.data
        .get("status")
        .and_then(|s| s.get("phase"))
        .and_then(|p| p.as_str())
        .unwrap_or("Unknown")
        .to_string()
}

/// Compact age, matching kubectl's convention of one significant unit.
pub fn format_age(created: &str, now: DateTime<Utc>) -> String {
    let Ok(ts) = created.parse::<DateTime<Utc>>() else {
        return "?".to_string();
    };
    let secs = (now - ts).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

fn extract_name(obj: &DynamicObject) -> String {
    obj.name_any()
}

fn extract_age(obj: &DynamicObject) -> String {
    match obj.metadata.creation_timestamp.as_ref() {
        Some(t) => format_age(&t.0.to_rfc3339(), Utc::now()),
        None => "?".to_string(),
    }
}

/// Columns for a kind. Kinds without an entry fall back to name and age, which
/// is always available via metadata — so CRDs render usefully with no per-kind code.
pub fn columns_for(gvk: &GroupVersionKind) -> Vec<Column> {
    if gvk.group.is_empty() && gvk.kind == "Pod" {
        return vec![
            Column { header: "NAME", width: Constraint::Fill(2), extract: extract_name },
            Column { header: "READY", width: Constraint::Length(7), extract: pod_ready },
            Column { header: "STATUS", width: Constraint::Length(14), extract: pod_phase },
            Column { header: "RESTARTS", width: Constraint::Length(9), extract: pod_restarts },
            Column { header: "AGE", width: Constraint::Length(6), extract: extract_age },
        ];
    }
    vec![
        Column { header: "NAME", width: Constraint::Fill(1), extract: extract_name },
        Column { header: "AGE", width: Constraint::Length(6), extract: extract_age },
    ]
}
```

Add `pub mod columns;` to `src/store/mod.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib columns`
Expected: PASS — 10 tests.

- [ ] **Step 5: Commit**

```bash
git add src/store Cargo.toml Cargo.lock
git commit -m "feat: column registry with pod-specific derived cells"
```

---

### Task 7: Hit-test registry

Ratatui is immediate-mode, so a click at `(col, row)` carries no meaning by itself. The registry is rebuilt every frame; mouse events resolve against it in reverse z-order so overlays win over what is beneath them. It is a pure function, so it is fully testable without a terminal.

**Files:**
- Create: `src/ui/mod.rs`
- Create: `src/ui/hit.rs`
- Modify: `src/lib.rs` (add `pub mod ui;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `enum HitTarget { TableRow(usize), ColumnHeader(usize), StatusBar, Background }`
  - `struct HitRegistry` with `new() -> Self`, `fn clear(&mut self)`, `fn push(&mut self, area: Rect, z: u8, target: HitTarget)`, `fn hit(&self, col: u16, row: u16) -> Option<&HitTarget>`

`HitTarget` grows in plan 2 to cover sidebar entries, tabs, and pane borders.

- [ ] **Step 1: Write the failing test**

Create `src/ui/hit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn a_click_inside_a_zone_hits_it() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        assert_eq!(r.hit(5, 2), Some(&HitTarget::TableRow(3)));
    }

    #[test]
    fn a_click_outside_every_zone_misses() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        assert_eq!(r.hit(50, 50), None);
    }

    #[test]
    fn zone_boundaries_are_inclusive_at_the_start_exclusive_at_the_end() {
        let mut r = HitRegistry::new();
        r.push(rect(2, 2, 3, 3), 0, HitTarget::TableRow(0));
        assert!(r.hit(2, 2).is_some(), "top-left corner is inside");
        assert!(r.hit(4, 4).is_some(), "bottom-right cell is inside");
        assert!(r.hit(5, 4).is_none(), "one past the right edge is outside");
        assert!(r.hit(4, 5).is_none(), "one past the bottom edge is outside");
        assert!(r.hit(1, 2).is_none(), "one before the left edge is outside");
    }

    #[test]
    fn a_higher_z_zone_wins_when_zones_overlap() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 20, 10), 0, HitTarget::Background);
        r.push(rect(5, 5, 5, 3), 1, HitTarget::TableRow(7));
        assert_eq!(r.hit(6, 6), Some(&HitTarget::TableRow(7)),
                   "an overlay must capture clicks over the pane beneath it");
    }

    #[test]
    fn later_registration_wins_at_equal_z() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 10), 0, HitTarget::TableRow(1));
        r.push(rect(0, 0, 10, 10), 0, HitTarget::TableRow(2));
        assert_eq!(r.hit(1, 1), Some(&HitTarget::TableRow(2)),
                   "drawn later means drawn on top");
    }

    #[test]
    fn clear_empties_the_registry_for_the_next_frame() {
        let mut r = HitRegistry::new();
        r.push(rect(0, 0, 10, 5), 0, HitTarget::TableRow(3));
        r.clear();
        assert_eq!(r.hit(5, 2), None, "stale zones must not survive a re-render");
    }

    #[test]
    fn zero_sized_zones_never_hit() {
        let mut r = HitRegistry::new();
        r.push(rect(3, 3, 0, 0), 0, HitTarget::TableRow(1));
        assert_eq!(r.hit(3, 3), None);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib hit`
Expected: FAIL — `cannot find type HitRegistry`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/ui/hit.rs`:

```rust
use ratatui::layout::Rect;

/// What a screen region means when clicked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    TableRow(usize),
    ColumnHeader(usize),
    StatusBar,
    Background,
}

/// Maps screen coordinates back to meaning.
///
/// Ratatui draws into a buffer and keeps no widget tree, so this registry is
/// rebuilt every frame as widgets draw. Resolution walks in reverse so that
/// higher z-index — and, at equal z, later-drawn — zones win.
pub struct HitRegistry {
    zones: Vec<(Rect, u8, HitTarget)>,
}

impl Default for HitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HitRegistry {
    pub fn new() -> Self {
        Self { zones: Vec::new() }
    }

    /// Drop all zones. Called at the start of every frame.
    pub fn clear(&mut self) {
        self.zones.clear();
    }

    pub fn push(&mut self, area: Rect, z: u8, target: HitTarget) {
        self.zones.push((area, z, target));
    }

    pub fn hit(&self, col: u16, row: u16) -> Option<&HitTarget> {
        self.zones
            .iter()
            .filter(|(area, _, _)| {
                area.width > 0
                    && area.height > 0
                    && col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
            })
            .max_by_key(|(_, z, _)| *z)
            .map(|(_, _, target)| target)
    }
}
```

Note: `max_by_key` returns the **last** maximum, which gives later-drawn-wins at equal z for free.

Create `src/ui/mod.rs`:

```rust
pub mod hit;
pub use hit::{HitRegistry, HitTarget};
```

Add `pub mod ui;` to `src/lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib hit`
Expected: PASS — 7 tests.

- [ ] **Step 5: Commit**

```bash
git add src/ui src/main.rs
git commit -m "feat: per-frame hit-test registry for mouse targeting"
```

---

### Task 8: Table view rendering

**Files:**
- Create: `src/ui/theme.rs`
- Create: `src/ui/views/mod.rs`
- Create: `src/ui/views/table.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Consumes: `Column`/`columns_for` (Task 6), `HitRegistry`/`HitTarget` (Task 7).
- Produces:
  - `struct TableView { pub state: TableState, pub scroll_offset: usize }`
  - `TableView::new() -> Self`
  - `fn render_table(f: &mut Frame, area: Rect, objects: &[Arc<DynamicObject>], gvk: &GroupVersionKind, view: &mut TableView, hits: &mut HitRegistry)`
  - `fn phase_style(phase: &str) -> Style`

- [ ] **Step 1: Write the failing test**

Create `src/ui/views/table.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Pod;
    use kube::api::ApiResource;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn pod(name: &str, phase: &str) -> Arc<DynamicObject> {
        let mut o = DynamicObject::new(name, &ApiResource::erase::<Pod>(&())).within("default");
        o.data = serde_json::json!({
            "status": {
                "phase": phase,
                "containerStatuses": [{"ready": true, "restartCount": 0}]
            }
        });
        Arc::new(o)
    }

    fn render(objects: &[Arc<DynamicObject>], w: u16, h: u16) -> (String, HitRegistry) {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut view = TableView::new();
        let mut hits = HitRegistry::new();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");
        term.draw(|f| {
            let area = f.area();
            render_table(f, area, objects, &gvk, &mut view, &mut hits);
        })
        .unwrap();

        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..h {
            for x in 0..w {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        (text, hits)
    }

    #[test]
    fn renders_the_column_headers() {
        let (text, _) = render(&[pod("a", "Running")], 60, 8);
        assert!(text.contains("NAME"), "expected NAME header in:\n{text}");
        assert!(text.contains("READY"), "expected READY header in:\n{text}");
        assert!(text.contains("STATUS"), "expected STATUS header in:\n{text}");
    }

    #[test]
    fn renders_object_names_and_derived_cells() {
        let (text, _) = render(&[pod("api-7d9f-x2k", "Running")], 60, 8);
        assert!(text.contains("api-7d9f-x2k"), "expected pod name in:\n{text}");
        assert!(text.contains("Running"), "expected phase in:\n{text}");
        assert!(text.contains("1/1"), "expected ready ratio in:\n{text}");
    }

    #[test]
    fn registers_one_hit_zone_per_visible_row() {
        let pods = vec![pod("a", "Running"), pod("b", "Running"), pod("c", "Running")];
        let (_, hits) = render(&pods, 60, 10);
        let mut found = Vec::new();
        for row in 0..10u16 {
            if let Some(HitTarget::TableRow(i)) = hits.hit(5, row) {
                found.push(*i);
            }
        }
        assert_eq!(found, vec![0, 1, 2], "each rendered row must be clickable");
    }

    #[test]
    fn registers_clickable_column_headers() {
        let (_, hits) = render(&[pod("a", "Running")], 60, 8);
        let mut found_header = false;
        for row in 0..8u16 {
            if matches!(hits.hit(2, row), Some(HitTarget::ColumnHeader(_))) {
                found_header = true;
            }
        }
        assert!(found_header, "the header row must be clickable for sorting");
    }

    #[test]
    fn an_empty_table_renders_without_panicking() {
        let (text, _) = render(&[], 60, 8);
        assert!(text.contains("NAME"), "headers still show when there are no rows");
    }

    #[test]
    fn a_tiny_viewport_renders_exactly_the_available_lines() {
        // A terminal too small for the header plus any row is a real crash
        // source in layout code; this pins both non-panic and correct extent.
        let pods = vec![pod("a", "Running"), pod("b", "Running")];
        let (text, _) = render(&pods, 12, 3);
        assert_eq!(text.lines().count(), 3, "must fill exactly the viewport height");
        assert!(text.lines().all(|l| l.chars().count() == 12), "no line may exceed the width");
    }

    #[test]
    fn failing_phases_are_styled_differently_from_running() {
        assert_ne!(phase_style("Running"), phase_style("CrashLoopBackOff"),
                   "a failing pod must be visually distinct");
        assert_ne!(phase_style("Running"), phase_style("Pending"));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib table`
Expected: FAIL — `cannot find type TableView`.

- [ ] **Step 3: Write the theme**

Create `src/ui/theme.rs`:

```rust
use ratatui::style::Color;

pub const FG: Color = Color::Gray;
pub const HEADER: Color = Color::Cyan;
pub const SELECTED: Color = Color::Yellow;
pub const OK: Color = Color::Green;
pub const WARN: Color = Color::Yellow;
pub const ERR: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
```

- [ ] **Step 4: Write the minimal implementation**

Prepend to `src/ui/views/table.rs`:

```rust
use crate::store::columns::columns_for;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use kube::api::{DynamicObject, GroupVersionKind};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Row, Table, TableState};
use ratatui::Frame;
use std::sync::Arc;

pub struct TableView {
    pub state: TableState,
    pub scroll_offset: usize,
}

impl Default for TableView {
    fn default() -> Self {
        Self::new()
    }
}

impl TableView {
    pub fn new() -> Self {
        Self { state: TableState::default().with_selected(Some(0)), scroll_offset: 0 }
    }
}

/// Colour a pod phase by severity so problems are visible without reading.
pub fn phase_style(phase: &str) -> Style {
    let color = match phase {
        "Running" | "Succeeded" => theme::OK,
        "Pending" | "ContainerCreating" => theme::WARN,
        "Failed" | "CrashLoopBackOff" | "Error" | "ImagePullBackOff" => theme::ERR,
        _ => theme::MUTED,
    };
    Style::default().fg(color)
}

/// Render the resource table and register a clickable zone for every visible row.
///
/// Rows are registered against the same geometry ratatui uses to lay them out:
/// the block border takes one line, the header one more, so the first data row
/// begins at `area.y + 2`.
pub fn render_table(
    f: &mut Frame,
    area: Rect,
    objects: &[Arc<DynamicObject>],
    gvk: &GroupVersionKind,
    view: &mut TableView,
    hits: &mut HitRegistry,
) {
    let columns = columns_for(gvk);
    let widths: Vec<Constraint> = columns.iter().map(|c| c.width).collect();

    let header = Row::new(columns.iter().map(|c| c.header).collect::<Vec<_>>())
        .style(Style::default().fg(theme::HEADER).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = objects
        .iter()
        .map(|obj| {
            let cells: Vec<String> = columns.iter().map(|c| (c.extract)(obj)).collect();
            // Style the whole row by phase when the kind exposes one.
            let style = columns
                .iter()
                .position(|c| c.header == "STATUS")
                .map(|i| phase_style(&cells[i]))
                .unwrap_or_else(|| Style::default().fg(theme::FG));
            Row::new(cells).style(style)
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(Style::default().fg(theme::SELECTED).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title(gvk.kind.clone()));

    f.render_stateful_widget(table, area, &mut view.state);

    // Register hit zones matching the geometry above.
    let header_y = area.y + 1;
    if header_y < area.y + area.height {
        hits.push(
            Rect { x: area.x + 1, y: header_y, width: area.width.saturating_sub(2), height: 1 },
            0,
            HitTarget::ColumnHeader(0),
        );
    }

    let first_row_y = area.y + 2;
    let last_y = area.y + area.height.saturating_sub(1);
    for (i, _) in objects.iter().enumerate() {
        let y = first_row_y + i as u16;
        if y >= last_y {
            break;
        }
        hits.push(
            Rect { x: area.x + 1, y, width: area.width.saturating_sub(2), height: 1 },
            0,
            HitTarget::TableRow(i),
        );
    }
}
```

Create `src/ui/views/mod.rs`:

```rust
pub mod table;
```

Update `src/ui/mod.rs`:

```rust
pub mod hit;
pub mod theme;
pub mod views;
pub use hit::{HitRegistry, HitTarget};
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib table`
Expected: PASS — 7 tests.

- [ ] **Step 6: Commit**

```bash
git add src/ui
git commit -m "feat: resource table rendering with per-row hit zones"
```

---

### Task 9: Mouse and keyboard input handling

Scroll targets the region **under the cursor** rather than the focused pane — this is what makes a TUI feel native rather than emulated.

**Files:**
- Create: `src/app/input.rs`
- Modify: `src/app/mod.rs`

**Interfaces:**
- Consumes: `HitRegistry`/`HitTarget` (Task 7).
- Produces:
  - `enum Action { SelectRow(usize), ScrollBy(i32), SortByColumn(usize), Quit, None }`
  - `fn action_for(event: &CtEvent, hits: &HitRegistry) -> Action`
  - `fn apply_selection(current: usize, delta: i32, len: usize) -> usize`

- [ ] **Step 1: Write the failing test**

Create `src/app/input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use ratatui::layout::Rect;

    fn registry() -> HitRegistry {
        let mut r = HitRegistry::new();
        r.push(Rect { x: 0, y: 2, width: 40, height: 1 }, 0, HitTarget::TableRow(0));
        r.push(Rect { x: 0, y: 3, width: 40, height: 1 }, 0, HitTarget::TableRow(1));
        r.push(Rect { x: 0, y: 1, width: 40, height: 1 }, 0, HitTarget::ColumnHeader(2));
        r
    }

    fn click(col: u16, row: u16) -> CtEvent {
        CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn scroll(kind: MouseEventKind, col: u16, row: u16) -> CtEvent {
        CtEvent::Mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE })
    }

    fn key(code: KeyCode) -> CtEvent {
        CtEvent::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn clicking_a_row_selects_it() {
        assert_eq!(action_for(&click(5, 3), &registry()), Action::SelectRow(1));
    }

    #[test]
    fn clicking_a_header_sorts_by_that_column() {
        assert_eq!(action_for(&click(5, 1), &registry()), Action::SortByColumn(2));
    }

    #[test]
    fn clicking_empty_space_does_nothing() {
        assert_eq!(action_for(&click(5, 40), &registry()), Action::None);
    }

    #[test]
    fn scrolling_over_the_table_scrolls_it() {
        assert_eq!(action_for(&scroll(MouseEventKind::ScrollDown, 5, 3), &registry()),
                   Action::ScrollBy(3));
        assert_eq!(action_for(&scroll(MouseEventKind::ScrollUp, 5, 3), &registry()),
                   Action::ScrollBy(-3));
    }

    #[test]
    fn scrolling_over_nothing_does_nothing() {
        assert_eq!(action_for(&scroll(MouseEventKind::ScrollDown, 5, 40), &registry()),
                   Action::None,
                   "scroll targets the region under the cursor, not the focused pane");
    }

    #[test]
    fn arrow_and_vim_keys_move_the_selection() {
        assert_eq!(action_for(&key(KeyCode::Down), &registry()), Action::ScrollBy(1));
        assert_eq!(action_for(&key(KeyCode::Up), &registry()), Action::ScrollBy(-1));
        assert_eq!(action_for(&key(KeyCode::Char('j')), &registry()), Action::ScrollBy(1));
        assert_eq!(action_for(&key(KeyCode::Char('k')), &registry()), Action::ScrollBy(-1));
    }

    #[test]
    fn q_and_esc_quit() {
        assert_eq!(action_for(&key(KeyCode::Char('q')), &registry()), Action::Quit);
        assert_eq!(action_for(&key(KeyCode::Esc), &registry()), Action::Quit);
    }

    #[test]
    fn selection_clamps_at_both_ends() {
        assert_eq!(apply_selection(0, -1, 5), 0, "must not wrap past the top");
        assert_eq!(apply_selection(4, 1, 5), 4, "must not wrap past the bottom");
        assert_eq!(apply_selection(2, 1, 5), 3);
        assert_eq!(apply_selection(2, -1, 5), 1);
        assert_eq!(apply_selection(0, 10, 5), 4, "a big jump clamps to the last row");
    }

    #[test]
    fn selection_on_an_empty_list_stays_at_zero() {
        assert_eq!(apply_selection(0, 1, 0), 0, "an empty table must not index out of bounds");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib input`
Expected: FAIL — `cannot find type Action`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/app/input.rs`:

```rust
use crate::ui::hit::{HitRegistry, HitTarget};
use crossterm::event::{Event as CtEvent, KeyCode, MouseButton, MouseEventKind};

/// Scroll wheel step, in rows. Matches typical terminal conventions.
const SCROLL_STEP: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SelectRow(usize),
    ScrollBy(i32),
    SortByColumn(usize),
    Quit,
    None,
}

/// Translate a raw input event into an action, resolving mouse position
/// through the current frame's hit registry.
pub fn action_for(event: &CtEvent, hits: &HitRegistry) -> Action {
    match event {
        CtEvent::Mouse(m) => match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(i)) => Action::SelectRow(*i),
                Some(HitTarget::ColumnHeader(i)) => Action::SortByColumn(*i),
                _ => Action::None,
            },
            // Scroll applies to whatever is under the cursor, not to focus.
            MouseEventKind::ScrollDown => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(SCROLL_STEP)
                }
                _ => Action::None,
            },
            MouseEventKind::ScrollUp => match hits.hit(m.column, m.row) {
                Some(HitTarget::TableRow(_)) | Some(HitTarget::ColumnHeader(_)) => {
                    Action::ScrollBy(-SCROLL_STEP)
                }
                _ => Action::None,
            },
            _ => Action::None,
        },
        CtEvent::Key(k) => match k.code {
            KeyCode::Down | KeyCode::Char('j') => Action::ScrollBy(1),
            KeyCode::Up | KeyCode::Char('k') => Action::ScrollBy(-1),
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            _ => Action::None,
        },
        _ => Action::None,
    }
}

/// Move a selection index by `delta`, clamped to the list.
pub fn apply_selection(current: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current as i64 + delta as i64;
    next.clamp(0, len as i64 - 1) as usize
}
```

Update `src/app/mod.rs`:

```rust
pub mod event;
pub mod input;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib input`
Expected: PASS — 9 tests.

- [ ] **Step 5: Commit**

```bash
git add src/app
git commit -m "feat: mouse and keyboard input mapped through hit registry"
```

---

### Task 10: Status bar and wiring it all together

This task produces the first genuinely runnable product: connect, watch, render, navigate.

**Files:**
- Create: `src/ui/views/status.rs`
- Modify: `src/ui/views/mod.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-9.
- Produces:
  - `fn render_status(f: &mut Frame, area: Rect, context: &str, namespace: &str, status: WatchStatus, count: usize, error: Option<&str>, hits: &mut HitRegistry)`
  - `fn status_label(status: WatchStatus) -> (&'static str, Style)`

- [ ] **Step 1: Write the failing test**

Create `src/ui/views/status.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(status: WatchStatus, count: usize, error: Option<&str>) -> String {
        let mut term = Terminal::new(TestBackend::new(80, 1)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let area = f.area();
            render_status(f, area, "prod-eu", "payments", status, count, error, &mut hits);
        })
        .unwrap();
        let buf = term.backend().buffer();
        (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    #[test]
    fn shows_context_and_namespace() {
        let text = render(WatchStatus::Synced, 42, None);
        assert!(text.contains("prod-eu"), "got: {text}");
        assert!(text.contains("payments"), "got: {text}");
    }

    #[test]
    fn shows_the_object_count() {
        let text = render(WatchStatus::Synced, 42, None);
        assert!(text.contains("42"), "got: {text}");
    }

    #[test]
    fn reconnecting_is_visible_so_stale_data_is_never_shown_as_live() {
        let text = render(WatchStatus::Reconnecting, 42, None);
        assert!(text.contains("reconnect"), "reconnect state must be visible; got: {text}");
    }

    #[test]
    fn an_error_is_surfaced_rather_than_swallowed() {
        let text = render(WatchStatus::Failed, 0, Some("forbidden: pods is denied"));
        assert!(text.contains("forbidden"), "errors must reach the user; got: {text}");
    }

    #[test]
    fn each_status_has_a_distinct_label() {
        let labels = [
            status_label(WatchStatus::Initialising).0,
            status_label(WatchStatus::Synced).0,
            status_label(WatchStatus::Reconnecting).0,
            status_label(WatchStatus::Failed).0,
        ];
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), 4, "statuses must be distinguishable");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib status`
Expected: FAIL — `cannot find function render_status`.

- [ ] **Step 3: Write the minimal implementation**

Prepend to `src/ui/views/status.rs`:

```rust
use crate::app::event::WatchStatus;
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// A short label and colour per watch state. Freshness is always visible:
/// presenting stale data as live is the worst failure mode for an ops tool.
pub fn status_label(status: WatchStatus) -> (&'static str, Style) {
    match status {
        WatchStatus::Initialising => ("loading", Style::default().fg(theme::MUTED)),
        WatchStatus::Synced => ("live", Style::default().fg(theme::OK)),
        WatchStatus::Reconnecting => ("reconnecting", Style::default().fg(theme::WARN)),
        WatchStatus::Failed => ("failed", Style::default().fg(theme::ERR)),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_status(
    f: &mut Frame,
    area: Rect,
    context: &str,
    namespace: &str,
    status: WatchStatus,
    count: usize,
    error: Option<&str>,
    hits: &mut HitRegistry,
) {
    let (label, style) = status_label(status);

    let mut spans = vec![
        Span::styled(format!(" {context} "), Style::default().fg(theme::HEADER)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(format!("{namespace} "), Style::default().fg(theme::FG)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(format!("{count} items "), Style::default().fg(theme::FG)),
        Span::styled("· ", Style::default().fg(theme::MUTED)),
        Span::styled(label, style),
    ];

    if let Some(e) = error {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(e.to_string(), Style::default().fg(theme::ERR)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
    hits.push(area, 0, HitTarget::StatusBar);
}
```

Add `pub mod status;` to `src/ui/views/mod.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib status`
Expected: PASS — 5 tests.

- [ ] **Step 5: Wire the full application**

Replace `src/main.rs`:

```rust
use kube_tui::app::event::{coalesce, AppEvent, WatchStatus};
use kube_tui::app::input::{action_for, apply_selection, Action};
use kube_tui::{cluster, store, terminal, ui};
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, GroupVersionKind};
use ratatui::layout::{Constraint, Layout};
use kube_tui::store::watch::{spawn_watch, ResourceStore, SharedStore};
use std::sync::Arc;
use kube_tui::terminal::{install_panic_hook, RealTerminal, TerminalGuard};
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use kube_tui::ui::hit::HitRegistry;
use kube_tui::ui::views::status::render_status;
use kube_tui::ui::views::table::{render_table, TableView};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_hook();

    let client = cluster::connect().await?;
    let contexts = cluster::load_contexts().unwrap_or_default();
    let current = contexts
        .iter()
        .find(|c| c.is_current)
        .map(|c| (c.name.clone(), c.namespace.clone().unwrap_or_else(|| "default".into())))
        .unwrap_or_else(|| ("unknown".into(), "default".into()));

    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();

    let pod_ar = ApiResource::erase::<Pod>(&());
    let pod_gvk = GroupVersionKind::gvk("", "v1", "Pod");
    let _watch = spawn_watch(
        client.clone(),
        pod_ar,
        Some(current.1.clone()),
        store.clone(),
        tx.clone(),
    );

    // Feed terminal input into the same channel so there is one wake source.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut events = crossterm::event::EventStream::new();
            while let Some(Ok(e)) = events.next().await {
                if tx.send(AppEvent::Input(e)).is_err() {
                    break;
                }
            }
        });
    }

    let mut term = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)?;
    let mut guard = TerminalGuard::new(RealTerminal);

    let mut view = TableView::new();
    let mut hits = HitRegistry::new();
    let mut selected: usize = 0;
    let mut last_error: Option<String> = None;
    let mut status = WatchStatus::Initialising;

    loop {
        // Block for at least one event, then drain everything queued behind it.
        let Some(first) = rx.recv().await else { break };
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

        let objects = store.read().await.objects(&pod_gvk);

        let mut quit = false;
        for input in &batch.inputs {
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

        view.state.select(if objects.is_empty() { None } else { Some(selected) });

        hits.clear();
        term.draw(|f| {
            let chunks = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
                .split(f.area());
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
```

- [ ] **Step 6: Verify the whole suite passes**

Run: `cargo test`
Expected: PASS — all 62 unit tests.

Run: `cargo clippy -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add src
git commit -m "feat: status bar and full application wiring"
```

---

### Task 11: Integration against a real cluster

The unit tests prove the logic. Only a real API server proves the watch behaves, and this is the first point where the binary is run against real data. These tests are marked `#[ignore]` so the default `cargo test` stays green without a cluster.

**Files:**
- Create: `tests/integration_kind.rs`
- Create: `scripts/dev-cluster.sh`

**Interfaces:**
- Consumes: `cluster::connect` (Task 3), `spawn_watch`/`ResourceStore` (Task 5).
- Produces: no library API; verification only.

- [ ] **Step 1: Install the tooling and create a cluster**

```bash
# kubectl
curl -LO "https://dl.k8s.io/release/$(curl -Ls https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl"
sudo install -o root -g root -m 0755 kubectl /usr/local/bin/kubectl && rm kubectl

# kind
curl -Lo ./kind https://kind.sigs.k8s.io/dl/latest/kind-linux-amd64
sudo install -o root -g root -m 0755 kind /usr/local/bin/kind && rm kind
```

Create `scripts/dev-cluster.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
CLUSTER="${CLUSTER:-kube-tui-dev}"

if ! kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  kind create cluster --name "$CLUSTER"
fi

kubectl --context "kind-$CLUSTER" create namespace demo --dry-run=client -o yaml \
  | kubectl --context "kind-$CLUSTER" apply -f -
kubectl --context "kind-$CLUSTER" -n demo create deployment web \
  --image=nginx:alpine --replicas=3 --dry-run=client -o yaml \
  | kubectl --context "kind-$CLUSTER" apply -f -
kubectl --context "kind-$CLUSTER" -n demo rollout status deployment/web --timeout=120s
echo "Cluster '$CLUSTER' ready with 3 pods in namespace 'demo'."
```

Run:

```bash
chmod +x scripts/dev-cluster.sh && ./scripts/dev-cluster.sh
```

Expected: `Cluster 'kube-tui-dev' ready with 3 pods in namespace 'demo'.`

- [ ] **Step 2: Write the failing integration test**

Create `tests/integration_kind.rs`:

```rust
//! Cluster-backed tests, marked #[ignore] so `cargo test` stays green on
//! machines with no cluster. Run them with:
//!   ./scripts/dev-cluster.sh && cargo test -- --ignored

use k8s_openapi::api::core::v1::Pod;
use kube::api::{ApiResource, GroupVersionKind};
use kube_tui::app::event::{AppEvent, WatchStatus};
use kube_tui::store::watch::{spawn_watch, ResourceStore, SharedStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn watch_populates_the_store_from_a_real_cluster() {
    let client = kube_tui::cluster::connect().await.expect("connect to cluster");
    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");

    let _h = spawn_watch(
        client,
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx,
    );

    // Wait for the initial sync rather than sleeping a fixed amount.
    let synced = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = rx.recv().await {
            if let AppEvent::WatchStatus { status: WatchStatus::Synced, .. } = ev {
                return true;
            }
        }
        false
    })
    .await
    .expect("watch did not sync within 30s");

    assert!(synced, "expected a Synced status event");

    let objects = store.read().await.objects(&gvk);
    assert!(
        objects.len() >= 3,
        "expected at least the 3 demo pods, found {}",
        objects.len()
    );
}

#[tokio::test]
#[ignore = "requires a cluster; run ./scripts/dev-cluster.sh then cargo test -- --ignored"]
async fn store_reflects_a_deletion_made_during_the_watch() {
    let client = kube_tui::cluster::connect().await.expect("connect");
    let store: SharedStore = Arc::new(RwLock::new(ResourceStore::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let gvk = GroupVersionKind::gvk("", "v1", "Pod");

    let _h = spawn_watch(
        client.clone(),
        ApiResource::erase::<Pod>(&()),
        Some("demo".to_string()),
        store.clone(),
        tx,
    );

    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(ev) = rx.recv().await {
            if let AppEvent::WatchStatus { status: WatchStatus::Synced, .. } = ev {
                return;
            }
        }
    })
    .await
    .expect("initial sync timed out");

    let before = store.read().await.objects(&gvk);
    let victim = before.first().expect("at least one pod").metadata.name.clone().unwrap();

    let pods: kube::Api<Pod> = kube::Api::namespaced(client, "demo");
    use kube::api::DeleteParams;
    let _ = pods.delete(&victim, &DeleteParams::default()).await;

    // The deployment replaces the pod, so assert the specific name disappears
    // rather than asserting on the count.
    let gone = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let names: Vec<String> =
                store.read().await.objects(&gvk).iter().filter_map(|o| o.metadata.name.clone()).collect();
            if !names.contains(&victim) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .unwrap_or(false);

    assert!(gone, "deleted pod {victim} never disappeared from the store");
}
```

- [ ] **Step 3: Confirm the library exposes every module**

The library target was created in Task 1 and each task added its module to it.
Confirm `src/lib.rs` reads:

```rust
pub mod app;
pub mod cluster;
pub mod store;
pub mod terminal;
pub mod ui;
```

Integration tests link against this library. No `Cargo.toml` change is needed —
the `[lib]` section already exists from Task 1.

- [ ] **Step 4: Run the test to verify it fails without a cluster, then passes with one**

Run: `cargo test --test integration_kind`
Expected: PASS with `0 passed; 2 ignored` — no cluster contacted.

Run: `cargo test --test integration_kind -- --ignored --nocapture`
Expected: PASS — both tests, with the store reporting at least 3 pods.

- [ ] **Step 5: Run the application against the real cluster**

Run: `cargo run`

Verify by hand:
1. The pod list appears, showing the 3 `web-*` pods from namespace `demo`.
2. The status bar shows the context, `demo`, the count, and `live`.
3. Arrow keys and `j`/`k` move the selection; it clamps at both ends.
4. Clicking a row selects it.
5. Scrolling the wheel over the table moves the selection; scrolling over the status bar does not.
6. In another shell, `kubectl -n demo scale deployment/web --replicas=6` — new rows appear without any interaction.
7. `kubectl -n demo delete pod <name>` — the row disappears and a replacement appears.
8. `q` exits and the shell is fully usable.

- [ ] **Step 6: Commit**

```bash
git add tests scripts src/lib.rs src/main.rs Cargo.toml
git commit -m "test: cluster-backed integration tests against kind"
```

---

## Definition of Done

- [ ] `cargo test` passes with no cluster present.
- [ ] `cargo test -- --ignored` passes against a kind cluster.
- [ ] `cargo clippy -- -D warnings` is clean.
- [ ] `cargo fmt --check` is clean.
- [ ] `cargo run` shows live pods, navigable by mouse and keyboard.
- [ ] Cluster changes appear without user interaction.
- [ ] Quitting and panicking both leave the terminal usable.

## What Plan 2 Builds On This

Sidebar tree with live per-kind counts, dynamic discovery of all kinds including CRDs, the detail pane with YAML and events tabs, context and namespace switching at runtime, pane resizing by border drag, column sorting, and the mouse-capture toggle for native text selection. `HitTarget` gains variants; `columns_for` gains entries and the server-side Table fallback.
