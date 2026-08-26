# kube

A fast, mouse-driven Kubernetes TUI — Lens-class cluster browsing in the terminal, with
first-class multi-cluster switching and a themed, colour-coded interface.

> **Status: Plan 3 complete in code; nothing in it has been run against a cluster.**
> Plan 1 built a single-cluster pod browser; Plan 2 added multi-cluster context
> switching, a themed UI, and server-side Table decoding; Plan 3 (this one) added the
> kind tree, kubectl-equivalent columns for *every* kind including CRDs, click-to-sort,
> and the detail pane. The full v1 design remains
> [`docs/superpowers/specs/2026-08-25-kube-tui-design.md`](docs/superpowers/specs/2026-08-25-kube-tui-design.md).
>
> **Read "What is built but never verified" below before trusting any of it.** The
> binary has never been launched in a real terminal and no `#[ignore]`d integration
> test has ever been executed, on any machine, because none of them had a container
> runtime. Everything below describes what the code does; the manual checklist at the
> end is how a human finds out whether it actually does it.

## What it does today

- **Browses every kind the cluster will let it.** On connect it runs API discovery,
  keeps every kind that supports both `list` and `watch`, and starts a watch for each
  one (up to a cap of 40, most-used kinds first — Pods, Deployments, Services and so
  on; anything beyond the cap is listed as `not watched` rather than silently missing).
  The left-hand sidebar groups them by API group with a live count beside each.
- **Renders each kind with kubectl's own columns.** The table asks the API server to
  render the active kind the way `kubectl get` would
  (`Accept: application/json;as=Table;v=1;g=meta.k8s.io`), so a CRD arrives with the
  printer columns its author declared rather than a generic NAME/AGE fallback. The
  refetch is driven by the watch — a change, then a quiet moment — never by a timer, so
  an idle cluster costs nothing. Until the first fetch lands (or if it fails) the table
  falls back to a built-in column registry rather than going blank.
- **Sorts by any column.** Click a header to sort by it, click again to reverse.
  Numeric columns sort numerically (`2 < 9 < 10`, not `10 < 2 < 9`).
- **Opens a detail pane on any object.** `Enter`, or a double-click on a row, opens
  Overview / YAML / Events over the table. YAML is the object as the server has it,
  scrollable; Events is that object's own events, or an explanation if listing them is
  forbidden — never an empty list dressed up as "nothing wrong". Tabs are clickable and
  keyboard-reachable; `Esc` or the `[x]` closes.
- **Watches live, in one namespace or all of them.** A watch-driven in-memory cache
  updates the table as the cluster changes; idle CPU targets zero (no timers, no
  animation frames — the event loop only wakes on real input or a real delta).
- **Says why a kind is unavailable instead of showing a zero.** A kind you lack RBAC on
  is marked with the API server's own reason, and its watch stops rather than retrying
  a 403 for ever.
- **Switches clusters without restarting.** Press `c` to open a picker over every
  context in your kubeconfig, filter by typing, and select one. The switch connects to
  the new cluster *before* tearing down the old one — a cluster you can't reach reports
  `failed` with a reason and leaves you exactly where you were, still working, on the
  cluster you started on. The new cluster's kinds are discovered fresh; the previous
  cluster's are discarded rather than shown over it.
- **Is mouse-native.** Click the ribbon to open the cluster picker, click a kind to
  switch the table to it, click a row to select it, double-click to inspect it, scroll
  over whichever pane the pointer is on — built on a per-frame hit-test registry so
  everything drawn is clickable by construction.
- **Is themed and colour-coded.** A one-column ribbon on the left, and the cluster name
  in the status bar, are both painted in a colour derived from the cluster's name — so
  "which cluster am I looking at" is answerable peripherally, without reading text. Pod
  status (`Running`, `CrashLoopBackOff`, ...) is coloured from a separate, warm palette
  that a cluster's own hue never borrows from, so the two kinds of colour never get
  confused for each other.
- **Re-scopes by namespace.** Press `n` to open a namespace picker and re-scope every
  watch. The list is the union of what the API reports, what the current watch has
  loaded, and the namespace in effect; where listing namespaces is itself forbidden the
  picker says so and still accepts a typed name, which needs no listing permission.
- **Cannot change anything.** There is no code path in the binary that issues a write
  to a cluster. That is a v1 decision, not an oversight.

### Keys

| Key | Effect |
| --- | --- |
| `Tab` | move focus between the sidebar and the table |
| `j`/`k`, `↑`/`↓` | move the focused pane's selection |
| `Enter` | sidebar: expand a group / select a kind. Table: open the detail pane |
| `Space` | sidebar: expand or collapse a group |
| `Esc` | close the detail pane or a picker; quit if neither is open |
| `Tab`, `←`/`→` | with the detail pane open: previous/next tab |
| `c` / `n` | cluster picker / namespace picker |
| `q`, `Ctrl-C` | quit |

## What is built but never verified

Everything in this section is code that compiles, is unit-tested where a unit test can
reach it, and has **never executed against a Kubernetes API server**. No machine this
was written on had Docker or Podman, so `./scripts/dev-cluster.sh` has never run and
`cargo test -- --ignored` has never run. Treat all of it as unverified until someone
does both.

- **Discovery** (`src/cluster/discovery.rs`). `discover_kinds` needs a cluster, so no
  unit test covers it at all — only its pure helpers (`is_browsable`, `sort_kinds`) are
  tested. `discovery_returns_every_kind_the_server_says_is_listable_and_watchable` is
  the test that would catch a filter that quietly drops kinds.
- **Server-side Table rendering** (`src/store/table.rs`, `fetch_table`). The decode
  logic is unit-tested against synthetic JSON, but the `Accept` header and the
  hand-appended `includeObject=Metadata` parameter exist only on the wire. A Kubernetes
  API server **ignores an `Accept` value it doesn't recognise** and answers with
  ordinary JSON rather than erroring, so a typo or a version drift there fails
  *silently*; nothing but a real request can catch it.
- **Events** (`src/store/events.rs`). The field selector
  (`involvedObject.name=…,involvedObject.namespace=…`) has never been sent. A selector
  the server rejects and one that simply matches nothing look the same from here.
- **The Table refetch debounce** (`TABLE_REFETCH_DEBOUNCE`, 750 ms). Picked to smooth a
  rollout's burst of deltas into one refetch; never measured against a real API server.
- **The RBAC path.** `a_forbidden_kind_is_marked_unavailable_not_retried_forever` runs
  against a restricted ServiceAccount that `dev-cluster.sh` now creates. It has never
  run.
- **The binary itself.** It has never been launched in a real terminal. Every rendering
  claim above rests on `ratatui`'s `TestBackend`, which is a buffer, not a terminal.

## Deliberately deferred

Editing and apply, exec, port-forwarding, metrics, and log streaming — Plan 4 or later.
Keeping the mutation risk surface at zero was a first-milestone decision, not an
oversight. YAML syntax highlighting is deferred because it needs a new dependency.

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
missing rather than kind's own opaque error. Besides the `demo` namespace and its
3-replica `web` deployment it creates a `restricted` ServiceAccount that can list and
watch pods in `demo` and nothing else, which is what
`a_forbidden_kind_is_marked_unavailable_not_retried_forever` authenticates as. Nothing
it does touches your kubeconfig: that test reads the ServiceAccount's token through the
API and builds its client in-process.

These tests are serialised against each other with an in-process `tokio::sync::Mutex`
(`cluster_lock()` in `tests/integration_kind.rs`), since they share the `demo`
namespace and some of them mutate it (deleting a pod, creating one to generate events)
— running them with Rust's default parallel test execution would make one test's
deletion race another's assertions.

## Manual verification checklist

A colour that actually appears on a real terminal, a mouse click that actually lands, a
cluster that actually goes unreachable, an `Accept` header a real API server actually
honours, memory that actually stays flat over real time — these are exactly the things
no unit test, no `TestBackend` assertion, and no CI job in this repo can check. This
checklist is how a human confirms them, and right now it is the **only** confirmation
any of it has ever had.

It assumes a real terminal, a real kubeconfig with at least two reachable contexts (one
of which should be a local `kind` cluster with several hundred pods for the
scroll/memory steps — see below), and ideally one *unreachable* context (a stale
VPN-only entry, or a bogus one you add temporarily) for the failure step. Steps 13–19
additionally want a cluster with at least one CRD installed and one kind you lack RBAC
on; `./scripts/dev-cluster.sh` gives you the second of those.

Run everything from the repo root.

0. **Run the cluster-backed tests first.** They are faster than the manual steps and
   they fail loudly where a human might not notice.
   ```sh
   ./scripts/dev-cluster.sh
   cargo test -- --ignored
   ```
   Expect: 8 tests, all passing, in a couple of minutes. A failure here is more
   informative than anything below it — in particular
   `fetch_table_returns_kubectl_equivalent_columns` failing means the API server did
   not honour the `Accept` header and every column in the app is coming from the
   built-in fallback, which looks completely normal on screen.

1. **Build and launch.**
   ```sh
   cargo build
   cargo run
   ```
   Expect: a one-column coloured ribbon down the left edge; a bordered `Kinds` pane
   about 28 columns wide beside it; the table to the right of that; the status bar
   (bottom row) shows `<cluster> · <namespace> · <count> items · <live|loading|...>`,
   with the cluster name painted in the same hue as the ribbon. The table populates
   with pods within a second or two — note that nothing is watched until discovery
   answers, so a second of empty table at startup is expected, not a hang.

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

6. **SSO cluster with an expired token: an actionable error, never a garbled screen.**
   Pick a context that authenticates through a credential plugin (`kubelogin`,
   `gke-gcloud-auth-plugin`, `aws-iam-authenticator` — anything under an `exec:` block
   in your kubeconfig) and make sure its cached token is expired or absent first
   (`kubelogin remove-cached-token`, `gcloud auth revoke`, or just wait one out).
   Press `c` and select it. Expect: the switch fails within a second or two with an
   error line in the status bar that **names the plugin** (`… — 'kubelogin' needs to
   log in; run it in a shell first`), and the cluster you were already on keeps its
   ribbon, its data and its live watch. The screen must stay intact: **no** login URL
   or prompt printed into the table, no staircased text, no frozen UI swallowing your
   keystrokes. Then run the plugin (or any `kubectl` command against that context) in
   another terminal to refresh the credential, come back, and switch again — it should
   now succeed normally. This is what `disable_interactive_exec` in
   `src/cluster/auth.rs` exists to prevent: without it the plugin inherits our stdin
   and stderr and does exactly the damage described above.

7. **Picker scrolling with more contexts than fit on screen.**
   You need a kubeconfig with more contexts than your terminal has rows for the picker
   (roughly 20 in an 80×24 window; `KUBECONFIG` can point at a throwaway file with
   twenty dummy contexts). Press `c`, then hold Down past the bottom of the list.
   Expect: the list scrolls, the highlighted row stays visible at all times, and the
   last context is reachable and highlighted. Then **click** it — the cluster you
   clicked must be the one it tries to connect to. Every context must be reachable by
   mouse alone.

8. **Client-race check: cluster switch immediately followed by a namespace pick.**
   Press `c`, select a different reachable cluster, and — without waiting for
   `connecting…` to resolve — as soon as (or just after) the table repopulates, press
   `n` and pick a namespace. Expect: the resulting table contains only objects from
   the *new* cluster, scoped to the namespace you just picked — never a mix of the
   old cluster's objects with the new cluster's namespace, and never the old cluster's
   client used for the new watch. (This is the race `restart_watch` reading `client`
   from the same lock guard as the teardown, in `src/app/session.rs`, exists to close.)

9. **Namespace picker.**
   Press `n`. Expect: a picker listing "all namespaces" plus the union of the
   namespaces the API reports, those seen among loaded objects, and the one currently
   in effect. Select a different namespace: the table re-scopes to it, and — because
   every kind's watch is restarted — the sidebar's counts all re-populate too. On a
   cluster where listing namespaces is forbidden, expect the "all namespaces" entry to
   carry an explanation and typing a name plus `Enter` to still work.

10. **Scroll and click accuracy on a large table.**
   Point `kind`, or any cluster, at a namespace with several hundred pods (a
   `kubectl scale` on `dev-cluster.sh`'s `web` deployment, or your own test fixture,
   works). Scroll through the table with the mouse wheel — expect smooth, responsive
   scrolling with no visible lag or dropped frames, and CPU staying near zero between
   scroll events (check with `top`/`htop` while idling on the table — no timers or
   animation frames means idle CPU should read ~0%). Click a row that's visible partway
   down the screen — expect **that exact row** to become selected (highlighted), not
   the one above or below it. Repeat after scrolling to a different offset to confirm
   it isn't a coincidence of the first screen position.

11. **Quit leaves the shell intact.**
   Press `q`. Expect: the alternate screen closes, the terminal returns to its normal
   contents, and the shell prompt is back with no leftover raw-mode weirdness — type a
   few characters and confirm they echo normally, press Enter and confirm the prompt
   redraws normally.

12. **Panic still restores the terminal (optional, don't commit the trigger).**
    To confirm the panic hook actually works rather than trusting that it does:
    temporarily add `panic!("manual test")` somewhere it's guaranteed to run early in
    `run_with_scope` (`src/main.rs`, after `install_panic_hook()`), `cargo run`, and
    confirm the terminal is restored (cursor visible, raw mode off, alternate screen
    closed, panic message printed to the now-restored normal screen) rather than left
    in a corrupted, unusable state. **Revert the `panic!` before committing anything**
    — it must never land in the tree.

13. **The sidebar lists groups, and expanding one shows kinds with live counts.**
    Look at the `Kinds` pane. Expect: one row per API group — `core`, `apps`, `batch`,
    `networking.k8s.io`, `rbac.authorization.k8s.io`, and one per CRD group your
    cluster has — in alphabetical order, each with a `▸` marker. The group holding the
    kind the table is showing (`core`, at startup) is already expanded (`▾`) with its
    kinds indented beneath it and a number beside each. Cross-check two of them:
    ```sh
    kubectl get pods -A --no-headers | wc -l
    kubectl get configmaps -A --no-headers | wc -l
    ```
    Expect: the numbers match what the sidebar shows for `Pod` and `ConfigMap` (run
    without `-A` if you're scoped to one namespace). Click a collapsed group's row —
    it expands; click again — it collapses. Press `Tab` to move focus to the sidebar,
    then `j`/`k` to move the highlight and `Space` to expand — the same thing must be
    reachable without the mouse.

14. **Counts change as the cluster changes.**
    With the sidebar showing a `Pod` count, from another terminal:
    ```sh
    kubectl -n demo scale deployment/web --replicas=8
    ```
    Expect: the `Pod` count climbs to match within a few seconds, with no keypress and
    no visible refresh — the whole screen should not flicker. Scale back to 3 and watch
    it fall. If the count never moves, the watch for that kind is not running; if it
    moves only when you press a key, the redraw is not being armed by store deltas.

15. **A kind you lack RBAC on shows a reason, not a perpetual zero.**
    You need a kind you cannot list. The simplest is to run against the restricted
    identity `./scripts/dev-cluster.sh` creates:
    ```sh
    kubectl -n demo get secret restricted-token -o jsonpath='{.data.token}' | base64 -d
    ```
    Add a temporary context to a throwaway kubeconfig using that token, point
    `KUBECONFIG` at it, and `cargo run -- -n demo`. Expect: `Pod` shows a real count,
    while `Secret`, `ConfigMap` and the rest each show a short reason (`secrets is
    forbidden…`) in muted text where a count would be — **never** a blank or a `0`,
    which reads as "this kind is empty". The status bar shows the same reason once.
    Leave it running for a minute: the reason must stay put and the status bar must not
    cycle through repeated errors, which would mean the watch is retrying a 403 for
    ever. On a corporate cluster this is the normal case, not the exception.

16. **Selecting a kind switches the table with no visible refetch.**
    Expand `core`, click `ConfigMap`. Expect: the table's title changes to `ConfigMap`,
    the rows change to config maps, and the columns change to config maps' columns —
    immediately, from data already in memory, because that kind's watch has been
    running since startup. There must be no empty-table flash and no perceptible pause.
    Click back to `Pod` and back again a few times: still instant each way. (The
    *columns* may take a beat the very first time you visit a kind — that is the
    one-off Table fetch, and until it lands you'll see a plain NAME/AGE table.)
    Then do the same by keyboard: `Tab` to the sidebar, `j`/`k` to a kind, `Enter`.

17. **A CRD renders with its own columns.**
    Pick any CRD your cluster has (`kubectl get crds`). Expand its group in the sidebar
    and select the kind. Then compare:
    ```sh
    kubectl get <crd-plural> -A
    ```
    Expect: the same column headers, in the same order, with the same values —
    including whatever custom printer columns the CRD's author declared. If you instead
    see only `NAME` and `AGE`, the Table request is not being honoured: either the
    `Accept` header drifted or the fetch failed (check the status bar for a
    `fetching … columns:` error). That failure mode is silent by design on the server's
    side, which is why this step exists.

18. **Clicking a column header sorts by it; clicking again reverses.**
    On a kind with at least a dozen rows, click the `Name` header. Expect: rows reorder
    alphabetically and the row you had selected stays selected *by highlight position*
    — check that the highlighted row is still a row, not off the end. Click `Name`
    again: the order reverses. Now click a numeric header (`Restarts` on Pods, or
    `Replicas` on Deployments) and confirm it sorts **numerically** — a cluster with
    pods at 2, 9 and 10 restarts must order them `2, 9, 10`, not `10, 2, 9`. If you see
    the latter, the numeric path is not being taken.

19. **`Enter` opens the detail pane; all three tabs work by mouse and keyboard.**
    Select a pod and press `Enter` (then, separately, double-click a row — both must
    open it). Expect: a rounded, bordered pane covering the table area only — the
    sidebar and ribbon stay visible — titled with the object's name, with
    `Overview  YAML  Events` across the top and an `[x]` at the right.
    - **Overview** lists Name, Namespace, Node, Status and Age. Compare against
      `kubectl -n <ns> get pod <name> -o wide`.
    - **YAML**: click the tab. Compare against
      ```sh
      kubectl -n <ns> get pod <name> -o yaml
      ```
      Expect the same document, leading with `apiVersion`, `kind`, `metadata`. Scroll
      with `j`/`k` and with the wheel: it must reach the very last line of the document
      and stop there, not scroll into blank space and not stop short. Long
      single-line values (a base64 secret, a long annotation) wrap; everything after
      them must still be reachable.
    - **Events**: click the tab. See step 20.
    - Cycle the tabs with `Tab` and `←`/`→` as well as by clicking. Press `Esc` — the
      pane closes and the table is intact underneath. Reopen and close it with the
      `[x]` instead.
    - While the pane is open, click a different kind in the sidebar: the pane closes
      (it was showing an object of the old kind) and the table switches. Press `c`: the
      cluster picker still opens over the top of it.
    - Finally, with the pane open on a pod, delete that pod from another terminal:
      ```sh
      kubectl -n demo delete pod <name>
      ```
      Expect the pane to close when the object leaves the store, rather than continuing
      to show a deleted object's YAML as though it were live.

20. **Events appear for a pod with recent activity.**
    Create something that will definitely generate events, and inspect it before they
    expire (Kubernetes drops events after an hour by default, so an old pod legitimately
    has none):
    ```sh
    kubectl -n demo run evtest --image=nginx:alpine
    ```
    Open `evtest` in the detail pane, click **Events**, and compare against:
    ```sh
    kubectl -n demo describe pod evtest      # the Events: table at the bottom
    ```
    Expect: the same events, most-relevant fields first (age, type, reason, message),
    warnings coloured differently from normal ones, repeated events showing `(xN)`.
    Then the negative case, which matters more than the positive one:
    ```sh
    kubectl -n demo run badimage --image=nginx:this-tag-does-not-exist
    ```
    Open it and expect `Failed` / `ErrImagePull` / `ImagePullBackOff` warnings — this is
    the whole reason the tab exists. Finally, on the restricted identity from step 15,
    open a pod's Events tab and expect **`Events unavailable: …`** with a reason, not an
    empty pane: "no events" and "you may not read events" must never look the same.
    Clean up with `kubectl -n demo delete pod evtest badimage`.

21. **Idle CPU is still zero with everything watching.**
    Leave the app on a quiet cluster with all its kinds watched, no keys pressed, for a
    minute, and watch it in `top`/`htop`. Expect: ~0%. The Table refetch is triggered by
    watch activity plus a 750 ms settle, not by a timer, so a cluster where nothing is
    happening must produce no work at all. A steady low-single-digit percentage means
    something is waking the loop on a schedule — most likely the refetch debounce
    re-arming itself.

### What this checklist cannot cover

It's still a human doing a fixed number of runs, not a fuzzer: it won't catch a leak
that only shows up after hundreds of switches, a race that only reproduces under a
specific network timing, or a rendering glitch specific to a terminal emulator nobody
tested it in. Treat a clean run as "no regression found today," not as a proof.

Some things it deliberately cannot reach at all:

- **A cluster with hundreds of CRDs**, where the 40-kind watch cap engages and the
  sidebar starts showing `not watched`. No local `kind` cluster has enough kinds to
  trigger it; the cap's behaviour is only covered by unit test.
- **A cluster large enough to matter for the refetch load.** The 750 ms debounce is a
  guess. Confirming it is the right guess means watching API server request rates
  during a real rollout on a real cluster, which is an operations exercise, not a
  checklist step.
- **Terminal emulators other than the one you use.** Mouse reporting, wide-character
  widths and colour rendering all vary.
- **Anything about correctness under concurrent cluster switching** beyond step 8's
  single interleaving.

## License

MIT
