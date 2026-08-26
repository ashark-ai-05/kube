# kube — Plan 3: Kind tree, server-side columns, and the detail pane

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every resource kind in a cluster reachable and inspectable — a sidebar tree with live counts, kubectl-equivalent columns for any kind including CRDs, and a detail pane with Overview, YAML and Events.

**Architecture:** Extends Plan 2. `cluster` gains discovery of watchable kinds. `store` gains multi-kind watching under an explicit cap, and a supervisor that distinguishes RBAC denial from transient failure. `ui` gains a sidebar tree, a tabbed detail pane, and a shared geometry module for hit-testing compound widgets ratatui does not measure for us.

**Tech Stack:** Rust (edition 2024), `kube` 4.2, `k8s-openapi` 0.28, `ratatui` 0.30, `crossterm` 0.29, `tokio` 1.x, `http` 1, **`serde_norway` 0.9 (new)**, **`unicode-width` 0.2 (new)**.

Plan 3 of 5 for v1. Source spec: `docs/superpowers/specs/2026-08-25-kube-tui-design.md`.
**Verified API reference: `docs/superpowers/plan3-api-reference.md`** — every kube/ratatui call this plan relies on was confirmed by compiling it, and two probes were executed rather than merely type-checked. Consult it rather than recalling APIs; this project's plans have been wrong about that surface five times.

## Context: what Plans 1 and 2 established

Plan 1 shipped a single-cluster pod browser. Plan 2 added multi-cluster switching, a theme system, the cluster ribbon, O(viewport) rendering, and `fetch_table` (built, not wired in). Both are merged; 264 tests pass.

Three things carried forward that shape this plan:

1. **`fetch_table` exists and is unwired.** `src/store/table.rs` can ask the API server to render any kind kubectl-style. Task 6 connects it.
2. **The binary has never run in a real terminal, and no integration test has ever executed.** That remains true entering this plan. Nothing here changes it; Task 10 extends the manual checklist.
3. **One bug class caused four Critical/Important findings across Plans 1-2: two sources of truth requiring a sync step.** Watch for it. The fix is always to collapse to one, never to test that the copies agree.

## Global Constraints

These apply to every task and are implicitly part of every task's requirements.

- **Rust edition 2024.** Minimum toolchain 1.85.
- **Existing dependencies are fixed.** Two new ones are authorised, both verified to resolve and compile: `serde_norway = "0.9"` and `unicode-width = "0.2"`. Nothing else may be added.
- **v1 is read-only.** Nothing in the binary may mutate cluster state. No `create`, `patch`, `replace`, `delete`, `PostParams`, `PatchParams`, `DeleteParams`.
- **Credentials are never read, logged or printed.** `Session` must not derive `Debug`. `auth_info.token` is opaque.
- **The render closure passed to `term.draw` must be synchronous**, perform no I/O and acquire no locks. Read snapshots before drawing.
- **Never render on a fixed tick.** No timers, no animation frames. Idle stays 0% CPU.
- **Formatting cost stays O(viewport), not O(objects).**
- **Chrome uses the cool hue family; status signals use the warm-plus-green family.** Never colour a status with a chrome token or vice versa.
- **Never write to stdout/stderr while the alternate screen is active.**
- **No `unwrap()`/`expect()` outside tests** except where a comment justifies the invariant.
- **Tests must not require a cluster, network or TTY** except the `#[ignore]`d integration tests.
- **TDD, two commits per task:** commit the failing tests alone first (`test: <thing> (failing)`), then the implementation. This makes RED auditable from git history rather than taken on trust.
- **Mutation-check each task's central guarantee** before committing: break the implementation deliberately, confirm the covering test fails, restore, confirm it passes. Paste both.
- **On fixtures:** five fixtures written for earlier plans were vacuous — the data chosen made correct and incorrect implementations produce identical output. Before finalising any fixture ask: *would a wrong implementation give a different answer with this data?* Prefer values where the quantities being distinguished diverge — indices that are not positions, counts that are not zero, offsets that are not the identity.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` clean before every commit.
- **Stage explicit paths when committing**, never `git add -A`.

## Three findings from the API probe that shape this plan

1. **Eager watching is not throttled client-side.** `Client::clone()` is a cheap handle clone over a tower `Buffer` (1024 requests in flight, not a connection cap); hyper's pool is left at `usize::MAX`. There is no client-side reason to watch lazily. The plan therefore watches eagerly with an explicit **configurable cap** as the only guard against pathological clusters — a knob, not a code-path fork.
2. **`watcher::Error` treats all five variants as retryable**, so RBAC denial is indistinguishable from a flaky cluster at that level. The supervisor must match one level deeper into `kube::Error::Api(Status)` and check `is_forbidden()`. On a corporate cluster, lacking access to some kinds is the common case; without this, those kinds retry forever and look identical to an outage.
3. **`serde_norway` output needs no post-processing.** It leads with `apiVersion`/`kind`/`metadata` and alphabetises the rest, matching `kubectl get -o yaml`. Budget zero time for making the YAML readable.

## File Structure

| File | Responsibility |
|---|---|
| `src/store/rbac.rs` | Classify a watcher error as forbidden vs retryable |
| `src/cluster/discovery.rs` | Enumerate watchable kinds for a cluster |
| `src/store/multi.rs` | Watch many kinds under a cap; per-kind availability |
| `src/ui/tree.rs` | Sidebar tree model: groups, kinds, expansion, flattening |
| `src/ui/views/sidebar.rs` | Sidebar rendering and hit-testing |
| `src/ui/geometry.rs` | Measure sub-regions of compound widgets ratatui does not expose |
| `src/ui/views/detail.rs` | Tabbed detail pane: Overview, YAML, Events |
| `src/store/events.rs` | Events for a specific object |
| `src/store/columns.rs` | **Modified.** Prefer server-side Table columns |
| `src/ui/views/table.rs` | **Modified.** Column sorting |
| `src/app/session.rs` | **Modified.** Active kind, discovered kinds |
| `src/main.rs` | **Modified.** Wiring |

---

### Task 1: Distinguish RBAC denial from transient failure

The foundation for everything else: with 20+ corporate clusters, lacking access to some kinds is normal. Without this, a forbidden kind retries forever and is indistinguishable in the UI from a cluster having a bad moment.

`watcher::Error`'s own doc comment says all five variants are "considered retryable", so the outer enum cannot be trusted. The classification must reach into `kube::Error::Api(Status)`.

**Files:**
- Create: `src/store/rbac.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: `kube::runtime::watcher::Error`, `kube::Error`, `kube::core::Status`.
- Produces:
  - `enum WatchFailure { Forbidden { reason: String }, NotFound, Retryable }`
  - `fn classify(err: &watcher::Error) -> WatchFailure`

- [ ] **Step 1: Write the failing tests**

Building a real `watcher::Error` requires constructing the inner `kube::Error`. Consult `plan3-api-reference.md` section B6 for the verified construction before writing these — it shows the exact variant path and `Status` shape.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: u16, reason: &str) -> kube::Error {
        kube::Error::Api(kube::core::Status {
            status: Some("Failure".into()),
            code,
            message: format!("{reason} is forbidden"),
            reason: reason.into(),
            details: None,
        })
    }

    #[test]
    fn a_403_is_classified_as_forbidden_not_retryable() {
        let e = watcher::Error::WatchStartFailed(api_error(403, "Forbidden"));
        match classify(&e) {
            WatchFailure::Forbidden { reason } => assert!(reason.contains("forbidden")),
            other => panic!("403 must not be {other:?} — it would retry forever"),
        }
    }

    #[test]
    fn a_404_is_not_confused_with_a_403() {
        // A kind that vanished (CRD uninstalled) is not the same as one we
        // are not allowed to see, and the sidebar should say so differently.
        assert!(matches!(
            classify(&watcher::Error::WatchStartFailed(api_error(404, "NotFound"))),
            WatchFailure::NotFound
        ));
    }

    #[test]
    fn a_500_is_retryable() {
        assert!(matches!(
            classify(&watcher::Error::WatchStartFailed(api_error(500, "InternalError"))),
            WatchFailure::Retryable
        ));
    }

    #[test]
    fn a_transport_error_is_retryable() {
        // Not an ApiError at all — a connection blip. Must not be mistaken
        // for a permission problem, or a VPN hiccup would permanently mark a
        // kind unavailable until the next cluster switch.
        let e = watcher::Error::WatchFailed(kube::Error::Discovery(
            kube::error::DiscoveryError::MissingKind("Pod".into()),
        ));
        assert!(matches!(classify(&e), WatchFailure::Retryable));
    }

    #[test]
    fn forbidden_is_detected_in_every_watcher_variant_that_can_carry_it() {
        // watcher::Error has five variants and the docs call them all
        // retryable; a 403 can arrive through more than one of them.
        for e in [
            watcher::Error::WatchStartFailed(api_error(403, "Forbidden")),
            watcher::Error::WatchFailed(api_error(403, "Forbidden")),
            watcher::Error::InitialListFailed(api_error(403, "Forbidden")),
        ] {
            assert!(
                matches!(classify(&e), WatchFailure::Forbidden { .. }),
                "403 missed in {e:?}"
            );
        }
    }
}
```

Check the exact variant names against the API reference before writing — if `watcher::Error`'s variants differ from those above, use the real ones and say so in your report.

- [ ] **Step 2: Run the tests, observe the failure, commit them alone**

Run: `cargo test --lib rbac`
Expected: FAIL — `cannot find function classify`.
Commit: `test: classify watch failures (failing)`

- [ ] **Step 3: Implement**

Match every `watcher::Error` variant that carries a `kube::Error`, then match `kube::Error::Api(status)` and branch on `status.code`. Anything else is `Retryable` — defaulting to retry is the safe direction, since wrongly marking a kind unavailable hides data, while wrongly retrying only wastes requests.

- [ ] **Step 4: Verify and mutation-check**

Run: `cargo test --lib rbac` — expect 5 passing.

Mutation: make `classify` return `Retryable` unconditionally. Confirm `a_403_is_classified_as_forbidden_not_retryable` and `forbidden_is_detected_in_every_watcher_variant_that_can_carry_it` FAIL. Restore, confirm green. Paste both.

- [ ] **Step 5: Commit**

```bash
cargo test && cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/store/rbac.rs src/store/mod.rs
git commit -m "feat: distinguish RBAC denial from transient watch failure"
```

---

### Task 2: Discover watchable kinds

**Files:**
- Create: `src/cluster/discovery.rs`
- Modify: `src/cluster/mod.rs`

**Interfaces:**
- Consumes: `kube::discovery::{Discovery, Scope, verbs}` — see API reference A1/A2/B5.
- Produces:
  - `struct KindInfo { pub gvk: GroupVersionKind, pub resource: ApiResource, pub namespaced: bool, pub group_label: String }`
  - `fn group_label_for(group: &str) -> String` — `""` becomes `"core"`
  - `fn is_browsable(caps: &ApiCapabilities) -> bool` — requires both `list` and `watch`
  - `async fn discover_kinds(client: &Client) -> anyhow::Result<Vec<KindInfo>>`

- [ ] **Step 1: Write the failing tests**

`discover_kinds` needs a cluster and is covered by Task 10's integration tests. The pure parts get thorough tests here.

```rust
    #[test]
    fn the_core_group_gets_a_readable_label() {
        // The core group's name is the empty string; showing a blank sidebar
        // heading would be worse than useless.
        assert_eq!(group_label_for(""), "core");
        assert_eq!(group_label_for("apps"), "apps");
        assert_eq!(group_label_for("networking.k8s.io"), "networking.k8s.io");
    }

    #[test]
    fn a_kind_needs_both_list_and_watch_to_be_browsable() {
        assert!(is_browsable(&caps(&["list", "watch", "get"])));
        assert!(!is_browsable(&caps(&["list", "get"])), "no watch: counts would never update");
        assert!(!is_browsable(&caps(&["watch"])), "no list: the initial population would be empty");
        assert!(!is_browsable(&caps(&[])));
    }

    #[test]
    fn subresources_and_write_only_verbs_do_not_make_a_kind_browsable() {
        assert!(!is_browsable(&caps(&["create", "delete", "patch"])));
    }
```

Write a `caps(&[&str]) -> ApiCapabilities` helper. Check `ApiCapabilities`'s real shape in the API reference — it has `scope`, `subresources` and `operations`, and `supports_operation` is the intended accessor.

- [ ] **Step 2: Run, observe failure, commit the tests alone**

- [ ] **Step 3: Implement**

`discover_kinds` iterates `disc.groups()` and `group.recommended_resources()` — the preferred version per group, which is what a resource browser wants. Filter with `is_browsable`. Record `namespaced` from `matches!(caps.scope, Scope::Namespaced)`.

- [ ] **Step 4: Mutation-check**

Make `is_browsable` require only `list`. Confirm `a_kind_needs_both_list_and_watch_to_be_browsable` FAILS. Restore, confirm green.

- [ ] **Step 5: Commit** — `feat: discover the kinds a cluster can browse`

---

### Task 3: Watch many kinds under a cap

The probe established there is no client-side throttle, so this watches eagerly. The cap exists only as a guard against clusters with hundreds of CRDs, and is a configuration value, not a second code path.

**Files:**
- Create: `src/store/multi.rs`
- Modify: `src/store/mod.rs`

**Interfaces:**
- Consumes: `KindInfo` (Task 2), `classify`/`WatchFailure` (Task 1), `spawn_watch`, `WatchHandles`.
- Produces:
  - `const DEFAULT_MAX_EAGER_WATCHES: usize = 40;`
  - `enum KindAvailability { Watching, Unavailable { reason: String }, NotWatched }`
  - `fn kinds_to_watch(kinds: &[KindInfo], cap: usize) -> (Vec<&KindInfo>, usize)` — returns those to watch and how many were skipped
  - `fn prioritise(kinds: &mut Vec<KindInfo>)` — common workload kinds first

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn every_kind_is_watched_when_under_the_cap() {
        let kinds = make_kinds(&["Pod", "Deployment", "Service"]);
        let (watched, skipped) = kinds_to_watch(&kinds, 40);
        assert_eq!(watched.len(), 3);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn the_cap_bounds_the_watch_count_and_reports_what_was_dropped() {
        // Silent truncation would read as "this cluster has 40 kinds".
        let kinds = make_kinds(&(0..100).map(|i| format!("Kind{i}")).collect::<Vec<_>>());
        let (watched, skipped) = kinds_to_watch(&kinds, 40);
        assert_eq!(watched.len(), 40);
        assert_eq!(skipped, 60, "the count dropped must be reportable to the user");
    }

    #[test]
    fn the_kinds_people_actually_look_at_survive_the_cap() {
        // With a cap, which kinds get dropped matters. Pods being cut while
        // some operator's CRD survives would make the tool useless on exactly
        // the clusters where the cap engages.
        let mut kinds = make_kinds(&["Widget", "Gizmo", "Pod", "Doodad", "Deployment"]);
        prioritise(&mut kinds);
        let names: Vec<&str> = kinds.iter().map(|k| k.gvk.kind.as_str()).collect();
        assert_eq!(&names[..2], &["Pod", "Deployment"]);
    }

    #[test]
    fn prioritising_is_stable_for_kinds_of_equal_rank() {
        let mut kinds = make_kinds(&["Zebra", "Apple", "Pod"]);
        prioritise(&mut kinds);
        let names: Vec<&str> = kinds.iter().map(|k| k.gvk.kind.as_str()).collect();
        assert_eq!(names, vec!["Pod", "Zebra", "Apple"], "unranked kinds keep discovery order");
    }

    #[test]
    fn a_cap_of_zero_watches_nothing_rather_than_panicking() {
        let kinds = make_kinds(&["Pod"]);
        let (watched, skipped) = kinds_to_watch(&kinds, 0);
        assert!(watched.is_empty());
        assert_eq!(skipped, 1);
    }
```

- [ ] **Step 2: Run, observe failure, commit the tests alone**

- [ ] **Step 3: Implement**

`prioritise` sorts by a rank table (Pod, Deployment, StatefulSet, DaemonSet, Service, Ingress, ConfigMap, Secret, Job, CronJob, Node, Namespace, PVC, then everything else) using a **stable** sort so unranked kinds keep discovery order.

Spawn one watch per selected kind, each pushed into `WatchHandles` so a cluster switch aborts them all — Plan 2 established that machinery and it must be reused, not duplicated.

On a watch error, call `classify`. `Forbidden` and `NotFound` mark the kind `Unavailable { reason }` and stop retrying; `Retryable` keeps the existing backoff behaviour.

- [ ] **Step 4: Mutation-checks**

1. `kinds_to_watch` ignores the cap. Confirm the cap test FAILS.
2. `prioritise` uses an unstable sort or no rank table. Confirm the priority tests FAIL.
3. A `Forbidden` classification is treated as retryable. Confirm a test asserting the kind is marked `Unavailable` FAILS.

- [ ] **Step 5: Commit** — `feat: watch every discovered kind under an explicit cap`

---

### Task 4: The sidebar tree model

Pure data structure and flattening — no rendering. The flattened view is what both the renderer and the hit-tester consume, so they cannot disagree about what is on screen.

**Files:**
- Create: `src/ui/tree.rs`
- Modify: `src/ui/mod.rs`

**Interfaces:**
- Produces:
  - `struct TreeGroup { pub label: String, pub expanded: bool, pub kinds: Vec<TreeKind> }`
  - `struct TreeKind { pub gvk: GroupVersionKind, pub label: String, pub count: Option<usize>, pub availability: KindAvailability }`
  - `struct KindTree { pub groups: Vec<TreeGroup>, pub selected: usize }`
  - `enum TreeRow<'a> { Group { index: usize, group: &'a TreeGroup }, Kind { group_index: usize, kind: &'a TreeKind } }`
  - `fn flatten(tree: &KindTree) -> Vec<TreeRow<'_>>`
  - `KindTree::toggle(&mut self, row: usize)`, `fn selected_kind(&self) -> Option<&TreeKind>`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn a_collapsed_group_contributes_only_its_own_row() {
        let t = tree(&[("core", false, &["Pod", "Service"])]);
        assert_eq!(flatten(&t).len(), 1);
    }

    #[test]
    fn an_expanded_group_contributes_its_kinds_in_order() {
        let t = tree(&[("core", true, &["Pod", "Service"])]);
        let rows = flatten(&t);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], TreeRow::Group { .. }));
        assert!(matches!(&rows[1], TreeRow::Kind { kind, .. } if kind.label == "Pod"));
        assert!(matches!(&rows[2], TreeRow::Kind { kind, .. } if kind.label == "Service"));
    }

    #[test]
    fn flattening_interleaves_groups_correctly() {
        // Two expanded groups and one collapsed between them — the classic
        // off-by-one shape. Row indices must map back to the right kinds.
        let t = tree(&[
            ("core", true, &["Pod"]),
            ("apps", false, &["Deployment"]),
            ("batch", true, &["Job", "CronJob"]),
        ]);
        let rows = flatten(&t);
        assert_eq!(rows.len(), 6);
        assert!(matches!(&rows[3], TreeRow::Group { group, .. } if group.label == "batch"));
        assert!(matches!(&rows[5], TreeRow::Kind { kind, .. } if kind.label == "CronJob"));
    }

    #[test]
    fn toggling_a_group_row_changes_only_that_group() {
        let mut t = tree(&[("core", true, &["Pod"]), ("apps", true, &["Deployment"])]);
        t.toggle(0);
        assert!(!t.groups[0].expanded);
        assert!(t.groups[1].expanded, "collapsing one group must not collapse another");
    }

    #[test]
    fn toggling_a_kind_row_does_nothing_rather_than_collapsing_its_parent() {
        let mut t = tree(&[("core", true, &["Pod"])]);
        t.toggle(1);
        assert!(t.groups[0].expanded);
    }

    #[test]
    fn toggling_a_row_past_the_end_is_a_no_op() {
        let mut t = tree(&[("core", true, &["Pod"])]);
        t.toggle(99);
        assert_eq!(flatten(&t).len(), 2);
    }

    #[test]
    fn the_selected_kind_follows_the_flattened_row() {
        let mut t = tree(&[("core", true, &["Pod", "Service"])]);
        t.selected = 2;
        assert_eq!(t.selected_kind().map(|k| k.label.as_str()), Some("Service"));
        t.selected = 0;
        assert_eq!(t.selected_kind(), None, "a group row selects no kind");
    }
```

- [ ] **Step 2: Run, observe failure, commit the tests alone**

- [ ] **Step 3: Implement, then Step 4: mutation-check**

Mutation: make `flatten` always include a group's kinds regardless of `expanded`. Confirm `a_collapsed_group_contributes_only_its_own_row` FAILS.

- [ ] **Step 5: Commit** — `feat: sidebar tree model with flattening`

---

### Task 5: Shared widget geometry, and sidebar rendering

Ratatui does not expose post-layout geometry for compound widgets — Plan 2 found this for `Table` columns, and the probe confirms it again for `Tabs` and for tree indentation. Rather than each view reimplementing measurement, this task extracts a small shared module.

**Files:**
- Create: `src/ui/geometry.rs`
- Create: `src/ui/views/sidebar.rs`
- Modify: `src/ui/mod.rs`, `src/ui/views/mod.rs`

**Interfaces:**
- Produces:
  - `fn tab_spans(labels: &[&str], area: Rect, divider_width: u16) -> Vec<Rect>` — per-tab clickable rects, using `unicode_width::UnicodeWidthStr` for correct widths
  - `fn render_sidebar(f, area, tree: &mut KindTree, hits: &mut HitRegistry)`
  - `HitTarget` gains `TreeRow(usize)`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn tab_rects_tile_left_to_right_without_overlap() {
        let rects = tab_spans(&["Overview", "YAML", "Events"], rect(0, 0, 60, 1), 3);
        assert_eq!(rects.len(), 3);
        for pair in rects.windows(2) {
            assert!(pair[0].x + pair[0].width <= pair[1].x, "tabs overlap: {pair:?}");
        }
    }

    #[test]
    fn tab_widths_account_for_wide_characters() {
        // A CJK label is two cells per character; measuring in chars would
        // make every tab after it clickable at the wrong offset.
        let narrow = tab_spans(&["ab", "cd"], rect(0, 0, 40, 1), 1);
        let wide = tab_spans(&["日本", "cd"], rect(0, 0, 40, 1), 1);
        assert!(wide[1].x > narrow[1].x, "wide label did not widen its tab");
    }

    #[test]
    fn tabs_that_do_not_fit_are_dropped_rather_than_drawn_off_screen() {
        let rects = tab_spans(&["Overview", "YAML", "Events"], rect(0, 0, 10, 1), 3);
        for r in &rects {
            assert!(r.x + r.width <= 10, "tab {r:?} extends past the area");
        }
    }

    #[test]
    fn each_visible_sidebar_row_registers_a_hit_zone_at_its_own_index() {
        let mut t = tree(&[("core", true, &["Pod", "Service"])]);
        let (text, hits) = render_to_string(&mut t, 24, 10);
        for (row, expected) in [(0usize, "core"), (1, "Pod"), (2, "Service")] {
            let y = (row + 1) as u16; // border
            assert!(text.lines().nth(y as usize).unwrap().contains(expected));
            assert_eq!(hits.hit(2, y), Some(&HitTarget::TreeRow(row)));
        }
    }

    #[test]
    fn an_unavailable_kind_says_so_instead_of_showing_a_count() {
        // On a corporate cluster the user lacks RBAC on some kinds. Showing a
        // perpetual blank or zero would read as "this kind is empty".
        let mut t = tree_with_unavailable("Secret", "forbidden");
        let (text, _) = render_to_string(&mut t, 30, 10);
        assert!(text.contains("Secret"));
        assert!(text.to_lowercase().contains("forbidden") || text.contains("—"),
                "unavailable kind rendered as if it were merely empty:\n{text}");
    }
```

- [ ] **Step 2: Run, observe failure, commit the tests alone**

- [ ] **Step 3: Implement**

Render each flattened row: groups with `▸`/`▾` disclosure in `theme::label_style()`, kinds indented two cells with their count in `theme::count_style()`. An unavailable kind renders its reason in `theme::muted_style()` where the count would go. Register `TreeRow(index)` per visible row.

Reuse `ui::scroll` (Plan 2) for the sidebar's own scrolling — with 40 kinds the list will exceed the pane. Do not reimplement it.

- [ ] **Step 4: Mutation-checks**

1. `tab_spans` measures in `chars()` rather than display width. Confirm the CJK test FAILS.
2. Sidebar hit zones registered at `row + 1`. Confirm the hit-zone test FAILS.
3. Unavailable kinds render a count of 0. Confirm that test FAILS.

- [ ] **Step 5: Commit** — `feat: shared widget geometry and sidebar rendering`

---

### Task 6: Server-side columns and sorting in the live table

Wires `fetch_table` (built in Plan 2, never called) into the view, so any kind — including CRDs — renders with kubectl's own columns. Adds click-to-sort using the two-stage column layout the Plan 2 reference established.

**Files:**
- Modify: `src/store/columns.rs`, `src/ui/views/table.rs`, `src/store/table.rs`

**Interfaces:**
- Produces:
  - `enum ColumnSource { Builtin(Vec<Column>), Server(TableData) }`
  - `fn column_offsets(widths: &[Constraint], area: Rect, column_spacing: u16, selection_width: u16) -> Vec<Rect>`
  - `struct SortState { pub column: usize, pub descending: bool }`
  - `fn sort_rows(rows: &mut Vec<Vec<String>>, sort: &SortState)`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn column_offsets_account_for_the_selection_column_and_spacing() {
        // Plan 2's reference established that Layout::horizontal alone is only
        // correct when the selection symbol is zero-width. A naive split makes
        // every column header click land on the wrong column.
        let widths = [Constraint::Length(10), Constraint::Length(8), Constraint::Fill(1)];
        let plain = column_offsets(&widths, rect(0, 0, 60, 1), 1, 0);
        let with_sel = column_offsets(&widths, rect(0, 0, 60, 1), 1, 2);
        assert!(with_sel[0].x > plain[0].x, "selection column not reserved");
        for pair in with_sel.windows(2) {
            assert!(pair[0].x + pair[0].width <= pair[1].x, "columns overlap");
        }
    }

    #[test]
    fn sorting_is_stable_and_reversible() {
        let mut rows = vec![
            vec!["b".into(), "2".into()],
            vec!["a".into(), "1".into()],
            vec!["c".into(), "2".into()],
        ];
        sort_rows(&mut rows, &SortState { column: 1, descending: false });
        assert_eq!(rows[0][0], "a");
        assert_eq!(&[rows[1][0].as_str(), rows[2][0].as_str()], &["b", "c"],
                   "equal keys must keep their original order");
    }

    #[test]
    fn sorting_descending_reverses_the_order() {
        let mut rows = vec![vec!["a".into()], vec!["c".into()], vec!["b".into()]];
        sort_rows(&mut rows, &SortState { column: 0, descending: true });
        assert_eq!(rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(), vec!["c", "b", "a"]);
    }

    #[test]
    fn sorting_by_a_column_beyond_the_row_width_leaves_the_rows_alone() {
        let mut rows = vec![vec!["a".into()], vec!["b".into()]];
        let before = rows.clone();
        sort_rows(&mut rows, &SortState { column: 9, descending: false });
        assert_eq!(rows, before, "a ragged CRD table must not panic or scramble");
    }

    #[test]
    fn ages_and_counts_sort_numerically_not_lexically() {
        // "10" before "9" is the classic wrong answer, and RESTARTS is one of
        // the columns people actually sort by.
        let mut rows = vec![vec!["9".into()], vec!["10".into()], vec!["2".into()]];
        sort_rows(&mut rows, &SortState { column: 0, descending: false });
        assert_eq!(rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(), vec!["2", "9", "10"]);
    }
```

- [ ] **Step 2: Run, observe failure, commit the tests alone**

- [ ] **Step 3: Implement**

`sort_rows` compares numerically when both values parse as `f64`, otherwise lexically — with a **stable** sort so equal keys keep order.

`column_offsets` reproduces `Table`'s two-stage layout: reserve the selection column, then `Layout::horizontal(widths).spacing(column_spacing)`. Consult the Plan 2 reference D14 for the verified form.

Prefer `ColumnSource::Server` when a `TableData` is available for the active kind, falling back to the builtin registry. Fetching is a one-shot per kind change, not per frame — it is a request, and the render path performs no I/O.

- [ ] **Step 4: Mutation-checks**

1. `column_offsets` ignores `selection_width`. Confirm the offsets test FAILS.
2. `sort_rows` uses an unstable sort. Confirm the stability test FAILS.
3. `sort_rows` always compares lexically. Confirm the numeric test FAILS.

- [ ] **Step 5: Commit** — `feat: server-side columns and click-to-sort`

---

### Task 7: Detail pane — tabs and Overview

**Files:**
- Create: `src/ui/views/detail.rs`
- Modify: `src/ui/views/mod.rs`, `src/ui/hit.rs`

**Interfaces:**
- Produces:
  - `enum DetailTab { Overview, Yaml, Events }`
  - `struct DetailPane { pub tab: DetailTab, pub yaml_scroll: u16, pub events_scroll: u16 }`
  - `fn render_detail(f, area, obj: &DynamicObject, pane: &mut DetailPane, events: &[EventRow], hits: &mut HitRegistry)`
  - `fn overview_rows(obj: &DynamicObject) -> Vec<(String, String)>`
  - `HitTarget` gains `DetailTab(usize)`, `DetailClose`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn overview_shows_the_fields_you_open_a_pod_to_check() {
        let obj = pod_with_status();
        let rows = overview_rows(&obj);
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        for expected in ["Name", "Namespace", "Node", "Status", "Age"] {
            assert!(keys.contains(&expected), "overview missing {expected}: {keys:?}");
        }
    }

    #[test]
    fn overview_of_an_object_with_no_status_still_renders_its_metadata() {
        // ConfigMaps and Secrets have no status block at all.
        let rows = overview_rows(&bare_object("my-config"));
        assert!(rows.iter().any(|(k, v)| k == "Name" && v == "my-config"));
    }

    #[test]
    fn each_tab_is_clickable_and_maps_to_its_own_index() {
        let (_, hits) = render_to_string(DetailTab::Overview, 60, 20);
        let mut found = Vec::new();
        for y in 0..20u16 {
            for x in 0..60u16 {
                if let Some(HitTarget::DetailTab(i)) = hits.hit(x, y) {
                    if !found.contains(i) { found.push(*i); }
                }
            }
        }
        assert_eq!(found, vec![0, 1, 2], "all three tabs must be clickable");
    }

    #[test]
    fn the_active_tab_is_visually_distinct() {
        let (a, _) = render_styles(DetailTab::Overview);
        let (b, _) = render_styles(DetailTab::Yaml);
        assert_ne!(a, b, "switching tabs changed nothing on screen");
    }

    #[test]
    fn the_pane_has_a_close_affordance() {
        let (_, hits) = render_to_string(DetailTab::Overview, 60, 20);
        let mut found = false;
        for y in 0..20u16 {
            for x in 0..60u16 {
                if matches!(hits.hit(x, y), Some(HitTarget::DetailClose)) { found = true; }
            }
        }
        assert!(found, "no way to close the pane by mouse");
    }
```

- [ ] **Step 2-5: Run, commit tests, implement, mutation-check, commit**

Mutations: (1) tabs registered with a constant index — confirm the tab-index test fails; (2) active tab styled identically — confirm the distinctness test fails.

Use `geometry::tab_spans` from Task 5 for tab hit zones. Do not compute them locally.

---

### Task 8: Detail pane — YAML

The probe confirmed `serde_norway` output needs no post-processing.

**Files:**
- Modify: `src/ui/views/detail.rs`, `Cargo.toml` (add `serde_norway = "0.9"`)

**Interfaces:**
- Produces:
  - `fn object_to_yaml(obj: &DynamicObject) -> String`
  - `fn yaml_line_count(yaml: &str) -> u16`
  - `fn clamp_scroll(scroll: u16, total_lines: u16, viewport: u16) -> u16`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn yaml_leads_with_apiversion_kind_and_metadata() {
        // kubectl's own convention. A serialiser that alphabetised the top
        // level would put `apiVersion` after nothing but still bury `kind`.
        let y = object_to_yaml(&pod_with_status());
        let lines: Vec<&str> = y.lines().collect();
        assert!(lines[0].starts_with("apiVersion:"), "first line was {:?}", lines[0]);
        assert!(lines.iter().any(|l| l.starts_with("kind:")));
        assert!(lines.iter().any(|l| l.starts_with("metadata:")));
    }

    #[test]
    fn yaml_renders_multiline_annotations_readably() {
        let obj = object_with_annotation("desc", "line one\nline two\nline three");
        let y = object_to_yaml(&obj);
        assert!(y.contains("line one"));
        assert!(y.contains("line three"));
        assert!(!y.contains("\\n"), "multi-line value was escaped rather than blocked:\n{y}");
    }

    #[test]
    fn scroll_clamps_to_the_document_and_never_underflows() {
        assert_eq!(clamp_scroll(0, 100, 20), 0);
        assert_eq!(clamp_scroll(50, 100, 20), 50);
        assert_eq!(clamp_scroll(200, 100, 20), 80, "cannot scroll past the last screenful");
        assert_eq!(clamp_scroll(10, 5, 20), 0, "a document shorter than the viewport does not scroll");
    }
```

- [ ] **Step 2-5:** Run, commit tests, implement, mutation-check (`clamp_scroll` returns its input unchanged — confirm the clamp test fails), commit.

Consult API reference D12 for `Paragraph::scroll((y, x))` semantics — they were verified by rendering, not by reading docs.

---

### Task 9: Detail pane — Events

**Files:**
- Create: `src/store/events.rs`
- Modify: `src/ui/views/detail.rs`, `src/store/mod.rs`

**Interfaces:**
- Produces:
  - `struct EventRow { pub kind: String, pub reason: String, pub message: String, pub age: String, pub count: i32 }`
  - `fn field_selector_for(name: &str, namespace: Option<&str>) -> String`
  - `fn event_rows(events: &[Event], now: DateTime<Utc>) -> Vec<EventRow>`
  - `async fn fetch_events(client: &Client, ns: &str, name: &str) -> anyhow::Result<Vec<EventRow>>`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn the_field_selector_scopes_to_one_object() {
        let s = field_selector_for("api-x2k", Some("payments"));
        assert!(s.contains("involvedObject.name=api-x2k"));
        assert!(s.contains("involvedObject.namespace=payments"));
    }

    #[test]
    fn a_cluster_scoped_object_omits_the_namespace_term() {
        let s = field_selector_for("node-1", None);
        assert!(s.contains("involvedObject.name=node-1"));
        assert!(!s.contains("namespace"), "cluster-scoped objects have no namespace to match");
    }

    #[test]
    fn warnings_are_distinguishable_from_normal_events() {
        let rows = event_rows(&[event("Normal", "Scheduled"), event("Warning", "BackOff")], now());
        assert_eq!(rows[0].kind, "Normal");
        assert_eq!(rows[1].kind, "Warning");
    }

    #[test]
    fn an_event_with_no_timestamps_still_renders() {
        // event_time, first_timestamp and last_timestamp are all Option.
        let rows = event_rows(&[event_without_timestamps()], now());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].age, "?");
    }

    #[test]
    fn age_prefers_the_most_recent_timestamp_available() {
        // last_timestamp is what kubectl shows; falling back to first would
        // make a repeating event look stale.
        let rows = event_rows(&[event_with_times(hours_ago(5), hours_ago(1))], now());
        assert_eq!(rows[0].age, "1h");
    }
```

Check `Event`'s real field names and Option-ness in API reference C8 before writing the helpers.

- [ ] **Step 2-5:** Run, commit tests, implement, mutation-check (age falls back to `first_timestamp` — confirm the age test fails), commit.

---

### Task 10: Wiring and integration

**Files:**
- Modify: `src/main.rs`, `src/app/session.rs`, `tests/integration_kind.rs`, `README.md`

- [ ] **Step 1: Wire the sidebar and detail pane**

- `Session` gains `kinds: Vec<KindInfo>` and `active_kind: GroupVersionKind`, both written under the same lock as `client`/`namespace`. **Do not add a second copy in the event loop** — Plans 1 and 2 produced four Critical/Important findings from exactly that.
- Layout: ribbon | sidebar | table, with the detail pane overlaying the table when open.
- Draw order: ribbon, sidebar, table, detail pane, then any picker overlay last.
- Selecting a kind in the sidebar changes `active_kind` and re-renders from the store — no refetch, the watch is already running.
- `Enter` or double-click on a table row opens the detail pane; `Esc` closes it. Both must be reachable by mouse and keyboard.

- [ ] **Step 2: Add integration tests** (`#[ignore]`d, serialised via the existing `cluster_lock()`)

1. `discovery_finds_the_core_workload_kinds` — assert Pod, Deployment and Service are present and browsable.
2. `a_forbidden_kind_is_marked_unavailable_not_retried_forever` — needs an RBAC-restricted context; if that cannot be arranged in `dev-cluster.sh`, say so and cover it by unit test only.
3. `events_for_a_real_pod_are_returned` — assert at least one event for a freshly-created pod.

- [ ] **Step 3: Extend the manual checklist in README.md**

Add, with exact commands and expected results:
- The sidebar lists groups; expanding shows kinds with live counts; counts change as the cluster changes.
- A kind you lack RBAC on shows a reason, not a perpetual zero.
- Selecting a kind switches the table without a visible refetch.
- A CRD renders with its own columns, matching `kubectl get <crd>`.
- Clicking a column header sorts by it; clicking again reverses.
- Enter opens the detail pane; the three tabs are clickable; YAML scrolls and matches `kubectl get -o yaml`.
- Events appear for a pod with recent activity.

- [ ] **Step 4: Verify and commit**

`cargo test`, `cargo test --test integration_kind` (expect all ignored), `cargo build --tests`, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`.

---

## Definition of Done

- [ ] `cargo test` passes with no cluster present.
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean.
- [ ] Every discovered, browsable kind appears in the sidebar with a live count.
- [ ] A kind the user lacks RBAC on is marked unavailable with a reason and stops retrying.
- [ ] Any kind, including a CRD, renders with kubectl-equivalent columns.
- [ ] Column headers sort, numerically where the values are numeric.
- [ ] The detail pane shows Overview, YAML and Events, all reachable by mouse and keyboard.
- [ ] No second copy of `active_kind`, `namespace` or `client` exists outside `Session`.
- [ ] Render cost stays O(viewport); idle CPU stays 0%.
- [ ] Nothing in the binary can mutate cluster state.

## Carried to Plan 4

Log streaming — live tail, multi-pod and multi-container aggregation, filtering, time-range selection, and the four export modes. `Api::log_stream` was verified in the Plan 2 reference (E15) and returns a **futures-io** `AsyncBufRead`, not a tokio one.

Also carried: extracting an `AppState` struct from `main.rs` (recommended by Plan 2's whole-branch review — `main.rs` does wiring *and* per-cluster view state, and the extraction would make the latter structurally impossible to leave stale); YAML syntax highlighting, which the probe confirms needs a new dependency; and making `decode_table` reject a `rows` value that is present but not an array, once `fetch_table` is actually wired in.
