# kube — Plan 2: Multi-cluster, theme, and server-side columns

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the tool usable across 20+ corporate clusters — pick a cluster, connect lazily, switch without leaking watches — and give it a deliberate visual identity that stays legible under pressure.

**Architecture:** Extends Plan 1's three layers. The `cluster` layer gains a registry that owns per-cluster connection state and lifecycle. The `store` layer gains a handle registry so every watch a cluster owns can be aborted atomically on switch. The `ui` layer gains a theme token system, a cluster ribbon, and picker overlays. Render cost becomes O(viewport) rather than O(objects).

**Tech Stack:** Rust (edition 2024), `kube` 4.2, `k8s-openapi` 0.28, `ratatui` 0.30, `crossterm` 0.29, `tokio` 1.x, `http` (new — required to set an Accept header on a raw request).

Plan 2 of 5 for v1. Source spec: `docs/superpowers/specs/2026-08-25-kube-tui-design.md`.
**Verified API reference: `docs/superpowers/plan2-api-reference.md`** — every kube/ratatui call this plan depends on was confirmed by compiling it. Consult it rather than recalling APIs.

## Context: what changed since Plan 1

Plan 1 shipped and merged (36 commits, 112 tests). First run against a real corporate cluster confirmed auth, connection, and the watch loop all work — the status bar reached `live`.

Three facts from that run reshape this plan:

1. **20+ clusters, hundreds of pods each.** Per-cluster scale is comfortable for the eager in-memory cache. Cluster *count* is the new design pressure.
2. **Contexts do not set a namespace**, so the tool landed in an empty `default`. `-n`/`-A` flags now exist; this plan adds an in-app picker and makes all-namespaces the default on connect.
3. **The UI must be colourful, fast, smooth and intuitive** — an explicit brief, not an inference.

## Global Constraints

These apply to every task and are implicitly part of every task's requirements.

- **Rust edition 2024.** Minimum toolchain 1.85.
- **Existing dependencies are fixed:** `kube = "4.2"` (features `runtime`, `client`, `derive`, `oidc`, `http-proxy`, `socks5`, `gzip`), `k8s-openapi = "0.28"` (feature `latest`), `ratatui = "0.30"`, `crossterm = "0.29"` (feature `event-stream`), `tokio = "1"` (feature `full`), `futures = "0.3"`, `serde_json = "1"`, `indexmap = "2"`, `chrono = "0.4"`, `anyhow = "1"`, `thiserror = "2"`.
- **One new dependency is authorised: `http = "1"`.** It is already in the graph via `kube`, but Rust requires a direct manifest entry to name `http::header::ACCEPT`. No other dependency may be added. (`serde_norway` for YAML is decided but belongs to Plan 3 — do not add it here.)
- **Never connect to more than one cluster at a time.** Listing clusters is a kubeconfig parse and must not touch the network.
- **The render closure passed to `term.draw` must be synchronous**, perform no I/O and acquire no locks. Read snapshots before drawing.
- **Never render on a fixed tick.** No timers, no animation frames. Idle must remain 0% CPU.
- **Formatting cost must be O(viewport), not O(objects).**
- **Chrome uses the cool hue family; status signals use the warm-plus-green family.** Never colour a status with a chrome token or vice versa.
- **Never write to stdout/stderr while the alternate screen is active.**
- **No `unwrap()`/`expect()` outside tests** except where a comment justifies the invariant.
- **Credentials are never read, logged or printed.** `auth_info.token` is opaque; only `.is_some()`.
- **Tests must not require a cluster, network or TTY** except the `#[ignore]`d integration tests.
- **TDD:** write the failing test, run it, observe the failure, then implement. Report the real output.
- **Mutation-check each task's central guarantee** before committing: break the implementation deliberately, confirm the covering test fails, restore, confirm it passes. Paste both.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean before every commit.
- **Stage explicit paths when committing**, never `git add -A`.

## Design tokens (authoritative — derive every colour from here)

Chrome is the cool hue family. Signal is the warm-plus-green family. They never swap roles.

| Token | Hex | Role |
|---|---|---|
| `INK` | `#0F1117` | application ground |
| `ABYSS` | `#151A24` | panel fill, raised surface |
| `DUSK` | `#3A4260` | unfocused borders, rules |
| `INDIGO` | `#5B6EE1` | focused pane border, focus ring |
| `PERIWINKLE` | `#8FA0FF` | column headers |
| `TEAL` | `#4FD6C9` | sidebar section labels |
| `VIOLET` | `#A78BFA` | counts, badges |
| `PAPER` | `#E4E8F0` | primary text |
| `MIST` | `#8A93A6` | secondary text, labels |
| `VIRIDIAN` | `#3DD68C` | healthy · Running · Ready |
| `AMBER` | `#FFC145` | transitional · Pending |
| `CORAL` | `#FF6B6B` | failed · CrashLoopBackOff |

Glyph vocabulary — geometric, single weight, no emoji (they break the monospace grid):
`●` status · `▸ ▾` disclosure · `▎` change marker · `⟳` reconnecting · `⌕` filter · `╭ ╮ ╰ ╯` rounded corners.

## File Structure

| File | Responsibility |
|---|---|
| `src/ui/theme.rs` | **Rewritten.** Colour tokens, `Style` builders, cluster-hue hashing |
| `src/ui/ribbon.rs` | The per-cluster colour spine |
| `src/ui/views/picker.rs` | Modal list overlay, used for both cluster and namespace pickers |
| `src/cluster/registry.rs` | `ClusterId`, per-cluster connection state, lazy connect |
| `src/store/handles.rs` | Watch handle registry; abort-all on cluster switch |
| `src/store/table.rs` | Server-side Table request + decode (CRD columns) |
| `src/store/columns.rs` | **Modified.** Accept server-side columns; format only visible rows |
| `src/ui/views/table.rs` | **Modified.** Viewport-only formatting; themed |
| `src/ui/views/status.rs` | **Modified.** Themed; shows cluster hue |
| `src/app/mod.rs` | **Modified.** Focus/overlay state |
| `src/main.rs` | **Modified.** Wiring |

---

### Task 1: Theme tokens and cluster hue hashing

The visual system, built first so every later task draws from it rather than inventing colours.

The cluster hue is the signature element: with 20+ clusters, "which cluster am I in?" is the recurring question and the classic cause of production accidents. Hashing into a **curated hue set** at fixed saturation and lightness — rather than into raw RGB — guarantees 20 clusters get 20 distinguishable, equally-legible colours instead of one landing on unreadable dark brown.

**Files:**
- Rewrite: `src/ui/theme.rs`
- Test: same file

**Interfaces:**
- Consumes: nothing.
- Produces:
  - Colour constants as listed in Design tokens, as `ratatui::style::Color::Rgb`
  - `fn cluster_hue(name: &str) -> Color`
  - `fn phase_style(phase: &str) -> Style` (moved here from `views/table.rs`)
  - `fn border_style(focused: bool) -> Style`
  - `fn header_style() -> Style`, `fn label_style() -> Style`, `fn count_style() -> Style`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_hue_is_stable_for_the_same_name() {
        assert_eq!(cluster_hue("prod-eu"), cluster_hue("prod-eu"));
    }

    #[test]
    fn different_clusters_generally_get_different_hues() {
        // Not a guarantee for every pair — 20+ clusters into a finite palette
        // will collide — but the common case must discriminate.
        let names = ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"];
        let hues: std::collections::HashSet<_> = names.iter().map(|n| cluster_hue(n)).collect();
        assert!(hues.len() >= 4, "expected at least 4 distinct hues from 5 names, got {}", hues.len());
    }

    #[test]
    fn every_cluster_hue_comes_from_the_curated_palette() {
        // Hashing to raw RGB would eventually produce an unreadable colour.
        for name in ["a", "b", "prod", "zzzz", "", "tst-wsdc", "a-very-long-cluster-name"] {
            assert!(
                CLUSTER_HUES.contains(&cluster_hue(name)),
                "{name} produced a hue outside the curated palette"
            );
        }
    }

    #[test]
    fn failing_phases_are_visually_distinct_from_healthy_ones() {
        assert_ne!(phase_style("Running"), phase_style("CrashLoopBackOff"));
        assert_ne!(phase_style("Running"), phase_style("Pending"));
        assert_ne!(phase_style("Pending"), phase_style("CrashLoopBackOff"));
    }

    #[test]
    fn status_colours_never_reuse_a_chrome_token() {
        // Chrome is the cool family, signal is warm-plus-green. If a status
        // ever renders in a border colour it stops reading as a signal.
        let chrome = [INK, ABYSS, DUSK, INDIGO, PERIWINKLE, TEAL, VIOLET];
        for phase in ["Running", "Pending", "Failed", "CrashLoopBackOff", "Succeeded", "Unknown"] {
            let fg = phase_style(phase).fg.expect("phase styles must set a foreground");
            assert!(!chrome.contains(&fg), "{phase} rendered in a chrome colour");
        }
    }

    #[test]
    fn focus_is_visible_in_the_border() {
        assert_ne!(border_style(true), border_style(false));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib theme`
Expected: FAIL — `cannot find function cluster_hue`.

- [ ] **Step 3: Write the implementation**

```rust
use ratatui::style::{Color, Modifier, Style};

// Chrome — the cool hue family. Borders, headers, labels, counts.
pub const INK: Color = Color::Rgb(0x0F, 0x11, 0x17);
pub const ABYSS: Color = Color::Rgb(0x15, 0x1A, 0x24);
pub const DUSK: Color = Color::Rgb(0x3A, 0x42, 0x60);
pub const INDIGO: Color = Color::Rgb(0x5B, 0x6E, 0xE1);
pub const PERIWINKLE: Color = Color::Rgb(0x8F, 0xA0, 0xFF);
pub const TEAL: Color = Color::Rgb(0x4F, 0xD6, 0xC9);
pub const VIOLET: Color = Color::Rgb(0xA7, 0x8B, 0xFA);

// Text.
pub const PAPER: Color = Color::Rgb(0xE4, 0xE8, 0xF0);
pub const MIST: Color = Color::Rgb(0x8A, 0x93, 0xA6);

// Signal — the warm-plus-green family. Data only, never chrome.
pub const VIRIDIAN: Color = Color::Rgb(0x3D, 0xD6, 0x8C);
pub const AMBER: Color = Color::Rgb(0xFF, 0xC1, 0x45);
pub const CORAL: Color = Color::Rgb(0xFF, 0x6B, 0x6B);

/// Curated cluster hues: fixed saturation and lightness so every cluster's
/// colour is equally legible. Hashing into raw RGB would eventually produce
/// something unreadable against the ground.
pub const CLUSTER_HUES: [Color; 10] = [
    Color::Rgb(0x5B, 0x6E, 0xE1), // indigo
    Color::Rgb(0x4F, 0xD6, 0xC9), // teal
    Color::Rgb(0xA7, 0x8B, 0xFA), // violet
    Color::Rgb(0x3D, 0xD6, 0x8C), // green
    Color::Rgb(0xFF, 0xC1, 0x45), // amber
    Color::Rgb(0xFF, 0x8F, 0xB1), // rose
    Color::Rgb(0x6B, 0xC5, 0xFF), // sky
    Color::Rgb(0xD6, 0xA5, 0x5B), // sand
    Color::Rgb(0x9B, 0xE5, 0x64), // lime
    Color::Rgb(0xFF, 0x9E, 0x64), // tangerine
];

/// A stable colour for a cluster, used by the ribbon and the context label.
///
/// FNV-1a: small, deterministic across runs and platforms, and good enough
/// for bucketing names into a fixed palette.
pub fn cluster_hue(name: &str) -> Color {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    CLUSTER_HUES[(hash % CLUSTER_HUES.len() as u64) as usize]
}

/// Colour a pod phase by severity so problems are visible without reading.
pub fn phase_style(phase: &str) -> Style {
    let color = match phase {
        "Running" | "Succeeded" | "Ready" | "Active" | "Bound" => VIRIDIAN,
        "Pending" | "ContainerCreating" | "PodInitializing" | "Terminating" => AMBER,
        "Failed" | "CrashLoopBackOff" | "Error" | "ImagePullBackOff"
        | "ErrImagePull" | "Evicted" | "OOMKilled" => CORAL,
        _ => MIST,
    };
    Style::default().fg(color)
}

pub fn border_style(focused: bool) -> Style {
    Style::default().fg(if focused { INDIGO } else { DUSK })
}

pub fn header_style() -> Style {
    Style::default().fg(PERIWINKLE).add_modifier(Modifier::BOLD)
}

pub fn label_style() -> Style {
    Style::default().fg(TEAL)
}

pub fn count_style() -> Style {
    Style::default().fg(VIOLET)
}

pub fn text_style() -> Style {
    Style::default().fg(PAPER)
}

pub fn muted_style() -> Style {
    Style::default().fg(MIST)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib theme`
Expected: PASS — 6 tests.

- [ ] **Step 5: Update `views/table.rs` to use the theme**

`phase_style` now lives in `theme`. Remove the copy in `views/table.rs` and import it. Its existing test `failing_phases_are_styled_differently_from_running` moves to `theme.rs` (already written above as `failing_phases_are_visually_distinct_from_healthy_ones`) — delete the duplicate rather than leaving two.

Apply `header_style()` to the table header and `border_style(true)` to its block. Keep every existing table test passing; update only expected styles, never expected content.

- [ ] **Step 6: Mutation check**

Change `cluster_hue` to always return `CLUSTER_HUES[0]`. Run `cargo test --lib theme`. Confirm `different_clusters_generally_get_different_hues` FAILS. Restore, confirm green. Paste both.

- [ ] **Step 7: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/ui/theme.rs src/ui/views/table.rs
git commit -m "feat: theme tokens with cool chrome, warm signal, and per-cluster hues"
```

---

### Task 2: Cluster registry and connection state

Lists clusters from kubeconfig without touching the network, and models the lifecycle of connecting to one.

The key property: **listing is free, connecting is lazy.** With 20+ clusters, eagerly connecting would open 20 authenticated sessions — some of which will hang on a VPN-unreachable endpoint.

**Files:**
- Create: `src/cluster/registry.rs`
- Modify: `src/cluster/mod.rs`

**Interfaces:**
- Consumes: `ContextInfo`, `AuthMethod`, `connect_with`, `ConnectOptions` (Plan 1, Task 3/3b).
- Produces:
  - `struct ClusterId(pub String)` — the context name
  - `enum ConnectionState { Disconnected, Connecting, Connected, Failed { reason: String } }`
  - `struct ClusterEntry { pub id: ClusterId, pub context: ContextInfo, pub state: ConnectionState }`
  - `struct ClusterRegistry { entries: Vec<ClusterEntry>, active: Option<ClusterId> }`
  - `ClusterRegistry::from_contexts(Vec<ContextInfo>) -> Self`
  - `fn entries(&self) -> &[ClusterEntry]`, `fn active(&self) -> Option<&ClusterEntry>`
  - `fn set_state(&mut self, id: &ClusterId, state: ConnectionState)`
  - `fn set_active(&mut self, id: &ClusterId) -> bool`
  - `fn find(&self, id: &ClusterId) -> Option<&ClusterEntry>`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::auth::AuthMethod;

    fn ctx(name: &str, current: bool) -> ContextInfo {
        ContextInfo {
            name: name.to_string(),
            cluster: format!("{name}-cluster"),
            namespace: None,
            is_current: current,
            auth: AuthMethod::None,
        }
    }

    fn registry() -> ClusterRegistry {
        ClusterRegistry::from_contexts(vec![ctx("prod", false), ctx("staging", true), ctx("dev", false)])
    }

    #[test]
    fn every_context_becomes_an_entry() {
        assert_eq!(registry().entries().len(), 3);
    }

    #[test]
    fn entries_start_disconnected_because_listing_touches_no_network() {
        for e in registry().entries() {
            assert_eq!(e.state, ConnectionState::Disconnected, "{} was not disconnected", e.id.0);
        }
    }

    #[test]
    fn the_current_context_becomes_active_on_construction() {
        let r = registry();
        assert_eq!(r.active().map(|e| e.id.0.as_str()), Some("staging"));
    }

    #[test]
    fn with_no_current_context_nothing_is_active() {
        let r = ClusterRegistry::from_contexts(vec![ctx("a", false), ctx("b", false)]);
        assert!(r.active().is_none(), "must not guess an active cluster");
    }

    #[test]
    fn state_is_tracked_per_cluster() {
        let mut r = registry();
        r.set_state(&ClusterId("prod".into()), ConnectionState::Connected);
        assert_eq!(r.find(&ClusterId("prod".into())).unwrap().state, ConnectionState::Connected);
        assert_eq!(
            r.find(&ClusterId("dev".into())).unwrap().state,
            ConnectionState::Disconnected,
            "one cluster's state must not leak into another"
        );
    }

    #[test]
    fn a_failed_cluster_keeps_its_reason() {
        let mut r = registry();
        let id = ClusterId("prod".into());
        r.set_state(&id, ConnectionState::Failed { reason: "no route to host".into() });
        match &r.find(&id).unwrap().state {
            ConnectionState::Failed { reason } => assert_eq!(reason, "no route to host"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn setting_an_unknown_cluster_active_is_rejected_not_panicked() {
        let mut r = registry();
        assert!(!r.set_active(&ClusterId("nope".into())));
        assert_eq!(r.active().map(|e| e.id.0.as_str()), Some("staging"), "active must be unchanged");
    }

    #[test]
    fn setting_state_on_an_unknown_cluster_is_a_no_op() {
        let mut r = registry();
        r.set_state(&ClusterId("nope".into()), ConnectionState::Connected);
        assert_eq!(r.entries().len(), 3, "must not invent an entry");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib registry`
Expected: FAIL — `cannot find type ClusterRegistry`.

- [ ] **Step 3: Write the implementation**

```rust
use crate::cluster::config::ContextInfo;

/// A cluster's identity: its kubeconfig context name, which is unique per file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterId(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Failed { reason: String },
}

#[derive(Debug, Clone)]
pub struct ClusterEntry {
    pub id: ClusterId,
    pub context: ContextInfo,
    pub state: ConnectionState,
}

/// Every cluster the kubeconfig knows about, plus which one we are using.
///
/// Construction parses kubeconfig only — no network. With 20+ clusters,
/// connecting eagerly would open 20 authenticated sessions, some of which
/// will hang against an endpoint the current VPN cannot reach.
#[derive(Debug, Clone, Default)]
pub struct ClusterRegistry {
    entries: Vec<ClusterEntry>,
    active: Option<ClusterId>,
}

impl ClusterRegistry {
    pub fn from_contexts(contexts: Vec<ContextInfo>) -> Self {
        let active = contexts
            .iter()
            .find(|c| c.is_current)
            .map(|c| ClusterId(c.name.clone()));
        let entries = contexts
            .into_iter()
            .map(|context| ClusterEntry {
                id: ClusterId(context.name.clone()),
                context,
                state: ConnectionState::Disconnected,
            })
            .collect();
        Self { entries, active }
    }

    pub fn entries(&self) -> &[ClusterEntry] {
        &self.entries
    }

    pub fn find(&self, id: &ClusterId) -> Option<&ClusterEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    pub fn active(&self) -> Option<&ClusterEntry> {
        self.active.as_ref().and_then(|id| self.find(id))
    }

    pub fn set_state(&mut self, id: &ClusterId, state: ConnectionState) {
        if let Some(e) = self.entries.iter_mut().find(|e| &e.id == id) {
            e.state = state;
        }
    }

    /// Returns false if the cluster is unknown, leaving `active` unchanged.
    pub fn set_active(&mut self, id: &ClusterId) -> bool {
        if self.entries.iter().any(|e| &e.id == id) {
            self.active = Some(id.clone());
            true
        } else {
            false
        }
    }
}
```

Add `pub mod registry;` and re-exports to `src/cluster/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib registry`
Expected: PASS — 8 tests.

- [ ] **Step 5: Mutation check**

Make `set_state` apply the state to every entry rather than the matching one. Confirm `state_is_tracked_per_cluster` FAILS. Restore, confirm green. Paste both.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/cluster
git commit -m "feat: cluster registry with lazy per-cluster connection state"
```

---

### Task 3: Watch handle registry and abort-on-switch

Plan 1 spawns a watch and supervises its `JoinHandle` but **never aborts it**. That is harmless while the app connects once and never switches. The moment cluster switching exists it leaks: switch 20 times and you hold 20 live watch connections and 20 caches.

This task fixes the leak before the feature that would expose it.

**Files:**
- Create: `src/store/handles.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: `tokio::task::JoinHandle`.
- Produces:
  - `struct WatchHandles { handles: Vec<JoinHandle<()>> }`
  - `WatchHandles::new()`, `fn push(&mut self, h: JoinHandle<()>)`, `fn len(&self) -> usize`, `fn is_empty(&self) -> bool`
  - `fn abort_all(&mut self) -> usize` — aborts every handle, clears the list, returns how many were aborted

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn abort_all_stops_running_tasks_and_reports_the_count() {
        let ran = Arc::new(AtomicUsize::new(0));
        let mut handles = WatchHandles::new();

        for _ in 0..3 {
            let ran = ran.clone();
            handles.push(tokio::spawn(async move {
                // Long enough that abort lands first.
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                ran.fetch_add(1, Ordering::SeqCst);
            }));
        }

        assert_eq!(handles.len(), 3);
        assert_eq!(handles.abort_all(), 3);
        assert!(handles.is_empty(), "aborted handles must not linger in the registry");

        tokio::task::yield_now().await;
        assert_eq!(ran.load(Ordering::SeqCst), 0, "no task should have run to completion");
    }

    #[tokio::test]
    async fn abort_all_on_an_empty_registry_is_zero_not_a_panic() {
        let mut handles = WatchHandles::new();
        assert_eq!(handles.abort_all(), 0);
    }

    #[tokio::test]
    async fn a_registry_can_be_refilled_after_abort() {
        let mut handles = WatchHandles::new();
        handles.push(tokio::spawn(async {}));
        handles.abort_all();
        handles.push(tokio::spawn(async {}));
        assert_eq!(handles.len(), 1, "switching clusters repeatedly must not accumulate handles");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib handles`
Expected: FAIL — `cannot find type WatchHandles`.

- [ ] **Step 3: Write the implementation**

```rust
use tokio::task::JoinHandle;

/// Every watch task belonging to the active cluster.
///
/// Switching clusters must abort all of them. Without this, each switch leaks
/// a live watch connection and its cache — invisible with one cluster, and
/// twenty times over with twenty.
#[derive(Default)]
pub struct WatchHandles {
    handles: Vec<JoinHandle<()>>,
}

impl WatchHandles {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, handle: JoinHandle<()>) {
        self.handles.push(handle);
    }

    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Abort every watch and clear the registry. Returns how many were aborted.
    pub fn abort_all(&mut self) -> usize {
        let n = self.handles.len();
        for h in self.handles.drain(..) {
            h.abort();
        }
        n
    }
}
```

Add `pub mod handles;` to `src/store/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib handles`
Expected: PASS — 3 tests.

- [ ] **Step 5: Mutation check**

Make `abort_all` clear the list without calling `.abort()`. Confirm `abort_all_stops_running_tasks_and_reports_the_count` FAILS (the tasks will complete and increment the counter). Restore, confirm green. Paste both.

**Note on the supervisor:** aborting a task makes its `JoinHandle` return `Err(JoinError)` with `is_cancelled()` true. Plan 1's supervisor treats any `Err` as a reason to send `Quit`. **Task 8 must make the supervisor distinguish a deliberate abort from a genuine failure** — otherwise switching clusters will quit the app. Do not fix that here; it is called out in Task 8.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/store
git commit -m "feat: watch handle registry with abort-all for cluster switching"
```

---

### Task 4: Viewport-only formatting

The final review of Plan 1 measured 5.8ms per frame at 1000 pods, because every object is formatted every frame regardless of visibility. `extract_age` alone does a `format!`, a `Utc::now()`, and an RFC3339 reparse per row.

This makes render cost O(viewport) instead of O(objects) — constant whether a cluster holds 200 pods or 20,000.

**Files:**
- Modify: `src/ui/views/table.rs`

**Interfaces:**
- Consumes: `columns_for` (Plan 1, Task 6), `TableState::offset` (verified in the API reference, D-section).
- Produces:
  - `fn visible_window(offset: usize, area_height: u16, total: usize) -> std::ops::Range<usize>`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_visible_window_covers_only_rows_that_fit() {
        // A 10-row area spends 1 line on the top border, 1 on the header and
        // 1 on the bottom border, leaving 7 data rows.
        assert_eq!(visible_window(0, 10, 100), 0..7);
    }

    #[test]
    fn the_visible_window_follows_the_scroll_offset() {
        assert_eq!(visible_window(24, 10, 100), 24..31);
    }

    #[test]
    fn the_visible_window_is_clamped_to_the_object_count() {
        assert_eq!(visible_window(0, 10, 3), 0..3, "must not run past the end of the list");
        assert_eq!(visible_window(98, 10, 100), 98..100);
    }

    #[test]
    fn a_viewport_with_no_room_for_rows_yields_an_empty_window() {
        for h in [0u16, 1, 2, 3] {
            let w = visible_window(0, h, 100);
            assert!(w.start >= w.end || w.len() <= 1, "height {h} produced {w:?}");
        }
        assert!(visible_window(0, 0, 100).is_empty());
    }

    #[test]
    fn an_offset_past_the_end_yields_an_empty_window_rather_than_panicking() {
        assert!(visible_window(500, 10, 100).is_empty());
    }

    #[test]
    fn only_visible_rows_are_formatted() {
        // The guarantee this task exists for: a 5000-object list in a small
        // viewport must format tens of rows, not thousands.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static FORMATS: AtomicUsize = AtomicUsize::new(0);

        fn counting_extract(_o: &DynamicObject) -> String {
            FORMATS.fetch_add(1, Ordering::SeqCst);
            "x".to_string()
        }

        let pods: Vec<_> = (0..5000).map(|i| pod(&format!("p{i}"), "Running")).collect();
        let cols = vec![Column {
            header: "NAME",
            width: Constraint::Fill(1),
            extract: counting_extract,
        }];

        FORMATS.store(0, Ordering::SeqCst);
        let window = visible_window(0, 20, pods.len());
        for obj in &pods[window] {
            for c in &cols {
                let _ = (c.extract)(obj);
            }
        }

        let n = FORMATS.load(Ordering::SeqCst);
        assert!(n <= 20, "formatted {n} rows for a 20-row viewport; expected at most 20");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib table`
Expected: FAIL — `cannot find function visible_window`.

- [ ] **Step 3: Write the implementation**

```rust
/// The half-open range of object indices that can actually be drawn.
///
/// Formatting the whole list every frame costs O(objects); this makes it
/// O(viewport). The block border takes one line at the top and one at the
/// bottom, and the header takes one more, leaving `height - 3` data rows.
pub fn visible_window(offset: usize, area_height: u16, total: usize) -> std::ops::Range<usize> {
    let rows = area_height.saturating_sub(3) as usize;
    let start = offset.min(total);
    let end = start.saturating_add(rows).min(total);
    start..end
}
```

In `render_table`, build `Row`s only for `objects[visible_window(..)]`, and register hit zones for the same window (Plan 1's fix already registers against the offset — keep that behaviour and derive both from this one function so they cannot drift apart).

**Important:** `Table` must still be told the full selection index, not a window-relative one. Read the existing `render_stateful_widget` call carefully before changing it, and keep every existing table test passing — especially `hit_zones_follow_the_scrolled_viewport` and `hit_zones_align_with_the_rows_ratatui_actually_draws`.

If passing a windowed row list changes how ratatui computes `offset`, that is a real conflict — report it as DONE_WITH_CONCERNS rather than working around it, because the hit-zone tests encode the invariant that must not break.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib table`
Expected: PASS — all previous table tests plus 6 new.

- [ ] **Step 5: Mutation check**

Change `visible_window` to return `0..total`. Confirm `only_visible_rows_are_formatted` FAILS. Restore, confirm green. Paste both.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/ui/views/table.rs
git commit -m "perf: format only the rows the viewport can show"
```

---

### Task 5: The cluster ribbon

The signature element: a one-cell vertical spine, coloured by cluster, present in every frame.

**Files:**
- Create: `src/ui/ribbon.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Consumes: `cluster_hue` (Task 1), `HitRegistry`/`HitTarget` (Plan 1, Task 7).
- Produces:
  - `const RIBBON_WIDTH: u16 = 1;`
  - `fn render_ribbon(f: &mut Frame, area: Rect, cluster: Option<&str>, hits: &mut HitRegistry)`
  - `fn split_ribbon(area: Rect) -> (Rect, Rect)` — (ribbon, remainder)

`HitTarget` gains a `Ribbon` variant so clicking the spine opens the cluster picker.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use crate::ui::theme;

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect { Rect { x, y, width: w, height: h } }

    #[test]
    fn split_reserves_exactly_one_column_for_the_ribbon() {
        let (ribbon, rest) = split_ribbon(rect(0, 0, 80, 24));
        assert_eq!(ribbon.width, RIBBON_WIDTH);
        assert_eq!(ribbon.height, 24);
        assert_eq!(rest.x, RIBBON_WIDTH);
        assert_eq!(rest.width, 79, "the rest of the screen must lose exactly the ribbon column");
    }

    #[test]
    fn split_on_a_zero_width_area_does_not_underflow() {
        let (ribbon, rest) = split_ribbon(rect(0, 0, 0, 24));
        assert_eq!(rest.width, 0);
        assert!(ribbon.width <= 1);
    }

    #[test]
    fn the_ribbon_is_painted_in_the_clusters_own_hue() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("tst-wsdc"), &mut hits);
        }).unwrap();

        let buf = term.backend().buffer();
        let expected = theme::cluster_hue("tst-wsdc");
        for y in 0..5 {
            assert_eq!(buf[(0, y)].style().fg, Some(expected), "ribbon row {y} was not the cluster hue");
        }
    }

    #[test]
    fn two_clusters_paint_different_ribbons() {
        let paint = |name: &str| {
            let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
            let mut hits = HitRegistry::new();
            term.draw(|f| {
                let (ribbon, _) = split_ribbon(f.area());
                render_ribbon(f, ribbon, Some(name), &mut hits);
            }).unwrap();
            term.backend().buffer()[(0, 0)].style().fg
        };
        assert_ne!(paint("prod-eu"), paint("staging"));
    }

    #[test]
    fn with_no_cluster_the_ribbon_is_muted_not_absent() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, None, &mut hits);
        }).unwrap();
        assert_eq!(term.backend().buffer()[(0, 0)].style().fg, Some(theme::DUSK));
    }

    #[test]
    fn the_ribbon_is_clickable_along_its_whole_height() {
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut hits = HitRegistry::new();
        term.draw(|f| {
            let (ribbon, _) = split_ribbon(f.area());
            render_ribbon(f, ribbon, Some("prod"), &mut hits);
        }).unwrap();
        for y in 0..5 {
            assert_eq!(hits.hit(0, y), Some(&HitTarget::Ribbon), "ribbon not clickable at y={y}");
        }
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib ribbon`
Expected: FAIL — `cannot find function split_ribbon`.

- [ ] **Step 3: Write the implementation**

```rust
use crate::ui::hit::{HitRegistry, HitTarget};
use crate::ui::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Block;

pub const RIBBON_WIDTH: u16 = 1;

/// Reserve the leftmost column for the ribbon.
pub fn split_ribbon(area: Rect) -> (Rect, Rect) {
    let ribbon_w = RIBBON_WIDTH.min(area.width);
    let ribbon = Rect { x: area.x, y: area.y, width: ribbon_w, height: area.height };
    let rest = Rect {
        x: area.x.saturating_add(ribbon_w),
        y: area.y,
        width: area.width.saturating_sub(ribbon_w),
        height: area.height,
    };
    (ribbon, rest)
}

/// Paint the cluster spine.
///
/// With twenty-odd clusters, "which cluster am I in?" is the question that
/// matters and the one people get wrong. A persistent colour answers it
/// peripherally, without reading anything.
pub fn render_ribbon(f: &mut Frame, area: Rect, cluster: Option<&str>, hits: &mut HitRegistry) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let color = cluster.map(theme::cluster_hue).unwrap_or(theme::DUSK);
    let block = Block::default().style(Style::default().fg(color).bg(color));
    f.render_widget(block, area);
    hits.push(area, 0, HitTarget::Ribbon);
}
```

Add `Ribbon` to `HitTarget` in `src/ui/hit.rs`. Its existing 8 tests must keep passing unchanged — adding an enum variant should not touch them.

Add `pub mod ribbon;` to `src/ui/mod.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib ribbon`
Expected: PASS — 6 tests.

- [ ] **Step 5: Mutation check**

Make `render_ribbon` always use `theme::DUSK`. Confirm both `the_ribbon_is_painted_in_the_clusters_own_hue` and `two_clusters_paint_different_ribbons` FAIL. Restore, confirm green.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/ui
git commit -m "feat: per-cluster colour ribbon"
```

---

### Task 6: Modal picker overlay

One reusable overlay serving both the cluster picker and the namespace picker. Filterable, keyboard and mouse driven.

**Files:**
- Create: `src/ui/views/picker.rs`
- Modify: `src/ui/views/mod.rs`, `src/ui/hit.rs`

**Interfaces:**
- Consumes: theme (Task 1), `HitRegistry`/`HitTarget`.
- Produces:
  - `struct PickerItem { pub label: String, pub detail: String, pub accent: Option<Color> }`
  - `struct Picker { pub title: String, pub items: Vec<PickerItem>, pub filter: String, pub selected: usize }`
  - `fn filtered_indices(items: &[PickerItem], filter: &str) -> Vec<usize>`
  - `fn render_picker(f: &mut Frame, area: Rect, picker: &Picker, hits: &mut HitRegistry)`
  - `fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect`

`HitTarget` gains `PickerRow(usize)` — carrying the index **into the filtered list**, since that is what the user clicked.

- [ ] **Step 1: Write the failing tests**

```rust
    fn items() -> Vec<PickerItem> {
        ["prod-eu", "prod-us", "staging", "dev", "tst-wsdc"]
            .iter()
            .map(|n| PickerItem { label: n.to_string(), detail: String::new(), accent: None })
            .collect()
    }

    #[test]
    fn an_empty_filter_matches_everything() {
        assert_eq!(filtered_indices(&items(), "").len(), 5);
    }

    #[test]
    fn filtering_is_a_case_insensitive_substring_match() {
        assert_eq!(filtered_indices(&items(), "PROD"), vec![0, 1]);
        assert_eq!(filtered_indices(&items(), "wsdc"), vec![4]);
    }

    #[test]
    fn a_filter_matching_nothing_yields_an_empty_list_not_everything() {
        assert!(filtered_indices(&items(), "zzzz").is_empty());
    }

    #[test]
    fn centered_leaves_a_margin_on_every_side() {
        let a = centered(Rect { x: 0, y: 0, width: 100, height: 40 }, 60, 60);
        assert!(a.x > 0 && a.y > 0);
        assert!(a.x + a.width < 100);
        assert!(a.y + a.height < 40);
    }

    #[test]
    fn centered_on_a_tiny_area_does_not_underflow() {
        let a = centered(Rect { x: 0, y: 0, width: 3, height: 2 }, 60, 60);
        assert!(a.width <= 3 && a.height <= 2);
    }

    #[test]
    fn the_picker_draws_its_title_and_items() {
        let p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: String::new(),
            selected: 0,
        };
        let (text, _) = render_to_string(&p, 60, 16);
        assert!(text.contains("Clusters"), "title missing:\n{text}");
        assert!(text.contains("prod-eu"), "items missing:\n{text}");
    }

    #[test]
    fn each_visible_picker_row_is_clickable_and_maps_to_the_filtered_index() {
        let p = Picker {
            title: "Clusters".into(),
            items: items(),
            filter: "prod".into(),
            selected: 0,
        };
        let (_, hits) = render_to_string(&p, 60, 16);
        let mut found = Vec::new();
        for y in 0..16u16 {
            for x in 0..60u16 {
                if let Some(HitTarget::PickerRow(i)) = hits.hit(x, y) {
                    if !found.contains(i) { found.push(*i); }
                }
            }
        }
        assert_eq!(found, vec![0, 1], "filter 'prod' shows two rows, indices into the FILTERED list");
    }

    #[test]
    fn the_overlay_covers_what_is_beneath_it() {
        // Without Clear, the previous frame's content shows through the modal.
        let p = Picker { title: "T".into(), items: items(), filter: String::new(), selected: 0 };
        let (text, _) = render_over_noise(&p, 60, 16);
        assert!(!text.contains("XXXXXXXX"), "background bled through the overlay:\n{text}");
    }
```

Write the two helpers `render_to_string` and `render_over_noise` in the test module. `render_over_noise` fills the buffer with `X` before drawing the picker, which is how the `Clear` requirement gets tested.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib picker`
Expected: FAIL — `cannot find type Picker`.

- [ ] **Step 3: Write the implementation**

```rust
/// Case-insensitive substring match over item labels.
pub fn filtered_indices(items: &[PickerItem], filter: &str) -> Vec<usize> {
    if filter.is_empty() {
        return (0..items.len()).collect();
    }
    let needle = filter.to_lowercase();
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.label.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

/// A centred rectangle occupying the given percentage of `area`.
pub fn centered(area: Rect, pct_w: u16, pct_h: u16) -> Rect {
    let w = (area.width as u32 * pct_w as u32 / 100) as u16;
    let h = (area.height as u32 * pct_h as u32 / 100) as u16;
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w.min(area.width),
        height: h.min(area.height),
    }
}

pub fn render_picker(f: &mut Frame, area: Rect, picker: &Picker, hits: &mut HitRegistry) {
    let matches = filtered_indices(&picker.items, &picker.filter);

    // Clear first: without it the frame beneath shows through the modal.
    f.render_widget(Clear, area);

    let title = format!(" {} ", picker.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border_style(true))
        .title(Span::styled(title, theme::header_style()))
        .style(Style::default().bg(theme::ABYSS));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Filter line, then the list beneath it.
    let filter_line = Line::from(vec![
        Span::styled("⌕ ", theme::label_style()),
        Span::styled(picker.filter.clone(), theme::text_style()),
    ]);
    f.render_widget(
        Paragraph::new(filter_line),
        Rect { height: 1, ..inner },
    );

    let list_y = inner.y.saturating_add(1);
    let rows = inner.height.saturating_sub(1);
    for (row, &item_idx) in matches.iter().take(rows as usize).enumerate() {
        let y = list_y + row as u16;
        let item = &picker.items[item_idx];
        let selected = row == picker.selected;

        let accent = item.accent.unwrap_or(theme::MIST);
        let mut style = Style::default().fg(theme::PAPER);
        if selected {
            style = style.bg(theme::DUSK).add_modifier(Modifier::BOLD);
        }

        let line = Line::from(vec![
            Span::styled("▊ ", Style::default().fg(accent)),
            Span::styled(item.label.clone(), style),
            Span::styled(
                if item.detail.is_empty() { String::new() } else { format!("  {}", item.detail) },
                theme::muted_style(),
            ),
        ]);
        let row_area = Rect { x: inner.x, y, width: inner.width, height: 1 };
        f.render_widget(Paragraph::new(line).style(style), row_area);

        // z=1 so the overlay wins over the table beneath. The index is into
        // the FILTERED list, because that is what the user actually clicked.
        hits.push(row_area, 1, HitTarget::PickerRow(row));
    }
}
```

Each item's `accent` is painted on the `▊` marker — the cluster picker passes `cluster_hue(name)` so a cluster's colour is the same in the picker, the ribbon, and the status bar.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib picker`
Expected: PASS — 8 tests.

- [ ] **Step 5: Mutation checks**

1. Remove the `Clear` render. Confirm `the_overlay_covers_what_is_beneath_it` FAILS.
2. Make `filtered_indices` ignore the filter and return all indices. Confirm `filtering_is_a_case_insensitive_substring_match` FAILS.

Restore each, confirm green, paste all outputs.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/ui
git commit -m "feat: filterable modal picker overlay"
```

---

### Task 7: Server-side table columns

Gives every kind — including CRDs — the columns kubectl shows, without hand-writing a registry entry per kind.

**This is not a first-class kube-rs API.** Verified: there is no `kube::core::Table` in kube 4.2. It requires building a raw `http::Request` and decoding a hand-rolled response type. **Read `docs/superpowers/plan2-api-reference.md` section B4 before starting** — it contains the exact verified call.

**Files:**
- Create: `src/store/table.rs`
- Modify: `src/store/mod.rs`, `Cargo.toml` (add `http = "1"`)

**Interfaces:**
- Consumes: `kube::core::Request`, `Client::request`, `Api::resource_url` (all verified in the reference).
- Produces:
  - `struct TableColumn { pub name: String, pub priority: i32 }`
  - `struct TableData { pub columns: Vec<TableColumn>, pub rows: Vec<Vec<String>> }`
  - `fn decode_table(json: &serde_json::Value) -> anyhow::Result<TableData>`
  - `fn cell_to_string(v: &serde_json::Value) -> String`
  - `async fn fetch_table(client: &Client, resource_url: &str) -> anyhow::Result<TableData>`

- [ ] **Step 1: Write the failing tests**

The decode path is pure and gets thorough tests; `fetch_table` needs a cluster and is covered by Task 10's integration test.

```rust
    fn sample() -> serde_json::Value {
        serde_json::json!({
            "kind": "Table",
            "columnDefinitions": [
                {"name": "Name", "type": "string", "priority": 0},
                {"name": "Ready", "type": "string", "priority": 0},
                {"name": "Status", "type": "string", "priority": 0},
                {"name": "IP", "type": "string", "priority": 1}
            ],
            "rows": [
                {"cells": ["api-x2k", "2/2", "Running", "10.244.1.37"]},
                {"cells": ["api-q8w", "1/2", "CrashLoopBackOff", "10.244.1.38"]}
            ]
        })
    }

    #[test]
    fn decodes_columns_in_order() {
        let t = decode_table(&sample()).unwrap();
        let names: Vec<&str> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["Name", "Ready", "Status", "IP"]);
    }

    #[test]
    fn decodes_rows_as_strings() {
        let t = decode_table(&sample()).unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0], vec!["api-x2k", "2/2", "Running", "10.244.1.37"]);
    }

    #[test]
    fn retains_column_priority_for_narrow_terminals() {
        let t = decode_table(&sample()).unwrap();
        assert_eq!(t.columns[3].priority, 1, "priority>0 columns are kubectl's -o wide extras");
    }

    #[test]
    fn a_missing_priority_defaults_to_zero() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "Name", "type": "string"}],
            "rows": []
        });
        assert_eq!(decode_table(&json).unwrap().columns[0].priority, 0);
    }

    #[test]
    fn non_string_cells_are_rendered_not_dropped() {
        // Cells are heterogeneous: integers for restart counts, nulls, and
        // occasionally nested objects. None of them may vanish.
        assert_eq!(cell_to_string(&serde_json::json!("x")), "x");
        assert_eq!(cell_to_string(&serde_json::json!(7)), "7");
        assert_eq!(cell_to_string(&serde_json::json!(true)), "true");
        assert_eq!(cell_to_string(&serde_json::json!(null)), "");
        assert_eq!(cell_to_string(&serde_json::json!({"a": 1})), "{\"a\":1}");
    }

    #[test]
    fn a_row_with_fewer_cells_than_columns_is_padded_not_panicked() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "A"}, {"name": "B"}, {"name": "C"}],
            "rows": [{"cells": ["only-one"]}]
        });
        let t = decode_table(&json).unwrap();
        assert_eq!(t.rows[0].len(), 3, "ragged rows must be padded to the column count");
        assert_eq!(t.rows[0][0], "only-one");
        assert_eq!(t.rows[0][2], "");
    }

    #[test]
    fn a_row_with_more_cells_than_columns_is_truncated() {
        let json = serde_json::json!({
            "columnDefinitions": [{"name": "A"}],
            "rows": [{"cells": ["a", "b", "c"]}]
        });
        assert_eq!(decode_table(&json).unwrap().rows[0].len(), 1);
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(decode_table(&serde_json::json!({"nope": true})).is_err());
        assert!(decode_table(&serde_json::json!([])).is_err());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib table::` (note the `::` to avoid matching `views::table`)
Expected: FAIL — `cannot find function decode_table`.

- [ ] **Step 3: Add the dependency and implement**

Add to `Cargo.toml`:
```toml
http = "1"
```

```rust
use anyhow::{Context as _, anyhow};
use kube::Client;
use kube::api::ListParams;
use kube::core::Request as KubeRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// kubectl shows priority 0 always, and >0 only under `-o wide`.
    pub priority: i32,
}

#[derive(Debug, Clone, Default)]
pub struct TableData {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Vec<String>>,
}

/// Render one Table cell. Cells are heterogeneous — strings, integers for
/// restart counts, nulls, occasionally nested objects — and none may vanish.
pub fn cell_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Decode a `meta.k8s.io/v1 Table` response.
///
/// A CRD's declared printer columns and the rows the server returns are not
/// guaranteed to agree in length, so ragged rows are padded and over-long
/// rows truncated. Getting this wrong panics mid-render on someone's CRD.
pub fn decode_table(json: &serde_json::Value) -> anyhow::Result<TableData> {
    let defs = json
        .get("columnDefinitions")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("response has no columnDefinitions; not a Table"))?;

    let columns: Vec<TableColumn> = defs
        .iter()
        .map(|d| TableColumn {
            name: d.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
            priority: d.get("priority").and_then(|p| p.as_i64()).unwrap_or(0) as i32,
        })
        .collect();

    let width = columns.len();
    let rows = json
        .get("rows")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let mut cells: Vec<String> = row
                        .get("cells")
                        .and_then(|c| c.as_array())
                        .map(|c| c.iter().map(cell_to_string).collect())
                        .unwrap_or_default();
                    cells.resize(width, String::new());
                    cells
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(TableData { columns, rows })
}

/// Ask the API server to render a resource the way kubectl does.
///
/// kube 4.2 has no Table support, so this builds the request by hand and
/// sets the Accept header itself. Verified against kube 4.2; see
/// `docs/superpowers/plan2-api-reference.md` section B4.
pub async fn fetch_table(client: &Client, resource_url: &str) -> anyhow::Result<TableData> {
    let mut req = KubeRequest::new(resource_url)
        .list(&ListParams::default())
        .context("building the list request")?;
    req.headers_mut().insert(
        http::header::ACCEPT,
        http::HeaderValue::from_static("application/json;as=Table;v=1;g=meta.k8s.io"),
    );
    let json: serde_json::Value = client
        .request(req)
        .await
        .with_context(|| format!("requesting a Table for {resource_url}"))?;
    decode_table(&json)
}
```

Note `cells.resize(width, ..)` handles both padding and truncation in one call.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib table::`
Expected: PASS — 8 tests.

- [ ] **Step 5: Mutation check**

Remove the row padding so ragged rows keep their original length. Confirm `a_row_with_fewer_cells_than_columns_is_padded_not_panicked` FAILS. Restore, confirm green.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/store Cargo.toml Cargo.lock
git commit -m "feat: decode server-side Table responses for kubectl-equivalent columns"
```

---

### Task 8: Cluster switching — connect, teardown, reconnect

Where the pieces meet. This is the task with real concurrency risk; read Plan 1's `src/main.rs` and `src/store/watch.rs` fully before starting.

**Files:**
- Create: `src/app/session.rs`
- Modify: `src/main.rs`, `src/store/watch.rs`

**Interfaces:**
- Consumes: `ClusterRegistry` (Task 2), `WatchHandles` (Task 3), `spawn_watch`, `connect_with`.
- Produces:
  - `enum SessionEvent { Connecting(ClusterId), Connected(ClusterId), ConnectFailed { id: ClusterId, reason: String } }`
  - `fn is_deliberate_abort(e: &tokio::task::JoinError) -> bool`
  - `async fn switch_cluster(...) -> ()` — aborts existing watches, connects, respawns

- [ ] **Step 1: Write the failing test for the supervisor fix**

Task 3 flagged this: aborting a task makes its `JoinHandle` return `Err(JoinError)` with `is_cancelled()` true, and Plan 1's supervisor treats **any** `Err` as a reason to send `Quit`. Left unfixed, the first cluster switch quits the app.

```rust
    #[tokio::test]
    async fn a_deliberately_aborted_watch_is_not_treated_as_a_failure() {
        let h = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        h.abort();
        let err = h.await.expect_err("an aborted task must join as Err");
        assert!(
            is_deliberate_abort(&err),
            "switching clusters aborts watches; treating that as a crash would quit the app"
        );
    }

    #[tokio::test]
    async fn a_panicking_watch_is_still_treated_as_a_failure() {
        let h = tokio::spawn(async { panic!("boom") });
        let err = h.await.expect_err("a panicking task must join as Err");
        assert!(!is_deliberate_abort(&err), "a real panic must not be mistaken for a cluster switch");
    }
```

- [ ] **Step 2: Run to verify failure, then implement**

```rust
/// Distinguish "we aborted this watch on purpose" from "this watch died".
pub fn is_deliberate_abort(e: &tokio::task::JoinError) -> bool {
    e.is_cancelled()
}
```

Update the supervisor in `main.rs` to send `Error` + `Quit` only when `!is_deliberate_abort(&e)`.

- [ ] **Step 3: Implement switching**

The sequence, in this order:

1. `handles.abort_all()` — stop every watch belonging to the old cluster **before** touching the store, so no in-flight delta can land in the new cluster's cache.
2. Clear the store.
3. `registry.set_state(&id, ConnectionState::Connecting)`, emit `SessionEvent::Connecting`, and **redraw** — connecting a VPN-unreachable cluster can take tens of seconds and the UI must show that immediately rather than appearing frozen.
4. `connect_with` on a spawned task, never inline — the event loop must stay responsive.
5. On success: `set_state(Connected)`, `set_active`, spawn the watch, push its handle. On failure: `set_state(Failed { reason })` and leave the previous cluster active.

**Default scope on connect is all namespaces** (`None` to `spawn_watch`), per the design decision: contexts frequently set no namespace and `default` is empty on these clusters.

- [ ] **Step 4: Verify the leak is actually fixed**

```rust
    #[tokio::test]
    async fn switching_clusters_repeatedly_does_not_accumulate_watches() {
        let mut handles = WatchHandles::new();
        for _ in 0..20 {
            handles.abort_all();
            handles.push(tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }));
        }
        assert_eq!(handles.len(), 1, "twenty switches must leave one live watch, not twenty");
    }
```

- [ ] **Step 5: Mutation check**

Make `is_deliberate_abort` always return `false`. Confirm `a_deliberately_aborted_watch_is_not_treated_as_a_failure` FAILS. Restore, confirm green.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/app src/main.rs src/store/watch.rs
git commit -m "feat: cluster switching with watch teardown and abort-aware supervision"
```

---

### Task 9: Wiring — overlays, focus, and input

**Files:**
- Modify: `src/main.rs`, `src/app/input.rs`, `src/app/mod.rs`, `src/ui/views/status.rs`

**Interfaces:**
- Produces:
  - `enum Overlay { None, ClusterPicker(Picker), NamespacePicker(Picker) }`
  - `Action` gains `OpenClusterPicker`, `OpenNamespacePicker`, `PickerSelect(usize)`, `PickerFilterChar(char)`, `PickerBackspace`, `ClosePicker`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn c_opens_the_cluster_picker_and_n_the_namespace_picker() {
        let r = registry();
        assert_eq!(action_for(&key(KeyCode::Char('c')), &r), Action::OpenClusterPicker);
        assert_eq!(action_for(&key(KeyCode::Char('n')), &r), Action::OpenNamespacePicker);
    }

    #[test]
    fn clicking_the_ribbon_opens_the_cluster_picker() {
        let mut r = HitRegistry::new();
        r.push(Rect { x: 0, y: 0, width: 1, height: 24 }, 0, HitTarget::Ribbon);
        assert_eq!(action_for(&click(0, 5), &r), Action::OpenClusterPicker);
    }

    #[test]
    fn clicking_a_picker_row_selects_it() {
        let mut r = HitRegistry::new();
        r.push(Rect { x: 10, y: 5, width: 40, height: 1 }, 1, HitTarget::PickerRow(3));
        assert_eq!(action_for(&click(20, 5), &r), Action::PickerSelect(3));
    }

    #[test]
    fn escape_closes_an_open_picker() {
        assert_eq!(action_for(&key(KeyCode::Esc), &registry()), Action::ClosePicker);
    }
```

**Note the conflict:** Plan 1 binds `Esc` to `Quit`, and `j`/`k` to navigation. With a picker open, `Esc` must close it and typing must go to the filter. `action_for` is currently stateless.

**Resolve it by making the overlay state an explicit parameter** — `action_for(event, hits, overlay_open: bool)` — rather than by having the caller reinterpret actions after the fact. A stateless mapper that returns actions the caller then second-guesses is how input handling becomes unpredictable. Update Plan 1's 14 input tests to pass `false`; do not weaken them.

- [ ] **Step 2-4: Implement, verify, and wire**

- Route filter keystrokes to the picker when one is open.
- Draw order: ribbon, then main content, then the overlay last (so its z=1 hit zones win).
- Status bar: show the active cluster's name in its own hue, plus connection state.
- The `-A`/`-n` flags still apply to the *initial* connect; the picker overrides them afterwards.

- [ ] **Step 5: Mutation check**

Make `action_for` ignore `overlay_open`. Confirm `escape_closes_an_open_picker` or the equivalent overlay-routing test FAILS. Restore, confirm green.

- [ ] **Step 6: Verify and commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src
git commit -m "feat: picker overlays, focus routing, and themed status bar"
```

---

### Task 10: Integration and manual verification

**Files:**
- Modify: `tests/integration_kind.rs`
- Modify: `README.md`

- [ ] **Step 1: Add integration tests** (`#[ignore]`d, serialised by the existing `cluster_lock`)

1. `fetch_table_returns_kubectl_equivalent_columns` — call `fetch_table` against pods in `demo`, assert the columns include `Name`, `Ready`, `Status` and that at least one row is present.
2. `switching_clusters_aborts_the_previous_watch` — spawn a watch, abort via `WatchHandles`, confirm the store stops receiving deltas after a subsequent change.

- [ ] **Step 2: Manual verification — requires a TTY and a real cluster**

This cannot be done by an agent. Record it in the README as the checklist a human runs:

1. `cargo run` — ribbon visible, coloured; table populated; status bar shows cluster in its hue.
2. Press `c` — cluster picker opens over the table, listing all contexts; filter narrows it; `Esc` closes.
3. Select a different cluster — status shows `connecting`, then the ribbon changes colour and the table repopulates.
4. Switch back and forth ten times — memory stays flat (`ps -o rss= -p $(pgrep -f target/debug/kube)`), confirming no watch leak.
5. Select an unreachable cluster — it reports `failed` with a reason, and the previous cluster stays active.
6. Press `n` — namespace picker lists namespaces; selecting one re-scopes the table.
7. Scroll a table of several hundred pods — smooth, and clicking a visible row selects that row.
8. `q` exits and the shell is intact.

- [ ] **Step 3: Commit**

```bash
git add tests README.md
git commit -m "test: integration coverage for server-side tables and watch teardown"
```

---

## Definition of Done

- [ ] `cargo test` passes with no cluster present.
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean.
- [ ] Listing clusters performs no network I/O.
- [ ] Switching clusters twenty times leaves exactly one live watch.
- [ ] An unreachable cluster fails visibly without blocking the UI or losing the active cluster.
- [ ] Render cost is O(viewport): a 5000-object list formats at most a screenful of rows.
- [ ] No status colour is ever drawn in a chrome token, or vice versa.
- [ ] Idle CPU remains 0% — no timers, no animation frames.

## Carried to Plan 3

Sidebar kind tree with live counts; detail pane with Overview / YAML / Events tabs (**decided: `serde_norway` for YAML serialisation**); per-column header sort targets (**use the two-stage split from API reference D14 — `Layout::horizontal(widths).split(area)` alone is only correct when the selection symbol width is zero**); the `▎` change marker for rows whose state changed since last view.
