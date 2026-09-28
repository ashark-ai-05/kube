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
