# kube

A fast, mouse-driven Kubernetes TUI — Lens-class cluster browsing and log exploration
in the terminal.

> **Status: design phase.** No implementation yet. The architecture and v1 scope are
> specified in
> [`docs/superpowers/specs/2026-08-25-kube-tui-design.md`](docs/superpowers/specs/2026-08-25-kube-tui-design.md).

## What it aims to be

- **Fast.** A watch-driven in-memory cache, rendering only on change. Navigation within
  cached data targets a single frame; idle CPU targets zero.
- **Mouse-native.** Click, scroll, drag-to-resize, click-to-sort, double-click-to-open —
  built on a per-frame hit-test registry so every drawn element is clickable by
  construction, not as a special case.
- **Discoverable.** A persistent sidebar tree of resource kinds with live counts. No
  memorised commands required, with keyboard shortcuts as the fast path on top.
- **Good at logs.** Live tail with multi-pod and multi-container aggregation, regex
  search and filtering, time-range selection, and export as text, per-container dumps,
  or NDJSON.

## v1 scope

Read-only: multi-cluster context switching, resource browsing across core kinds and
CRDs, detail views (overview / YAML / events / logs), log streaming and export, and
querying via fuzzy filter, label selectors, field selectors, and jq.

Deliberately deferred to v2: editing and apply, exec, port-forwarding, metrics, and
cross-resource queries. Keeping v1 read-only removes the entire mutation risk surface
from the first milestone.

## Stack

Rust, [`ratatui`](https://ratatui.rs) for rendering, [`kube-rs`](https://kube.rs) for
the Kubernetes client, `tokio` for async I/O.

## Development

Requires a Kubernetes cluster for integration tests; development targets a local
[`kind`](https://kind.sigs.k8s.io) cluster.

```sh
cargo build
cargo test          # unit tests — no cluster required
```

Most of the system is testable without a cluster: the store is driven by synthetic
watch deltas, query and hit-testing are pure functions, and rendering is asserted
against `ratatui`'s `TestBackend`.

## License

MIT
