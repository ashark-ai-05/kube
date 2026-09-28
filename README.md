# kube

A fast Kubernetes terminal explorer with keyboard and mouse navigation, live workload logs, resource relationships, and explicitly confirmed operations. Rust + Ratatui + kube-rs.

```sh
cargo install --path . --locked
kube                   # current kubeconfig context and namespace
kube -n payments       # one namespace
kube -A                # all namespaces
```

Uses `KUBECONFIG` or `~/.kube/config`. No agent or service needs installing in the cluster. Normal browsing needs only the Kubernetes API. Exec and port-forward use `kubectl` on PATH. CPU/memory inspection uses metrics-server when available.

## Investigate

Select a Deployment and press Enter. The inspector provides Overview, YAML, Events, Logs, Related, and Metrics views. Related follows owner UIDs and selectors to ReplicaSets, Pods, containers, Services, EndpointSlices, nodes and storage. Select a related resource to keep investigating. Discovered custom resources use the server's printer columns and remain browsable without plugins.

Logs aggregate up to 16 pod/container streams, include source labels and timestamps, and follow replacement pods. Choose a container, inspect previous-container logs, search literal text or `re:patterns`, pause/follow, choose a time window, and export the filtered buffer. Kubernetes only exposes retained container logs; this is not a historical logging backend. Reconnect deduplication is best effort for identical timestamps.

## Controls

| Key | Action |
| --- | --- |
| `:` or click the top bar | Searchable command palette |
| `?` | Help |
| `/` | Fuzzy resource filter; combine with `label:app=api` or `label:tier!=batch` |
| `Tab`, arrows, `j`/`k` | Focus and navigation |
| `Enter`, double-click | Inspect selection |
| `c` / `n` | Cluster / namespace picker |
| `l` / `r` | Logs / related resources |
| `1`–`6`, `Tab` in inspector | Inspector tabs |
| `[` / `]`, `b` | Resize / collapse sidebar |
| Drag pane divider | Resize sidebar or inspector |
| `m` | Toggle mouse capture for native terminal copying |
| `Ctrl-U` in resource filter | Clear query |
| `Esc` / `q` | Close inspector or quit |
| `Ctrl-C` | Quit |

Inspector actions are clickable. Logs: `f` pause/follow, `c` cycle containers, `p` previous instance, `s` time window, `/` search, `j` JSON display, `e` export, `y` clipboard. Exports create a new file and never overwrite an existing file. Clipboard uses `pbcopy` on macOS or `xclip` on Linux. Pane sizes and mouse preference are saved to `$XDG_CONFIG_HOME/kube/preferences.json` (normally `~/.config/kube/preferences.json`).

## Operations

Starts read-only. `:write` enables confirmations; `:readonly` disables them. `:scale 3`, `:restart`, and `:delete` show the cluster, namespace, resource and operation, and require typing the resource name. UID checks prevent changing a replacement resource; resource-version preconditions protect concurrent changes. Changing context resets write mode.

Select a Pod and use `:exec` (or `:exec CONTAINER`) for an interactive shell. `:forward 8080:80` binds only to localhost; `:stop-forwards` stops this application's forwards. Forwards stop when the application exits or the context changes.

Secret data and last-applied Secret annotations are redacted in YAML, copy and export. Auth-plugin failures do not print credential output. Logs may contain application-emitted sensitive data and are shown as received, with terminal control characters removed.

## Performance and limits

Pods begin loading while API discovery runs. Resource kinds are watched on demand; eight recently visited kinds remain warm. Older watches and caches are evicted. Watch notifications coalesce before queuing, input is processed in bounded batches, and the table formats visible rows. Selection follows resource identity across updates.

Each log view retains at most 100,000 lines and 32 MiB of text/source payload, with a 64 KiB line cap and a bounded stream queue. The status line reports evicted lines. Total process memory additionally includes resource caches and runtime/UI overhead; caches grow with the resources in the watched scopes.

Run `cargo bench --locked --bench responsiveness` for repeatable synthetic performance checks. Local acceptance results and limitations are in [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md).

## Development and acceptance

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --locked
python3 scripts/terminal-smoke.py
cargo bench --locked --bench responsiveness
```

Live tests must use the isolated kind fixture:

```sh
export KUBECONFIG="$PWD/acceptance-kubeconfig.yaml"
export CLUSTER=kube-tui-acceptance
bash scripts/dev-cluster.sh
cargo test --locked --test integration_kind -- --ignored
python3 -m pip install -r scripts/requirements-test.txt
python3 scripts/tui-cluster-smoke.py
cargo build --release --locked
python3 scripts/tui-cluster-smoke.py target/release/kube --stress
```

This creates a `demo` namespace, nginx deployment and restricted ServiceAccount. Tests create/delete their own fixture resources. CI runs unit/render/terminal tests on Linux and macOS and Kubernetes acceptance on Linux. The binary workflow produces Linux x86-64 and macOS ARM64 archives with checksums on version tags or manual dispatch.
