# Plan 3 API Reference — kube 4.2 / ratatui 0.30 (verified by compilation)

Builds on `docs/superpowers/plan2-api-reference.md` (same scratch project, same method).
Everything below was compiled with `cargo check` against Plan 2's dependency set **plus
two new direct dependencies added for this task**:

```toml
serde_norway = "0.9"     # NEW — YAML serialisation (resolves to 0.9.42)
unicode-width = "0.2"    # NEW — needed to `use unicode_width::UnicodeWidthStr` directly
                          #       for Tabs click hit-testing (Rust 2024 direct-use rule,
                          #       same reason `http`/`serde` were added in Plan 2)
```

Scratch project: `/tmp/claude-1000/-home-kashar-Development-kube/46f5096c-448a-41d2-af49-c4fbb544a857/scratchpad/apiprobe`
Probes 1–29 were pre-existing (Plan 1/2, already verified). Probes 30–43 were added for
this task. **All compile clean** (`cargo check` — 16 dead-code warnings, all pre-existing,
0 errors, 0 new warnings because every new item is `#[allow(dead_code)]` or is exercised by
a `#[test]`). Two probes are `#[test]` functions that were actually **run** (not just
compiled) to get real evidence for the YAML output and the scroll semantics — both pass.

Per-project note: `unicode-width` turned out to already be in the dependency graph
transitively (pulled in by `kube-client` → `serde-saphyr`, its internal YAML-parsing dep for
kubeconfig, *and* by `ratatui-core`) — adding it directly costs nothing new to the build.

---

## A. YAML serialisation with `serde_norway`

### A1/A2. Does it serialise a `DynamicObject` cleanly, and does key order match kubectl?

Built a realistic Pod as a `DynamicObject` (multi-line annotation, a unicode annotation
value, a `null` field in `status`, an empty list) and ran it through
`serde_norway::to_string`. **Actual output** (probe 30, run via
`cargo test probe_yaml_output -- --nocapture`):

```yaml
apiVersion: v1
kind: Pod
metadata:
  annotations:
    kubectl.kubernetes.io/last-applied-configuration: |
      line one
      line two
      line three
    note: 'unicode: héllo wörld 日本語 🚀'
  creationTimestamp: 2026-08-20T10:00:00Z
  labels:
    app: web
    tier: frontend
  name: web-7c9c9
  namespace: payments
spec:
  containers:
  - image: nginx:1.25
    name: app
    ports:
    - containerPort: 80
  nodeName: node-1
status:
  conditions: []
  hostIP: null
  phase: Running
  startTime: 2026-08-20T10:00:05Z
```

Observations, verified by reading source, not guessed:

- **`apiVersion`/`kind`/`metadata`/`spec`/`status` lead, and within each block keys are
  alphabetised** (`annotations, creationTimestamp, labels, name, namespace`; `image, name,
  ports`; `conditions, hostIP, phase, startTime`). This is **not** `serde_norway` doing
  something clever — it's two separate, boring facts compounding:
  1. `k8s-openapi`'s generated `ObjectMeta` struct (and every other typed struct in that
     crate) already declares its fields in **alphabetical order** in the codegen'd source
     (`k8s-openapi-0.28.0/src/v1_36/apimachinery/pkg/apis/meta/v1/object_meta.rs`) — serde
     derives serialise struct fields in declaration order, so alphabetical-in-source becomes
     alphabetical-in-output for free.
  2. `DynamicObject.data` is a bare `serde_json::Value`; this project's `serde_json` has no
     `preserve_order` feature, so `serde_json::Map` is backed by a `BTreeMap` — iterating it
     for serialisation is unavoidably key-sorted.
  - **This happens to match `kubectl get -o yaml`**, which also alphabetises field names —
    so the output is not a discrepancy from what users expect, it's the same convention.
    The one theoretical edge case: a top-level key that alphabetically sorts before `kind`
    (e.g. a hypothetical CRD with a top-level field starting with `a`–`j`) would land before
    `kind`/`metadata`; this doesn't happen for any core built-in type and is out of scope
    to fix (it would require a custom serialiser, not a config flag).
- **Multi-line strings** render as YAML block scalars (`|`) automatically — no manual
  escaping needed, and it stays human-readable.
- **Unicode** (`héllo wörld 日本語 🚀`) is emitted literally, not `\uXXXX`-escaped. It gets
  single-quoted only because the string's content (`unicode: ...`) contains a colon+space,
  which would otherwise parse as a nested mapping — that's YAML syntax, not an escaping choice.
- **`null` handling differs by origin**: a `null` inside `data` (arbitrary JSON — e.g. our
  injected `status.hostIP: null`) is emitted literally as `null`, because raw
  `serde_json::Value::Null` has no `skip_serializing_if`. A `None` on a *typed* struct field
  with `#[serde(skip_serializing_if = "Option::is_none")]` (most `ObjectMeta`/k8s-openapi
  fields) is simply **omitted** — which is why unset fields like `generation`,
  `resourceVersion`, `uid`, etc. don't appear at all above. Both behaviours are what you want
  for a Kubernetes viewer: known-empty struct fields disappear, but values the server actually
  sent as `null` are shown as `null` (visible fidelity), not silently dropped.

**Verdict for the brief's question ("is this good enough to show a user without
post-processing?"): yes.** No post-processing is needed for readability — it already matches
kubectl's own convention. The only thing worth adding is a `let obj = strip_managed_fields(obj)`-style
pre-serialisation step if the plan wants to hide `metadata.managedFields` (a UX choice, not
a `serde_norway` limitation).

### A2 (continued). Round-trip fidelity

```rust
let obj = build_sample_pod();
let yaml = serde_norway::to_string(&obj)?;
let back: DynamicObject = serde_norway::from_str(&yaml)?;
assert_eq!(obj.metadata.name, back.metadata.name);
assert_eq!(obj.data, back.data);              // serde_json::Value equality ignores key order
let yaml2 = serde_norway::to_string(&back)?;
assert_eq!(yaml, yaml2);                       // re-serialising is stable/idempotent
```

All three assertions pass (probe 30/31, `cargo test probe_yaml_output`). Nothing is lost —
key reordering happens on the way *in* (to alphabetical) but is then stable on every
subsequent round trip, so a "load → edit → save" flow (if the plan ever wants one) would not
thrash unrelated lines on every save.

### A3. Resolved version and dependency weight

```
serde_norway v0.9.42
├── indexmap v2.14.0 (+ equivalent, hashbrown, allocator-api2, foldhash)
├── itoa v1.0.18            (already pulled in by serde_json)
├── ryu v1.0.23             (already pulled in by serde_json)
├── serde v1.0.229          (already a direct dep)
└── unsafe-libyaml-norway v0.2.15
```

`unsafe-libyaml-norway` is a **pure-Rust, C2Rust-transpiled** port of libyaml (same lineage
as the original `unsafe-libyaml` behind `serde_yaml`) — no `build.rs`, no `cc`/`bindgen`,
no system libyaml linkage. The only genuinely new crate family pulled in is `indexmap` (used
internally by the YAML emitter, not exposed in the public API). Net addition to the build:
small, pure Rust, no C toolchain requirement. This is a low-risk dependency to add.

---

## B. Watching many kinds at once

### B4. Concurrent `watcher::watcher` for N kinds against one `Client` — client-side limits

Verified by reading source (not by hitting a real 40-kind cluster, which this environment
doesn't have):

- **`kube::Client` derives `Clone`**, and the clone is cheap by design: internally it's a
  `tower::buffer::Buffer<Request<Body>, BoxFuture<...>>` (`kube-client-4.2.0/src/client/mod.rs`)
  — a handle to an mpsc-backed worker task, not a fresh connection. Cloning it for each
  watched kind (as probe 33 does) is the intended pattern; it is exactly what `Api::all`/
  `Api::namespaced` expect you to do.
- **The `Buffer` has a fixed queue capacity of 1024** (`Buffer::new(service, 1024)`,
  `kube-client-4.2.0/src/client/mod.rs:168`). This is a depth limit on *requests awaiting
  processing by the underlying service*, not a limit on concurrently-open long-lived watch
  streams — once a watch's headers come back, its body stream is driven independently by the
  caller's `.next().await` loop and no longer occupies a buffer slot. 1024 is far above any
  plausible "number of installed CRDs + built-ins" count.
- **The underlying `hyper_util` connection pool has no per-host cap in kube-client's
  config.** `kube-client-4.2.0/src/client/builder.rs` builds the transport with
  `hyper_util::client::legacy::Builder::new(TokioExecutor::new()).build(connector)` and never
  calls `.pool_max_idle_per_host(...)`. Reading `hyper-util-0.1.20/src/client/legacy/pool.rs`,
  the field defaults to `usize::MAX` when unset — i.e. **no artificial connection-count
  throttle exists in kube-client itself.** (HTTP/1.1 watch connections are long-lived and
  won't go idle/get pooled away mid-stream regardless.)
- **Conclusion for eager-vs-lazy watching**: nothing in `kube-client`'s HTTP stack forces
  lazy/on-expand watching for a "watch everything eagerly" sidebar design at typical scales
  (dozens of kinds). The real constraints, if any, are server-side (apiserver watch-cache
  memory, etcd watch fan-out) and are outside what this client library controls — that's an
  operational/cluster-sizing concern for the plan to note as a caveat, not a client-code
  blocker. A resource-conscious middle ground (e.g. cap concurrent eager watches at, say,
  50–100, or watch built-ins eagerly and CRDs lazily) is a design choice, not something kube
  forces on you.

```rust
async fn probe_concurrent_watches(client: Client, resources: Vec<(ApiResource, Scope)>) {
    let mut handles = Vec::new();
    for (ar, scope) in resources {
        let client = client.clone();                 // cheap: Buffer handle clone
        handles.push(tokio::spawn(async move {
            let api: Api<DynamicObject> = Api::all_with(client, &ar); // or namespaced_with
            let stream = watcher::watcher(api, WatcherConfig::default());
            futures::pin_mut!(stream);
            while let Some(_ev) = stream.next().await {}
        }));
    }
    for h in handles { let _ = h.await; }
}
```

### B5. `ApiCapabilities::supports_operation(verbs::WATCH)` for filtering

Already established in Plan 2 (A2) and re-confirmed here — compiles unchanged:

```rust
use kube::discovery::verbs;
fn is_watchable(caps: &kube::discovery::ApiCapabilities) -> bool {
    caps.supports_operation(verbs::WATCH)
}
```

Use this to skip kinds like some aggregated-API or subresource-only resources that support
`list`/`get` but not `watch`, before spawning a watcher task for them.

### B6. RBAC-forbidden watch vs. transient failure — exact error shape

`kube::runtime::watcher::Error` (`kube-runtime-4.2.0/src/watcher.rs`) has exactly 5 variants:

```rust
pub enum Error {
    InitialListFailed(#[source] kube_client::Error),   // api.list() failed before watching started
    WatchStartFailed(#[source] kube_client::Error),    // api.watch() call itself failed
    WatchError(#[source] Box<Status>),                 // a WatchEvent::Error frame mid-stream
    WatchFailed(#[source] kube_client::Error),         // the watch stream itself errored
    NoResourceVersion,                                 // missing metadata.resourceVersion
}
```

The doc comment on the enum is explicit: *"These are all considered retryable from a
watcher's point of view, even though they may require patching of rbac/netpols in the
background to fix."* — i.e. kube-rs does **not** pre-classify RBAC failures as a distinct,
non-retryable variant. You have to unwrap one level further, into `kube::Error::Api(Box<Status>)`:

```rust
pub struct Status {
    pub code: u16,          // plain HTTP-style status code, e.g. 403, 404, 410
    pub reason: String,     // e.g. "Forbidden"
    // ...
}
impl Status {
    pub fn is_forbidden(&self) -> bool { self.reason_or_code(reason::FORBIDDEN, 403) }
    pub fn is_not_found(&self) -> bool { /* similar, 404 */ }
    // is_conflict, is_invalid, is_already_exists, ...
}
```

`is_forbidden()` mirrors the Go client: true if `reason == "Forbidden"` **or** `code == 403`
(`kube-core-4.2.0/src/response.rs`). Verified classification helper (probe 35, compiles):

```rust
fn classify(err: &watcher::Error) -> &'static str {
    match err {
        watcher::Error::InitialListFailed(kube::Error::Api(status))
        | watcher::Error::WatchStartFailed(kube::Error::Api(status))
        | watcher::Error::WatchFailed(kube::Error::Api(status)) => {
            if status.is_forbidden() || status.code == 403 { "rbac-forbidden" }
            else if status.code == 410 { "resource-version-too-old-transient" }
            else { "other-api-error" }
        }
        watcher::Error::WatchError(status) if status.is_forbidden() => "rbac-forbidden-mid-stream",
        watcher::Error::WatchError(_) => "other-watch-event-error",
        watcher::Error::NoResourceVersion => "no-resource-version",
        _ => "non-api-error-transient",   // hyper/tower/io errors — treat as retryable
    }
}
```

**Yes, RBAC-forbidden is distinguishable from transient**: match on `kube::Error::Api(status)`
inside the three request-failure variants and check `status.is_forbidden()`/`status.code`.
Anything that *isn't* `kube::Error::Api` (raw hyper/tower/io errors wrapped by the same three
variants) should be treated as transient and retried with backoff; a 403 should stop retrying
and instead mark that kind as "no access" in the sidebar rather than looping forever.

`kube::error::ErrorResponse` still exists as a **deprecated** re-export of `Status` — use
`Status` directly (what `kube::Error::Api` actually wraps), not `ErrorResponse`.

---

## C. Events for the detail pane

### C7. Field-selector listing — re-confirmed unchanged from Plan 2

```rust
use k8s_openapi::api::core::v1::Event;
use kube::api::{Api, ListParams};

let api: Api<Event> = Api::namespaced(client, "default");
let selector = format!("involvedObject.name={},involvedObject.namespace={}", "mypod", "default");
let lp = ListParams::default().fields(&selector);
let events = api.list(&lp).await?;
```

Still compiles verbatim as probe 21 / Plan 2's C7. No change in kube 4.2.

### C8. `Event` field names/types (feature `latest` = k8s-openapi `v1_36`)

Checked `k8s-openapi-0.28.0/src/v1_36/api/core/v1/event.rs` (fields identical across
v1_32..v1_36 — no version-specific differences):

| Field | Type | Notes |
|---|---|---|
| `reason` | `Option<String>` | short machine-ish reason string |
| `message` | `Option<String>` | human-readable message |
| `type_` | `Option<String>` | **plain `String`, not an enum** — values are conventionally `"Normal"` / `"Warning"`, but nothing in the type system enforces that; match on the string |
| `count` | `Option<i32>` | repeat count for a de-duplicated event |
| `event_time` | `Option<MicroTime>` | the modern (Events v1-style) timestamp field, microsecond precision |
| `first_timestamp` | `Option<Time>` | legacy, second precision |
| `last_timestamp` | `Option<Time>` | legacy, second precision |

All three timestamp-ish fields are independently optional — a real cluster's events API
(`events.k8s.io/v1` vs. core `v1.Event`) tends to populate `event_time` OR
`first_timestamp`/`last_timestamp` depending on which API path produced it, so the detail
pane should fall back through all three (`event_time` → `last_timestamp` → `first_timestamp`)
rather than assuming one is always set.

### C9. Live Events tab — watch, not poll

`Event` is a plain `k8s_openapi` type implementing `kube::Resource`, so `watcher::watcher`
works on it exactly like any other typed kind — no special "events watch" API needed. Narrow
it to one object using `watcher::Config::fields` (a method on `watcher::Config`, distinct from
`ListParams::fields` used for one-shot lists):

```rust
let api: Api<Event> = Api::namespaced(client, ns);
let cfg = watcher::Config::default().fields(&format!(
    "involvedObject.name={name},involvedObject.namespace={ns}"
));
let stream = watcher::watcher(api, cfg);
```

This compiles (probe 38). **The Events tab can be live, not polled** — same watch mechanism
as everything else in the plan, just scoped with a field selector instead of `Api::all`.

---

## D. ratatui 0.30 widgets

### D10. Tree rendering — no built-in `Tree`/`TreeItem`, confirmed again for the sidebar specifically

Same finding as Plan 2's D11, re-verified in the kind-tree's actual shape (group → kinds,
with live counts, expand/collapse per group):

```rust
struct KindTreeGroup { group_name: String, expanded: bool, kinds: Vec<(String, usize)> }

fn flatten_kind_tree(groups: &[KindTreeGroup]) -> Vec<ListItem<'static>> {
    let mut out = Vec::new();
    for g in groups {
        let marker = if g.expanded { "v" } else { ">" };
        out.push(ListItem::new(format!("{marker} {} ({})", g.group_name, g.kinds.len())));
        if g.expanded {
            for (kind, count) in &g.kinds {
                out.push(ListItem::new(format!("    {kind} ({count})")));
            }
        }
    }
    out
}
```

The cleanest way to keep this in sync (as flagged in the brief): **don't try to diff the tree
structure against the previous flattened `Vec<ListItem>`.** Keep the tree model (groups +
expanded flags + counts) as the single source of truth, and rebuild the flattened
`Vec<ListItem>` from scratch every frame — it's cheap (dozens to low hundreds of rows), and
it eliminates an entire class of "flattened view got out of sync with tree state" bugs. Only
`ListState.selected()` (an index into the flattened list) needs to persist across frames;
recompute an index→(group, kind) mapping alongside the flattened `Vec` each time you build it,
so a click/keypress on row N can be resolved back to "which tree node is this."

### D11. `Tabs` — no per-tab geometry; hit-testing must be computed manually

Read `ratatui-widgets-0.3.2/src/tabs.rs` in full: `Tabs`'s public surface is `new`, `titles`,
`block`, `select`, `style`, `highlight_style`, `divider`, `padding`/`padding_left`/
`padding_right` — **nothing returns per-tab `Rect`s or exposes tab boundaries.** For clickable
tabs, replicate its layout loop yourself:

```rust
use unicode_width::UnicodeWidthStr;

fn tabs_hit_test(area: Rect, titles: &[&str], divider_width: u16, pad_left: u16, pad_right: u16)
    -> Vec<(usize, Rect)>
{
    let mut spans = Vec::new();
    let mut x = area.x;
    for (i, title) in titles.iter().enumerate() {
        let w = title.width() as u16 + pad_left + pad_right;   // unicode-width, not .len()
        spans.push((i, Rect { x, y: area.y, width: w, height: 1 }));
        x += w + divider_width;
    }
    spans
}
```

Use `UnicodeWidthStr::width()` (not byte length or `.chars().count()`) for the title
measurement — matches how `Tabs` itself measures title width internally, and handles
multi-byte/wide characters correctly if any tab title ever isn't plain ASCII.
`unicode-width` needs to be a **direct** dependency to `use` it (see top of this doc);
it's already transitively present via `kube-client`/`ratatui-core`.

### D12. `Paragraph` + `Wrap` + `scroll((y, x))` — scroll semantics confirmed by rendering, not just reading docs

Rather than trust the doc comment, probe 41 actually renders into a `TestBackend` and reads
the buffer back:

```rust
let text = "line1\nline2\nline3\nline4\nline5\n";
term.draw(|f| {
    let p = Paragraph::new(text).wrap(Wrap { trim: false }).scroll((2, 0));
    f.render_widget(p, f.area());   // 10x3 TestBackend
}).unwrap();
// buffer's row 0 reads "line3..." — confirmed: scroll_y=2 skipped the first 2 wrapped lines
```

This **passes** (`cargo test probe_paragraph_scroll_semantics`). Confirmed:
- `scroll((y, x))` is a **post-wrap line/column offset** — `y` skips N lines of the *already
  wrapped* text, not N lines of the raw unwrapped source.
- **No manual line-splitting is needed** for a long YAML document — hand the whole YAML
  string to one `Paragraph`, let `.wrap(Wrap { trim: false })` do the wrapping (use
  `trim: false` for YAML specifically, since `trim: true` strips leading whitespace on
  wrapped continuation lines, which would corrupt indentation-sensitive YAML visually).
- The one thing you *do* need to compute yourself is **how many wrapped lines the text
  produced**, if you want an accurate scrollbar thumb (see D13) — `Paragraph` doesn't expose
  a "line count after wrapping" query.

### D13. `Scrollbar`/`ScrollbarState` kept in sync with a `Paragraph`'s scroll

```rust
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

let mut state = ScrollbarState::new(total_wrapped_lines.saturating_sub(viewport_height))
    .position(scroll_y as usize);
let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight);
f.render_stateful_widget(sb, area, &mut state);
```

`ScrollbarState::new(content_length)` and `.position(usize)`/`.content_length(usize)`/
`.viewport_content_length(usize)` are `const fn` builders (`ratatui-widgets-0.3.2/src/scrollbar.rs:419-459`).
**Caveat carried over from D12**: `content_length` should be the wrapped line count, not the
raw newline count in the YAML text, or the thumb size will be wrong for any line that wraps.
Since `Paragraph` won't tell you the wrapped count, the practical options are: (a) pre-wrap
the text yourself once (e.g. with the `textwrap` crate — not currently a dependency) purely to
get a line count for the scrollbar, while still handing the *unwrapped* text to `Paragraph`
itself, or (b) accept an approximate thumb sized on raw line count (fine for a first cut,
since YAML documents rarely have pathologically long single lines).

### D14. Syntax highlighting for YAML — nothing free, would need a new dependency

Checked the full dependency graph of this project (`kube`, `k8s-openapi`, `ratatui`,
`crossterm`, `tokio`, `futures`, `serde`/`serde_json`, `http`, plus the two added for this
task, `serde_norway`/`unicode-width`) — **no syntax-highlighting crate anywhere in the graph**
(no `syntect`, `tree-sitter`, `synoptic`, `two-face`). Per the brief, nothing was added; if
Plan 3 wants colourised YAML, it needs an explicit dependency decision, same category as the
YAML-serialiser decision in Plan 2's C6:
- `syntect` — the common choice, but heavy (bundled `sublime-syntax`/theme dumps, pulls in
  `onig` or `fancy-regex`); would need its own highlighted-span → ratatui `Span`/`Line`
  mapping layer, not integrated with ratatui out of the box.
- `synoptic` / `two-face` — lighter pure-Rust alternatives, less mature/less complete
  language coverage, same manual span-mapping work required either way.
No dependency was added in the scratch project for this — reported per instructions, not
worked around.

---

## Design implications

1. **YAML output needs no post-processing (A1/A2)** — `serde_norway`'s output already leads
   with `apiVersion`/`kind`/`metadata` and alphabetises everything else, which matches
   `kubectl get -o yaml` convention exactly (both facts are incidental — k8s-openapi's
   alphabetical struct-field codegen plus `serde_json::Value`'s default `BTreeMap` — not a
   deliberate `serde_norway` feature — but the end result is what users already expect).
   Multi-line strings, unicode, and explicit `null`s all render cleanly. **Plan 3 can budget
   zero engineering time for "make the YAML view readable"** beyond calling
   `serde_norway::to_string(&obj)` — that was not a safe assumption going in, since Plan 2 had
   flagged YAML as *entirely missing* from the dependency set.

2. **Eager watching of every discovered kind is not blocked by kube-client itself (B4)** —
   `Client::clone()` is a cheap handle clone (tower `Buffer`, capacity 1024 requests-in-flight,
   not a connection cap), and the underlying `hyper_util` pool has `pool_max_idle_per_host =
   usize::MAX` (never configured otherwise by kube-client). There is no client-side
   architectural reason to make watching lazy-on-expand. **This should flip the plan's default
   from "watch lazily to be safe" to "watch eagerly, with an explicit configurable cap"** (e.g.
   a `max_eager_watches` setting) as the one guard against genuinely pathological clusters
   (hundreds of CRDs) — the constraint, if any, is apiserver/etcd watch-cache load, which no
   client-side change can fix anyway, so it's an operational knob, not a code-path fork
   between "eager" and "lazy" implementations.

3. **RBAC-forbidden watches are silently lumped in with transient errors by kube-runtime's own
   type system (B6)** — `watcher::Error`'s doc comment explicitly says all 5 variants are
   "considered retryable," so the plan's watch-supervisor loop must not trust the outer enum
   variant alone. It has to pattern-match one level deeper into `kube::Error::Api(Status)` and
   check `status.is_forbidden()` / `status.code == 403` to distinguish "give up, mark this kind
   as no-access" from "backoff and retry" — otherwise a kind the user lacks RBAC for will retry
   forever at whatever backoff schedule the plan uses, which is silently wasteful (and, worse,
   indistinguishable in the UI from "cluster is having a bad moment").

Two smaller but concrete implications worth flagging alongside the three above:

4. **Tabs and the sidebar tree both require hand-rolled hit-testing/layout (D10/D11)** —
   consistent with Plan 2's D14 finding about `Table` column offsets: ratatui 0.30 generally
   does not expose post-layout geometry for compound widgets. Budget for a small shared
   "measure this widget's sub-regions" module rather than expecting the widgets to answer
   "what did you just draw where" questions.

5. **The YAML view's scrollbar thumb will be approximate unless wrapped-line-count is computed
   separately (D12/D13)** — `Paragraph` wraps text but never reports how many lines the wrap
   produced. This is a minor but real gap: either accept an approximate thumb (raw line count)
   or add a lightweight pre-wrap pass purely for the count. Not a blocker, but should be a
   conscious choice in the plan rather than a bug discovered during implementation.
