# kube

A fast Kubernetes terminal explorer with keyboard and mouse navigation, live workload logs, resource relationships, and explicitly confirmed operations. Rust + Ratatui + kube-rs.

```sh
cargo install --path . --locked
kube                   # current kubeconfig context and namespace
kube -n payments       # one namespace
kube -A                # all namespaces
kube --kubeconfig ~/clusters.yaml  # your multi-cluster YAML file
kube --kubeconfig ~/clusters.yaml --context production
```

Uses `KUBECONFIG` or `~/.kube/config` by default. `--kubeconfig PATH` overrides that source; repeat the flag to merge files. The cluster picker lists the file’s **contexts** (cluster + credentials + optional namespace). Switching contexts does not modify the file or its `current-context`. Exec and port-forward use the same selected configuration. No agent or service needs installing in the cluster. Normal browsing needs only the Kubernetes API. Exec and port-forward use `kubectl` on PATH. CPU/memory inspection uses metrics-server when available.

## Workspace

Fleet radar opens with readiness counts, a pod attention queue and namespace signals for the selected scope. Counts come from the current watch; it does not query every cluster. Running pods without readiness evidence are shown as unknown.

`F2` opens Fleet radar. `F3` opens Focus studio, the resource browser. The compact rail contains common workloads and recent investigations; `Ctrl-R` or **All resources** opens the full discovered API catalog, including CRDs. `Tab` moves between the rail, resource list and inspector. The inspector adds readiness and owner context with direct Events, Logs and Related tabs. Cluster and namespace selectors stay visible throughout. Tables hide the API’s optional wide columns on smaller terminals to leave more room for resource names.

![Focus studio with wrapped and folded logs](docs/images/focus-studio.png)

## Pod groups and live dashboard

Fleet radar (`F2`) groups pods by `app.kubernetes.io/name`, then `app`, then their controller owner. Pods without those fields appear as Ungrouped. Namespaces remain separate. Each group shows readiness, restarts, node count and measured CPU/memory. Select with arrows and Enter, or click a group, to browse only its pods; Esc returns to the dashboard. `g` focuses groups, `a` focuses the attention queue, and `F3` returns to the full resource list. The selected group's grouping source and node names appear below the cards.

CPU and memory charts use the current namespace scope and refresh every five seconds while the dashboard is visible. They keep up to 60 local samples; switching namespace or cluster clears history and cancels old requests. Install metrics-server and grant list access to `pods.metrics.k8s.io` to see usage. Missing, denied or stale measurements are labelled unavailable; partial coverage shows the measured pod count. These charts are session history, not Prometheus or historical monitoring. Inspect an individual pod's Metrics tab for its container usage.

Override grouping with `kube --groups /path/to/groups.yaml`, or save a file at `$XDG_CONFIG_HOME/kube/groups.yaml` (normally `~/.config/kube/groups.yaml`). See [examples/pod-groups.yaml](examples/pod-groups.yaml). Rules support namespace, Kubernetes label selectors, name prefixes and regexes. All conditions within a rule must match; the first matching rule wins, followed by the default labels/owner fallback. Files are validated before entering the terminal UI.

## Investigate

Select a Deployment and press Enter. The inspector provides Overview, YAML, Events, Logs, Related, and Metrics views. Related follows owner UIDs and selectors to ReplicaSets, Pods, containers, Services, EndpointSlices, nodes and storage. Select a related resource to keep investigating. Discovered custom resources use the server's printer columns and remain browsable without plugins.

Structured JSON logs open in a message view with explicit severity badges and compact timestamps. Click a record or press Enter to expand its JSON details; J toggles formatted JSON and o toggles the original record. Search automatically reveals full records so hidden metadata stays searchable; exports preserve the originals. Repeated single-container source labels move to the toolbar.

Logs open in a wide reading pane; `z` expands the inspector across the terminal. Long messages wrap by default, including Unicode and long JSON fields. Search highlights matching text while keeping surrounding lines visible; `F` switches to matching lines only. Compact timestamps and container labels leave room for the actual message. Consecutive repeated messages and Java-style stack frames collapse into expandable groups (`v` toggles folding). Search automatically expands groups, and copy/export retain every raw record.

Logs aggregate up to 16 pod/container streams, include source labels and timestamps, and follow replacement pods. Choose a container, inspect previous-container logs, search literal text or `re:patterns`, pause/follow, choose a time window, and export the filtered buffer. Kubernetes only exposes retained container logs; this is not a historical logging backend. Reconnect deduplication is best effort for identical timestamps.

## Controls

| Key | Action |
| --- | --- |
| `F2` / `F3` | Fleet radar / Focus studio |
| `g` / `a` on Fleet radar | Focus pod groups / attention queue |
| `Ctrl-R` / All resources | Toggle the complete API resource catalog |
| `:` / `Ctrl-K` / click Commands | Searchable command palette |
| `?` | Help |
| `s` / `:sort` | Choose a sort column and direction; click a column header to toggle |
| `/` | Fuzzy resource filter; combine with `label:app=api` or `label:tier!=batch` |
| `Tab` / `Shift-Tab`, arrows, `j`/`k` | Switch pane and navigate |
| `Enter`, double-click | Inspect selection |
| `Ctrl-O` / `Ctrl-N`, or click top-bar selectors | Cluster / namespace picker from any pane |
| `c` / `n` in resource browser | Cluster / namespace picker |
| `l` / `r` | Logs / related resources |
| `1`–`6`, or click inspector tabs | Inspector views |
| `z` in inspector | Maximize / restore |
| `[` / `]`, `b` | Resize / collapse sidebar |
| Drag pane divider | Resize sidebar or inspector |
| `m` | Toggle mouse capture for native terminal copying |
| `Ctrl-U` in resource filter | Clear query |
| `Esc` | Close or go back; does nothing at the root |
| `q` | Close inspector; quit from the resource browser |
| `Ctrl-C` | Quit |

Sorting uses elapsed time for age (including compound values such as 2d3h), numerical restart counts (including last-restart annotations), and alphabetical names. The active header shows ↑ or ↓.

Inspector actions are clickable. Logs: `v` fold/expand repeated messages and stack frames; `w` wrap on/off; `←`/`→` pan unwrapped lines; `/` or `Ctrl-F` search; `n`/`N` next/previous matching line; `F` matching lines/context; `Ctrl-U` clear an editing query; `Esc` cancel editing or clear a committed search; `Home` oldest retained line; `End` live tail; `f` pause/follow; `c` cycle containers; `p` previous instance; `s` time window; `J` pretty JSON; `e` export; `y` clipboard. Exports create a new file and never overwrite an existing file. Clipboard uses `pbcopy` on macOS or `xclip` on Linux. Pane sizes and mouse preference are saved to `$XDG_CONFIG_HOME/kube/preferences.json` (normally `~/.config/kube/preferences.json`).

## Ask Kube (optional local AI)

Press `Ctrl-Space`, click **Ask Kube**, or enter `:ask`. Try `show failing pods in payments`, `show deployments`, `previous logs for this pod`, `why is this deployment stuck?`, or `switch to staging`. Review the displayed cluster, namespace, selected resource and proposed action, then press Enter to apply. Esc cancels. This path only reads and navigates; existing explicit commands handle mutations.

Common phrases are interpreted directly and work in the standard binary. The optional **AI edition** also includes FunctionGemma 270M Q8_0 (291,557,792 bytes) and a CPU runtime for other wording. Extract the whole archive and run `./kube`; keep the adjacent `ai/` directory. There is no separate model installation or runtime download. The weights are release assets and are not committed to Git.

Local model suggestions are **experimental**. The unfine-tuned model can choose the wrong action or arguments; unsupported arguments and invented namespace defaults are rejected, and every accepted suggestion still requires a visible preview. It is not a general Kubernetes expert. Diagnosis opens the app's observed status, events and logs rather than generating root-cause claims. Only the typed request and fixed action definitions reach the worker; kubeconfigs, credentials, object contents and log buffers are never passed to it.

The worker starts only when needed, uses two CPU threads and a 2,048-token context, and exits after each request or cancellation. It listens only on an authenticated loopback connection; the TUI remains responsive. `KUBE_AI_DIR=/path/to/ai` can select a locally assembled bundle. With no bundle installed, built-in phrases still work and other wording gets an actionable explanation.

To assemble the optional distribution (network needed **at packaging time**):

```sh
cargo build --release --locked
mkdir package
cp target/release/kube package/
python3 scripts/package-ai.py package --platform macos-arm64  # or ubuntu-x64
KUBE_AI_DIR="$PWD/package/ai" cargo test --locked --test assistant_local -- --ignored
cargo run --locked --example assistant-eval
```

The packager verifies pinned SHA-256 hashes and the model size budget and includes the model/runtime licenses. The **Build binaries** workflow has an `include_ai` option to produce both editions. `cargo run --example assistant-eval -- --model-only` measures raw model accuracy separately; failures are expected with the current experimental model and are documented in [ai/README.md](ai/README.md).

![Ask Kube action preview](docs/images/ask-kube.png)

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
python3 scripts/tui-ux-smoke.py target/release/kube
bash scripts/dev-metrics.sh  # disposable kind fixture only
python3 scripts/dashboard-smoke.py target/release/kube
```

This creates a `demo` namespace, nginx deployment and restricted ServiceAccount. Tests create/delete their own fixture resources. CI runs unit/render/terminal tests on Linux and macOS and Kubernetes acceptance on Linux. The binary workflow produces Linux x86-64 and macOS ARM64 archives with checksums on version tags or manual dispatch.

The visual design takes cues from [Ratatui’s app showcase](https://ratatui.rs/showcase/apps/): visible keyboard actions, distinct pane focus, compact metadata, and a dedicated reading area. Set `KUBE_UI_CAPTURE_DIR` when running the UX smoke test to save terminal-cell snapshots for visual review.
