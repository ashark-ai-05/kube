# kube

A **native iced desktop app** for browsing a Kubernetes cluster from your kubeconfig.

Not affiliated with Lens, OpenLens, or k9s. Not a TUI.

## Run

```bash
cargo run --release
# or after install:
kube
kube -n default
kube -A
```

Uses `KUBECONFIG` or `~/.kube/config`. The binary is named `kube`. It is read-only.

Requires a desktop (windowed) environment. This is not a terminal UI.

Inspect answers are built from objects already fetched (namespace / name / event citations). The app runs without `OPENAI_API_KEY`. If that variable is set, inspect may add a short model note; it still does not invent live cluster state.

## What it does

- Connects with kube-rs using your kubeconfig.
- Sidebar of discovered kinds (Pods and other list+watch kinds).
- Click a kind, click a row, click Overview / YAML / Events / Logs / Inspect.
- Cluster and namespace pickers (mouse). Typed namespace names still work when listing namespaces is forbidden (RBAC 403).
- Secret data is redacted in YAML and inspect.
- RBAC 403s show as real errors, not empty tables pretending to be fine.

## Develop

```bash
cargo test    # no live cluster required
cargo build
```

Cluster-backed tests in `tests/integration_kind.rs` stay `#[ignore]`.

## Stack kept from the previous app

`src/cluster` (kubeconfig, connect, discovery, namespace list, redaction), `src/store` (watch cache, table fetch, events, RBAC classify), and `src/app/session` (cluster switch / namespace restart) are reused. The ratatui/crossterm UI is gone; the only UI is iced.
