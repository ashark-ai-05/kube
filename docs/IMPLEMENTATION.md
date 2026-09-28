# Kubernetes TUI delivery

Changes land on master only after their acceptance checks pass.

1. Restore terminal UI and existing terminal/input/render tests; build and check CLI.
2. On-demand, correctly scoped watches, bounded notifications, retry backoff, stable resource selection; stress tests and benchmarks.
3. Workload relationships, diagnostic details, UID-scoped events; synthetic relationship tests and real cluster integration.
4. Streaming workload/container logs, previous logs, bounded buffers, search, follow/pause, export; stream lifecycle and buffer tests.
5. Command palette, filter, resize, help, preferences, metrics and connected resource exploration; terminal render/input acceptance.
6. Exec, port-forward and explicitly confirmed mutations; isolated cluster checks.
7. CI, performance budgets, documentation and release artifacts.

Targets (not claims until measured): cached navigation p95 <16 ms; input under update load p95 <50 ms; local pre-authenticated first populated view <2 s; no idle redraw; byte and line bounded log retention.

Validation must distinguish synthetic performance, terminal smoke tests and live API tests. No production context is used for integration tests.

## Milestone 1 — restored TUI

Passed: 511 unit/render/input tests; all 8 Kubernetes integration tests on an isolated kind v1.37 cluster; Clippy with warnings denied; debug build and CLI help; real PTY quit and SIGTERM cleanup. Corrected Table negotiation (`v=v1`) found by live tests, added watch retry backoff, and limited event batches to 256 so continuous updates cannot prevent input processing.

## Milestone 2 — demand-driven watches

Passed: 514 unit tests; 9 live kind integration tests, including opening Nodes while scoped to demo; Clippy; terminal quit/SIGTERM smoke tests. Initial Pods load concurrently with discovery. Any discovered kind can be opened. Eight recently visited watches stay warm; older subscriptions cancel and evict their caches. Watch change notifications coalesce before queuing (100,000 changes produce one pending notification per kind). Cluster-scoped watches and table requests now ignore namespace selection.

## Milestones 3–4 — investigation and streaming logs

Passed: 522 unit/render/input tests and all 10 live kind tests, including Deployment→ReplicaSet/Pod relationships and an actual streamed container log line. Added a six-view inspector, UID-scoped events, owner/selector navigation, readiness and restart diagnostics, metrics-server reads, cancellable log sessions, 16-stream concurrency limit, bounded message queue, 100,000-line/32 MiB payload buffer, 64 KiB line cap, search, follow/pause, previous-container logs, time filters, export and clipboard. YAML display/export redacts Secret data and last-applied annotations. Type metadata is restored on dynamic list results, a defect caught by live relationship tests.

## Milestones 5–6 — controls and operations

Passed: 528 unit/render/input tests; all 11 live kind integration tests; formatting and Clippy with warnings denied; PTY quit/SIGTERM restoration; rendered-screen workflow through filtering, inspection, live logs, command help, interactive exec, a real HTTP request through port-forward, forward shutdown and clean quit. First populated debug view measured 789 ms on the local pre-authenticated kind cluster.

Added searchable commands, fuzzy name and label filters, resource-identity selection, keyboard/mouse help, collapsible and draggable panes, persistent layout/mouse preferences, exec and localhost port-forward. Write mode starts disabled and resets with context; scale/restart/delete require the exact resource name, display the target context, and enforce UID/resource-version preconditions. Live tests cover scale/restart/delete and reject deletion when a name has been reused by another UID. Container-creation log errors retry, container restarts end the old stream, paused log offsets survive buffer eviction, and delayed forward output cannot overwrite stopped status.

Synthetic release measurements on the development Apple Silicon host: 10,000-resource cached navigation p95 **0.254 ms**, 100,000-line log search p95 **4.479 ms**, 100,000 log records ingested in **26.5 ms**, and 100,000 watch updates in **80.0 ms** with **one** pending notification. These measure local rendering/data structures, not network/authentication or end-to-end input latency. Log payload retention is bounded; total process memory also includes object caches and runtime overhead.

## Milestone 7 — automated acceptance and delivery

Added Linux/macOS formatting, Clippy, unit tests, PTY cleanup and benchmark checks; a Linux kind job runs the 11 live API tests and rendered terminal workflow with a temporary high-volume log Pod. A separate manual/tag workflow builds Linux x86-64 and macOS ARM64 archives with SHA-256 checksums. Both workflow files pass actionlint. Remote run results are tracked separately from these local checks.

Release PTY acceptance measured **508 ms** to the first populated view and **8.8 ms p95** from a keypress to rendered follow/pause state with a fixture producing approximately 10,000 lines/second (50 samples, local kind, pre-authenticated context). The load fixture is deleted afterward. Cached rendering and mixed resource/log batch budgets are enforced by the synthetic benchmark; the live load test enforces a 50 ms input budget. Authentication, remote network latency and terminal performance can change user-observed results.

Rendered-screen review also keeps the context/namespace footer visible with the inspector open and uses compact name/status columns in narrow panes. Regression coverage now includes 529 unit/render/input tests.

## Final cache race check

Passed 530 unit/render/input tests and Clippy after rejecting late Table responses for evicted kinds or superseded requests. This prevents slow API replies from repopulating caches outside the eight-kind retention policy. Table notifications also use the existing coalescing gate.

The release smoke/load check after this fix passed: first populated view **504 ms**, key-to-render **10.1 ms p95**. The preceding delivery commit (`ceae1a5`) also passed all three [remote Acceptance jobs](https://github.com/ashark-ai-05/kube/actions/runs/36365155447): Linux, macOS and the isolated Kubernetes workflow.

## UI revision — selectors, navigation and readable logs

The top bar now has separate clickable context and namespace selectors, available from the inspector through Ctrl-O/Ctrl-N too. `--kubeconfig` accepts a user's multi-cluster YAML file (repeatable for merged sources), and `--context` selects the startup context. Browsing, context switching, exec and port-forward share the same explicit configuration; its current-context is never rewritten.

Logs occupy the reading pane beside the resource sidebar, with `z` for full-width inspection. Long messages wrap by default using Unicode grapheme/display widths; `w` switches to horizontal scrolling. The visible records use a bounded layout cache and stable sequence anchors so paused logs survive incoming records and eviction. `/` and Ctrl-F find and highlight text, n/N move between matching records, and F toggles matching lines versus surrounding context. Repeated metadata is compact on screen and preserved by copy/export. J formats JSON; j/k remain navigation. Tab/Shift-Tab and mouse clicks switch focus. Pickers render above the inspector, including when maximized, with explicit high-contrast foreground/background colors.

Local acceptance passed: **536 unit/render/input tests**, **11 live Kubernetes integration tests**, formatting, Clippy with warnings denied, actionlint, PTY quit/SIGTERM restoration, and two rendered-terminal workflows. The new workflow covers an explicit kubeconfig whose path contains spaces, a deliberately different KUBECONFIG environment, two selectable contexts, namespace changes, modal selectors over logs, long-message wrapping, highlighted literal search with punctuation, n/N and matches-only filtering, 80×24 resize, and sidebar navigation back to the resource list. Existing exec and real HTTP port-forward checks now also use an explicit file overriding KUBECONFIG. Captured terminal cells/colors were rendered to PNGs and visually reviewed at 150×40 and 80×24.

Release measurements on the local pre-authenticated kind cluster: **58 ms** first populated view and **10.1 ms p95** key-to-render during approximately 10,000 log lines/second. Synthetic measurements: 100,000-line search **4.970 ms p95**, wrapped-log navigation with active search **0.016 ms p95**, and 64 KiB message reflow with highlighted search **1.301 ms p95**. These are local acceptance measurements, not guarantees for remote clusters or other terminals. CI now runs the new UX workflow as well as the existing load/operations checks.

## Focus workspace acceptance (2026-09-28)

Fleet radar now summarizes pod readiness and exposes an attention queue for the watched scope. Focus studio uses a compact navigation rail and recent investigations; Ctrl-R opens the complete API catalog. Inspector overview adds readiness and owner context. F2/F3 switch workspaces without losing cluster/namespace controls.

Validation: 539 unit/render tests, clippy with warnings denied, and the isolated kind terminal UX and functional smoke tests passed. The functional fixture's first populated resource view was 277 ms (debug build, local cluster; not a network latency guarantee). UX checks cover explicit kubeconfig selection, cluster/namespace pickers, long wrapped logs, search/context/filter, narrow-terminal resize, mouse navigation and terminal restoration. Visual review caught and fixed dashboard text bleeding through from the underlying table; startup navigation is also allowed before API discovery completes.
