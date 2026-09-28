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

## Ask Kube, offline bundle and log folding acceptance (2026-09-28)

The command drawer previews a closed set of read/navigation actions bound to the active scope. Standard phrases work without a model. The optional AI package includes pinned, SHA-256-verified FunctionGemma 270M Q8_0 weights and the CPU runtime, with notices; weights are excluded from Git. Raw model accuracy is insufficient for automatic execution and remains explicitly experimental (see `ai/README.md`). The normal supported-phrase evaluation passed 14/14 cases. The packaged runtime produced a validated action offline; final local test time was 1.40 s and peak worker RSS was 697.5 MiB.

Log presentation now folds same-source repeated messages and consecutive Java-style stack frames. The bounded raw ring and export contents are unchanged. Search disables presentation folding so hidden matches can be inspected. Stable sequence anchors and group eviction are covered by tests.

Local verification passed: 548 unit/render tests; 11 isolated kind integration tests; the optional offline-model integration test; terminal restoration; selector/log UX acceptance; scoped assistant-preview acceptance; exec/port-forward and sustained streaming smoke tests; format, clippy and workflow lint checks. The release binary under approximately 10,000 log lines/s had key-to-render p95 9.8 ms. Synthetic p95: 10,000-resource navigation 0.226 ms; 10,000-pod Fleet radar 3.942 ms; 100,000-line search 4.530 ms; wrapped log navigation 0.016 ms; 64 KiB reflow 1.227 ms. Hardware/network differences will change these numbers.

## Log readability, sorting, Escape and grouped metrics (2026-09-28)

JSON records now open as messages with severity badges; metadata remains available through per-record expansion, formatted JSON, original records, and search. Single-stream source labels move to the toolbar. Raw export/copy and bounded buffers are preserved. A searchable sort picker (`s`) and clickable headers support semantic age, numeric restart counts and alphabetical names. Root Escape no longer maps to Quit; repeated Escape closes nested UI and then stays at the root.

Fleet radar now groups by app labels, then controller owner, with ordered YAML overrides. Groups expose readiness, restarts, nodes and measured CPU/memory and open scoped pod lists. Namespace is part of group identity. Metrics-server requests run asynchronously only while the dashboard is visible, with a five-second interval, four-second timeout and a single pending reply. Scope changes drop the old worker and history; stale samples and samples predating a replacement pod are excluded. Missing/denied/partial measurements are explicit. Charts hold at most 60 session samples, not historical monitoring data. The 80×24 layout preserves room for groups.

Acceptance: **562 unit/render/input tests**, **11 isolated kind API tests**, Clippy, formatting, workflow lint and PTY restoration passed. Real terminal checks cover message/JSON/raw log views, hidden-metadata search, age/restart sorting, repeated Escape, custom kubeconfig selectors, scoped assistant navigation, exec, HTTP port-forward, real metrics, custom groups, group drilldown, mouse activation, compact layout and namespace reset. Fixture metrics-server uses its official pinned v0.9.0 manifest; insecure kubelet TLS is enabled only inside the disposable kind fixture.

The fuller performance fixture uses 10,000 pods with varied ages/restarts, 50 app labels and a metrics sample for every pod. It caught an expensive sorted path that formatted all columns; sorting and selection now extract only the requested column and format other cells only for visible rows. Local release p95: cached navigation **0.293 ms**, restart sort **4.800 ms**, age sort **4.582 ms**, name sort **0.673 ms**, grouped dashboard with metrics **8.572 ms**, 100,000-line search **4.183 ms**, wrapped-log navigation **0.015 ms**. Under approximately 10,000 log lines/second, real terminal key-to-render was **12.6 ms p95**, first populated view **500 ms**. These are development-host measurements, not remote-cluster guarantees.

The smaller model investigation is recorded in `ai/evaluations/needle3-2026-09-28`. The 35 MB candidate failed basic scope/action cases in two schema layouts and is not integrated. The optional existing model remains experimental; packaging size is separate from command accuracy.

## Flat pod monitor (2026-09-28)

Removed pod grouping, its YAML configuration and CLI option. The home view now lists individual pods with readiness/status, age/restarts, CPU/memory and nodes. Sorts preserve selected pod identity across reorderings. Four quick filters, search, keyboard/mouse selection and direct inspection/logs operate on that same selection. The bottom dock shows selected-pod usage and budgets, session trends, owner/node, last restart/termination, container uptime/images/probe configuration, and UID-scoped warning events. D hides the dock; d changes its detail view. Missing measurements and incomplete limits stay unknown.

Acceptance passed: 563 unit/render tests plus the new event-worker cancellation test, targeted dashboard tests, warning-free Clippy, formatting, workflow lint, release build, and real-terminal monitor and log/selector workflows. The monitor workflow checks actual metrics-server readings, configured probes, identity-preserving sorting, filters/search, mouse selection, correct log target, repeated Escape, dock hiding, 80×24 resize and namespace isolation. Captured terminal colors/layout were visually reviewed. Local synthetic p95 for 10,000 pods with metrics was 8.160 ms (16 ms budget); cached table navigation 0.293 ms, restart sort 3.990 ms, age sort 4.414 ms.

The first remote run passed Linux and Kubernetes checks but exceeded the 16 ms monitor navigation budget on the slower macOS runner (29.572 ms p95). Derived pod rows now cache immutable watch objects, filter/sort state and the metrics revision, with a one-second age refresh. Selection still samples its own history; watch updates and new metrics invalidate the cache. Regression tests cover invalidation and pod identity. Local p95 is now 0.288 ms for cached 10,000-pod navigation and 8.974 ms for a full metrics refresh. The benchmark separately enforces the existing 16 ms input budget and 50 ms full-data-update budget.

## Pretrained native NLP upgrade (2026-09-28)

Replaced the experimental FunctionGemma bundle with pretrained Qwen3.5-2B Q4_K_M (1,280,835,840 bytes), using the pinned native CPU runtime. No training, cloud inference or Python service is required. Packaging verifies model/runtime SHA-256 values and ships Apache/MIT notices; weights remain optional release assets, not Git source. Bundle protocol discovery prevents using the old model with the new JSON prompt. The app constrains output to read/navigation actions, validates scope/conditions/targets, and previews every proposal. Inference remains cancellable and scoped to the current selection.

Acceptance: 568 unit/render/input tests, 11 live kind API tests, warning-free Clippy, formatting, workflow lint, actual offline native-model integration and real-terminal NLP preview/cancellation passed. The standard vocabulary passed 14/14; the final built-in + native + validation path passed 45/45 development regression cases, including 20 unsupported requests. Raw model scores and failures remain explicit in ai/evaluations/qwen-2026-09-28; these limited tests do not establish general model accuracy.

The final live log stress check passed at approximately 10,000 lines/second with 4.3 ms p95 key-to-render latency and 263 ms to the first populated view. The cache fix also passed all remote Linux, macOS and Kubernetes Acceptance jobs: https://github.com/ashark-ai-05/kube/actions/runs/36397707603 .

Final app Acceptance passed on Linux, macOS and Kubernetes at a579b55 (run 36399164207). AI packaging passed Linux but the hosted Mac exceeded the original 30-second cold-request deadline. The native worker now has a 60-second overall bound, two-second health requests and a 45-second completion-request bound inside that overall deadline. It remains a cancellable background task using two CPU threads; UI frame budgets and the prompt/model are unchanged. This permits slower cold loading instead of rejecting an otherwise healthy native worker.

## Pod queries, saved views and troubleshooting (2026-09-28)

The flat monitor now combines name/namespace text, label predicates, readiness, restarts, age and measured CPU/memory conditions. Queries compile once per edit and reuse derived pod facts until watch objects, metrics or the age tick change. Invalid syntax produces a visible error and no actionable rows. Unknown/stale measurements do not satisfy numeric comparisons. Selection follows pod UID while queries and sorting change.

Named local views capture the query, quick filter, columns and sort direction. Views always use the current cluster/namespace; column choices also persist independently. Versioned JSON writes are atomic, limited in size/count, and preserve malformed existing files. The column picker keeps pod names visible, and the table adapts to narrow terminals. Controls are visible in the monitor toolbar and command palette.

The selected-pod troubleshooting view connects recorded termination/waiting states, readiness/scheduling conditions and UID-scoped events to their source and recorded time. It distinguishes previous container instances and opens their retained logs directly. Event series use the latest observation and series count. Findings describe observations rather than inferred causes; missing evidence remains explicit. Wide terminals show evidence beside details; narrow terminals stack the panes with scrollable details.

Local acceptance passed: **578 unit/render/input tests**, **11 isolated kind API tests**, formatting, warning-free Clippy, release build and terminal restoration. Live terminal workflows verify combined metrics/labels/readiness queries, invalid filters, view/column persistence across restart, namespace isolation, sorting, repeated Escape, mouse selection, 80×24 layout, actual exit-code evidence and previous-container logs. Existing log/search/selector and scoped assistant-preview workflows also pass. Terminal captures were visually reviewed and the README screenshots updated.

Development-host synthetic p95 with 10,000 pods: combined-query editing **0.589 ms**, combined-query navigation **0.273 ms**, cached monitor navigation **0.255 ms**, full metric refresh **8.432 ms**. Log search across 100,000 lines measured **3.965 ms**. The benchmark enforces 16 ms for query editing/navigation and 50 ms for full refresh/search; these measurements exclude network/authentication latency.
