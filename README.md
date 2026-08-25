# kube

A fast, mouse-driven Kubernetes TUI — Lens-class cluster browsing in the terminal, with
first-class multi-cluster switching and a themed, colour-coded interface.

> **Status: Plan 2 complete, Plan 3 not started.** Plan 1 built a single-cluster pod
> browser; Plan 2 (this one) added multi-cluster context switching, a themed UI, and
> server-side Table decoding. The full v1 design remains
> [`docs/superpowers/specs/2026-08-25-kube-tui-design.md`](docs/superpowers/specs/2026-08-25-kube-tui-design.md);
> what's actually built so far is described honestly below, including what still only
> exists as infrastructure.

## What it does today

- **Watches pods, live, in one namespace or all of them.** A watch-driven in-memory
  cache updates the table as the cluster changes; idle CPU targets zero (no timers, no
  animation frames — the event loop only wakes on real input or a real delta).
- **Switches clusters without restarting.** Press `c` to open a picker over every
  context in your kubeconfig, filter by typing, and select one. The switch connects to
  the new cluster *before* tearing down the old one — a cluster you can't reach reports
  `failed` with a reason and leaves you exactly where you were, still working, on the
  cluster you started on.
- **Is mouse-native.** Click the ribbon to open the cluster picker, click a row to
  select it, scroll to move through hundreds of pods, click again to close a picker —
  built on a per-frame hit-test registry so everything drawn is clickable by
  construction.
- **Is themed and colour-coded.** A one-column ribbon on the left, and the cluster name
  in the status bar, are both painted in a colour derived from the cluster's name — so
  "which cluster am I looking at" is answerable peripherally, without reading text. Pod
  status (`Running`, `CrashLoopBackOff`, ...) is coloured from a separate, warm palette
  that a cluster's own hue never borrows from, so the two kinds of colour never get
  confused for each other.
- **Re-scopes by namespace.** Press `n` to open a namespace picker and re-scope the
  watch. **Honestly:** this list is built from the namespaces of objects *already
  loaded* by the current watch, not a cluster-wide namespace listing — accurate and
  complete when you're already watching all namespaces, partial when you're scoped to
  one.

## What's built but not yet wired in

- **Server-side Table rendering** (`src/store/table.rs`, `fetch_table`/`decode_table`)
  — the raw-HTTP path that asks the API server to render a resource kubectl-style
  (`Accept: application/json;as=Table;v=1;g=meta.k8s.io`, since `kube` 4.2 has no
  built-in support for it). The decode logic (`decode_table`) is unit-tested against
  synthetic JSON. `fetch_table` itself — the part that actually sends the request and
  the Accept header — has **never been run against a cluster**: an integration test
  for it exists (`tests/integration_kind.rs`) but is `#[ignore]`d and has not been
  executed, because no container runtime was available on the machine it was written
  on. Until someone runs `./scripts/dev-cluster.sh && cargo test -- --ignored`, treat
  the header as unverified — a Kubernetes API server ignores an `Accept` value it
  doesn't recognise and falls back to ordinary JSON rather than erroring, so a typo or
  a version drift in that header fails **silently**, and no unit test can catch it: the
  header only exists on the wire, never in `decode_table`'s input. The live table view
  also still renders pods through its own hardcoded column extraction
  (`src/store/columns.rs`), not through this path at all.
- **Only pods.** The watch, the table, and the column logic are all general enough to
  handle other kinds, but `main.rs` currently hardcodes a single `Pod` watch. The
  sidebar kind tree that would make other kinds reachable is Plan 3.

## Deliberately deferred

Editing and apply, exec, port-forwarding, metrics, log streaming, and detail views
(overview / YAML / events tabs) — all Plan 3 or later. Keeping the mutation risk surface
at zero was a first-milestone decision, not an oversight.

## Stack

Rust, [`ratatui`](https://ratatui.rs) for rendering, [`kube-rs`](https://kube.rs) for
the Kubernetes client, `tokio` for async I/O.

## Development

```sh
cargo build
cargo test          # unit + wired-in tests — no cluster required
```

Most of the system is testable without a cluster: the store is driven by synthetic
watch deltas, query and hit-testing are pure functions, rendering is asserted against
`ratatui`'s `TestBackend`, and `switch_cluster`/`restart_watch` are tested against a
`Client` built with no I/O at all (`Client::try_from(kube::Config::new(uri))` performs
no network call).

A second layer of tests needs a real cluster and is `#[ignore]`d by default so `cargo
test` never touches a network:

```sh
./scripts/dev-cluster.sh     # idempotent: creates a kind cluster + demo namespace
cargo test -- --ignored      # runs tests/integration_kind.rs for real
```

`dev-cluster.sh` needs Docker or Podman on `PATH` (kind runs nodes as containers) plus
`kubectl` and `kind` themselves; it fails with an actionable message if any are
missing rather than kind's own opaque error. These tests are serialised against each
other with an in-process `tokio::sync::Mutex` (`cluster_lock()` in
`tests/integration_kind.rs`), since they share the `demo` namespace and some of them
mutate it (deleting a pod) — running them with Rust's default parallel test execution
would make one test's deletion race another's assertions.

## Manual verification checklist

The four sections above — a colour that actually appears on a real terminal, a mouse
click that actually lands, a cluster that actually goes unreachable, memory that
actually stays flat over real time — are exactly the things no unit test, no
`TestBackend` assertion, and no CI job in this repo can check. This checklist is how a
human confirms them. It assumes a real terminal, a real kubeconfig with at least two
reachable contexts (one of which should be a local `kind` cluster with several hundred
pods for the scroll/memory steps — see below), and ideally one *unreachable* context
(a stale VPN-only entry, or a bogus one you add temporarily) for the failure step.

Run everything from the repo root.

1. **Build and launch.**
   ```sh
   cargo build
   cargo run
   ```
   Expect: a one-column coloured ribbon down the left edge; the status bar (bottom
   row) shows `<cluster> · <namespace> · <count> items · <live|loading|...>`, with the
   cluster name painted in the same hue as the ribbon; the table above it is populated
   with pods within a second or two of startup.

2. **Open and close the cluster picker.**
   Press `c` (or click the ribbon). Expect: a bordered picker overlay appears centered
   over the table, titled "Clusters", listing every context from your kubeconfig, each
   in its own hue. Type a few characters of a context's name — the list narrows to
   matches as you type. Press `Esc` — the picker closes and the table underneath is
   visible again, unchanged.

3. **Switch to a different (reachable) cluster.**
   Press `c`, click or arrow-and-Enter to a different context. Expect, in order: the
   status bar briefly shows `connecting to <name>…` in amber while the old cluster's
   data is still on screen; then the ribbon changes to the new cluster's hue, the
   status bar's cluster name and colour both update, and the table repopulates with
   the new cluster's pods. The whole sequence should complete in at most a few
   seconds against a healthy cluster.

4. **Watch-leak check: switch back and forth ten times.**
   Alternate between two reachable clusters ten times (`c` → select → wait for
   `live` → `c` → select the other → wait for `live`, ×5 each way). Then, from
   another terminal:
   ```sh
   ps -o rss= -p $(pgrep -f target/debug/kube)
   ```
   Run that once before you start switching and once after. Expect: the two numbers
   are close (some growth from allocator fragmentation is normal; a number that keeps
   climbing roughly linearly with each switch means a watch — or its cache — is
   leaking). This is what `WatchHandles::abort_all` and the store-replacement-not-reuse
   design in `src/app/session.rs` exist to prevent; `switching_clusters_repeatedly_does_not_accumulate_watches`
   covers the same property at the unit level, but only this step confirms it holds
   for a real OS process over real time.

5. **Unreachable cluster: fails visibly, doesn't disturb what's working.**
   Pick a context you know is unreachable (VPN off, bogus server, etc.). Press `c`,
   select it. Expect: the status bar shows `connecting to <name>…`, then — after
   whatever timeout the failure takes — an error line reading
   `connecting to <name>: <reason>`, and the picker entry for that cluster (reopen
   `c` to check) shows `failed: <reason>`. Meanwhile the ribbon, status bar, and table
   must **still show the cluster you were on before**, with data still present and
   still updating live. Nothing about the working cluster should have paused or
   flickered during the failed attempt.

6. **Client-race check: cluster switch immediately followed by a namespace pick.**
   Press `c`, select a different reachable cluster, and — without waiting for
   `connecting…` to resolve — as soon as (or just after) the table repopulates, press
   `n` and pick a namespace. Expect: the resulting table contains only objects from
   the *new* cluster, scoped to the namespace you just picked — never a mix of the
   old cluster's objects with the new cluster's namespace, and never the old cluster's
   client used for the new watch. (This is the race `restart_watch` reading `client`
   from the same lock guard as the teardown, in `src/app/session.rs`, exists to close.)

7. **Namespace picker.**
   Press `n`. Expect: a picker listing "all namespaces" plus every namespace *actually
   present among the pods currently loaded* — not a full cluster-wide namespace list.
   If you're already watching all namespaces this is complete; if you're scoped to one
   namespace, it only shows that one (plus "all namespaces"). That's the honest
   behaviour, not a bug — there's no separate `Namespace` watch backing this yet.
   Select a different namespace: the table re-scopes to it.

8. **Scroll and click accuracy on a large table.**
   Point `kind`, or any cluster, at a namespace with several hundred pods (a
   `kubectl scale` on `dev-cluster.sh`'s `web` deployment, or your own test fixture,
   works). Scroll through the table with the mouse wheel — expect smooth, responsive
   scrolling with no visible lag or dropped frames, and CPU staying near zero between
   scroll events (check with `top`/`htop` while idling on the table — no timers or
   animation frames means idle CPU should read ~0%). Click a row that's visible partway
   down the screen — expect **that exact row** to become selected (highlighted), not
   the one above or below it. Repeat after scrolling to a different offset to confirm
   it isn't a coincidence of the first screen position.

9. **Quit leaves the shell intact.**
   Press `q`. Expect: the alternate screen closes, the terminal returns to its normal
   contents, and the shell prompt is back with no leftover raw-mode weirdness — type a
   few characters and confirm they echo normally, press Enter and confirm the prompt
   redraws normally.

10. **Panic still restores the terminal (optional, don't commit the trigger).**
    To confirm the panic hook actually works rather than trusting that it does:
    temporarily add `panic!("manual test")` somewhere it's guaranteed to run early in
    `run_with_scope` (`src/main.rs`, after `install_panic_hook()`), `cargo run`, and
    confirm the terminal is restored (cursor visible, raw mode off, alternate screen
    closed, panic message printed to the now-restored normal screen) rather than left
    in a corrupted, unusable state. **Revert the `panic!` before committing anything**
    — it must never land in the tree.

### What this checklist cannot cover

It's still a human doing a fixed number of runs, not a fuzzer: it won't catch a leak
that only shows up after hundreds of switches, a race that only reproduces under a
specific network timing, or a rendering glitch specific to a terminal emulator nobody
tested it in. Treat a clean run as "no regression found today," not as a proof.

## License

MIT
