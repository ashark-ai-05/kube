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
