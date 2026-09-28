# kube

A fast, keyboard and mouse driven Kubernetes terminal explorer built with Rust, Ratatui and kube-rs.

```sh
cargo run --release -- -n default
cargo run --release -- -A
```

Uses `KUBECONFIG` or `~/.kube/config`. Browse discovered kinds and CRDs, inspect YAML and events, and switch contexts and namespaces.

`Tab` changes focus, arrows or `j/k` navigate, `Enter` inspects, `c` chooses a cluster, `n` chooses a namespace, `Esc` closes a panel, and `q` quits. Mouse click, wheel, column sorting and double-click inspection are supported.

## Development

```sh
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
```

Cluster tests require an isolated development cluster; see `scripts/dev-cluster.sh`. Delivery milestones and performance targets are tracked in [docs/IMPLEMENTATION.md](docs/IMPLEMENTATION.md).
