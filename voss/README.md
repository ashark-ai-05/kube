# Voss

Native Kubernetes inspector. Fast, read-only, user kubeconfig only.

**Not affiliated with Lens, OpenLens, or k9s.** This is not a clone of those products.

## How to run

```bash
cd voss
cargo run --release
```

From the repo root:

```bash
cargo run -p voss --release
```

Uses the current kubecontext from `KUBECONFIG` or `~/.kube/config`. Missing kubeconfig or auth failures surface as real errors — the UI does not invent live cluster state.

Optional: set `OPENAI_API_KEY` for LLM-rewritten inspect answers. The app runs without it; inspect then uses deterministic retrieval + summary with citations (`ns` / `pod` / `event` / `log`).

## v0 scope

In:

- Connect current kubecontext
- Namespace picker
- Pod table: name, ready, phase, restarts, age, node
- Pod detail: containers, conditions, events, last N logs
- Secrets redacted by default
- HTTP 403 mapped to RBAC errors with resource and verb
- Inspect answers only from fetched objects, with citations

Out:

- Workload editors, ingress, Helm, multi-cluster UI, exec, apply, plugins, Lens-style layout

## Tests

```bash
cargo test -p voss
cargo build -p voss
```

No live cluster required for unit tests.
