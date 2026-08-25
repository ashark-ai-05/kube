# Plan 2 API Reference — kube 4.2 / ratatui 0.30 (verified by compilation)

All snippets below were compiled with `cargo check` against exactly the dependency set
Plan 2 will use:

```toml
crossterm = { version = "0.29", features = ["event-stream"] }
futures = "0.3.34"
http = "1"                                    # NEW — needed to touch headers directly
k8s-openapi = { version = "0.28", features = ["latest"] }
kube = { version = "4.2", features = ["runtime", "client", "derive", "oidc", "http-proxy", "socks5", "gzip"] }
ratatui = "0.30"
serde = { version = "1", features = ["derive"] }   # NEW — needed to derive Deserialize on our own types
serde_json = "1.0.151"
tokio = { version = "1", features = ["full"] }
```

`http` and `serde` were added as **direct** dependencies (they were already pulled in
transitively by `kube`, but Rust 2024 requires a direct `Cargo.toml` entry to `use` a
crate directly — e.g. `http::header::ACCEPT`, `#[derive(serde::Deserialize)]`). Plan 2
should add both.

Scratch project: `/tmp/claude-1000/-home-kashar-Development-kube/46f5096c-448a-41d2-af49-c4fbb544a857/scratchpad/apiprobe`
Probes 1–16 were pre-existing and already verified; probes 17–29 below were added for
this task. All 29 compile clean (`cargo check` — 16 dead-code warnings, 0 errors).

---

## A. Discovery and CRDs

### A1/A2. Enumerate resources: kind, plural, group, version, scope, verbs, list/watch support

```rust
use kube::discovery::{Discovery, Scope, verbs};

let disc = Discovery::new(client).run().await?;
for group in disc.groups() {
    let group_name: &str = group.name();
    for (ar, caps) in group.recommended_resources() {
        let kind: &str = &ar.kind;
        let plural: &str = &ar.plural;
        let group: &str = &ar.group;
        let version: &str = &ar.version;
        let api_version: &str = &ar.api_version;
        let namespaced: bool = matches!(caps.scope, Scope::Namespaced);
        let ops: &Vec<String> = &caps.operations;          // raw list of verb strings
        let supports_list: bool = caps.supports_operation(verbs::LIST);
        let supports_watch: bool = caps.supports_operation(verbs::WATCH);
    }
}
```

- `ApiResource` (in `kube::discovery` / re-exported from `kube_core::discovery`) has fields
  `group`, `version`, `api_version`, `kind`, `plural` — all plain `String`, all public.
- `ApiCapabilities` has `scope: Scope` (`Scope::Cluster | Scope::Namespaced`, a plain enum —
  **not** a bool, match on it or use `matches!`), `subresources: Vec<(ApiResource, ApiCapabilities)>`,
  and `operations: Vec<String>`.
- **Exact type/field for list/watch support (A2)**: there is no dedicated bool field. Use
  `ApiCapabilities::supports_operation(operation: &str) -> bool` with the string constants
  in `kube::discovery::verbs` (`verbs::LIST = "list"`, `verbs::WATCH = "watch"`, plus `GET`,
  `CREATE`, `DELETE`, `DELETE_COLLECTION`, `UPDATE`, `PATCH`). Equivalently check
  `caps.operations.contains(&"watch".to_string())`, but `supports_operation` is the intended API.
- `group.recommended_resources()` gives the **preferred-version** resource list per group
  (what you almost always want for a resource browser). `group.versioned_resources(ver)` and
  `group.resources_by_stability()` are also available if you need other version selections.

### A3. `Api<DynamicObject>` for an arbitrary discovered resource (namespaced + cluster-scoped)

```rust
use kube::discovery::Scope;
use kube::api::{Api, DynamicObject};

let disc = kube::discovery::Discovery::new(client.clone()).run().await?;
for group in disc.groups() {
    for (ar, caps) in group.recommended_resources() {
        let api: Api<DynamicObject> = match caps.scope {
            Scope::Cluster    => Api::all_with(client.clone(), &ar),
            Scope::Namespaced => Api::namespaced_with(client.clone(), "default", &ar),
        };
        let _url = api.resource_url(); // e.g. "/apis/apps/v1/namespaces/default/deployments"
    }
}
```

`Api::resource_url(&self) -> &str` is the key method for B4 below — it's how you get the
base path to build a raw request against an arbitrary discovered `ApiResource`.

---

## B. Server-side table rendering

### B4. `Accept: application/json;as=Table;v=1;g=meta.k8s.io` — **does NOT exist as a ready-made API in kube 4.2**

Searched exhaustively: `kube-core-4.2.0/src`, `kube-client-4.2.0/src` — **there is no
`kube::core::Table`, `TableRow`, `TableColumnDefinition`, or `request_table`-style method
anywhere in kube 4.2.** The only Accept-header content negotiation kube-rs ships is for
`PartialObjectMetadata` (`JSON_METADATA_MIME` in `kube-core/src/request.rs`, used by
`Api::get_metadata`/`list_metadata`), not for the Table format. `k8s-openapi 0.28` also has
no `Table` type anywhere under `apimachinery::pkg::apis::meta::v1` (checked `object_meta.rs`
et al. in that module — the file list has no `table.rs`).

**This confirms the finding from the task brief: server-side Table requires a raw
`http::Request` via `Client::request`, with a hand-rolled response type.** Working code:

```rust
use kube::core::Request as KubeRequest;
use kube::api::{Api, ListParams};
use k8s_openapi::api::core::v1::Pod;

#[derive(serde::Deserialize, Debug)]
struct TableColumnDefinition {
    name: String,
    #[serde(rename = "type")]
    type_: String,
    format: Option<String>,
    description: Option<String>,
    priority: Option<i32>,
}

#[derive(serde::Deserialize, Debug)]
struct TableRow {
    cells: Vec<serde_json::Value>,
    // object: Option<serde_json::Value>, // only present if includeObject=Object is requested
}

#[derive(serde::Deserialize, Debug)]
struct Table {
    #[serde(rename = "columnDefinitions")]
    column_definitions: Vec<TableColumnDefinition>,
    rows: Vec<TableRow>,
}

let api: Api<Pod> = Api::namespaced(client.clone(), "default");
let kreq = KubeRequest::new(api.resource_url());          // Api::resource_url() gives the base path
let mut http_req = kreq.list(&ListParams::default())?;    // -> http::Request<Vec<u8>>
http_req.headers_mut().insert(
    http::header::ACCEPT,
    http::HeaderValue::from_static("application/json;as=Table;v=1;g=meta.k8s.io"),
);
let table: Table = client.request(http_req).await?;
```

Notes:
- `kube::core::Request::new(url_path)` + its `.list(&ListParams)` / `.get(name, &GetParams)`
  builder methods return an `http::Request<Vec<u8>>` — this is the "raw request" surface;
  it does not set an Accept header itself, so you insert one on the returned request before
  sending.
- `Client::request<T: DeserializeOwned>(request: http::Request<Vec<u8>>) -> Result<T>` sends
  it and JSON-decodes the body into your own type. There's also `request_text` and
  `request_stream` if you want the raw body instead.
- This works equally well for `DynamicObject`/CRDs: build the `Api<DynamicObject>` as in A3,
  call `.resource_url()`, and proceed identically.
- **`http` must be added as a direct dependency** to touch `http::header::ACCEPT` /
  `http::HeaderValue` — it's already in the dependency graph via `kube`, but Rust requires a
  direct manifest entry to `use` it.

### B5. Reading column names/types and per-row cells

Shown in the `Table`/`TableColumnDefinition`/`TableRow` structs above:
`column_definitions[i].name`, `.type_` (renamed from JSON `"type"` because `type` is a Rust
keyword), `.format`, `.priority` (used by kubectl's wide/narrow column logic); `rows[i].cells`
is `Vec<serde_json::Value>` — cells are heterogeneous (strings, numbers, sometimes nested
objects for things like `containers`), so leaving them as raw `serde_json::Value` and calling
`.to_string()` / `.as_str()` as needed per-column is the pragmatic approach. There is no
per-cell type info beyond the column's declared `type` (`"string"`, `"integer"`, `"boolean"`, etc.).

---

## C. Object detail

### C6. `DynamicObject` → YAML — **no YAML serializer exists in the current dependency set**

Checked all current dependencies (`kube`, `k8s-openapi`, `serde_json`, `futures`,
`crossterm`, `ratatui`, `tokio`) — **none of them expose YAML serialization.** `serde_json`
can only produce JSON. `DynamicObject` does implement `Serialize`/`Deserialize` (it derives
them, `data: serde_json::Value` holds the object body), so JSON round-tripping works fine:

```rust
let obj: DynamicObject = dapi.get("mypod").await?;
let json_pretty: String = serde_json::to_string_pretty(&obj)?;   // compiles, but it's JSON not YAML
```

**Verdict: to render a YAML view, Plan 2 must add a YAML-emitting dependency.** `serde_yaml`
is deprecated upstream (archived by dtolnay) but still the most common choice; alternatives
are `serde_norway` (an actively maintained fork) or `yaml-rust2` (lower-level, no serde
integration). This is a dependency decision the plan needs to make explicitly — it does not
come for free. (Per instructions, no dependency was added in the scratch project; this is
reported, not silently worked around.)

### C7. Listing `Event` objects for a specific object — field selector form

```rust
use k8s_openapi::api::core::v1::Event;
use kube::api::{Api, ListParams};

let api: Api<Event> = Api::namespaced(client, "default");
let selector = format!(
    "involvedObject.name={},involvedObject.namespace={}",
    "mypod", "default"
);
let lp = ListParams::default().fields(&selector);
let events = api.list(&lp).await?;
for ev in events.items {
    let _msg = ev.message;
}
```

`ListParams::fields(&self, field_selector: &str) -> Self` is the builder method (field is
`field_selector: Option<String>` internally, same as `label_selector`/`.labels()`). The
comma-joined `involvedObject.name=X,involvedObject.namespace=Y` form is exactly what the
brief expected and it compiles/type-checks as written.

### C8. `managedFields`, `ownerReferences`, labels/annotations off a `DynamicObject`

All four come from the `ResourceExt` trait (`kube::api::ResourceExt`, already used for
`.name_any()` in existing probes) — no special-casing needed for `DynamicObject`:

```rust
use kube::api::ResourceExt;

let labels: &std::collections::BTreeMap<String, String> = obj.labels();
let annotations: &std::collections::BTreeMap<String, String> = obj.annotations();
let owners: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference] = obj.owner_references();
let managed: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::ManagedFieldsEntry] = obj.managed_fields();
```

(There are also `_mut` variants of all four if the plan ever needs to mutate.)

---

## D. ratatui 0.30 widgets

### D9. `Tabs`

```rust
use ratatui::widgets::Tabs;

let titles = vec!["Overview", "YAML", "Events", "Logs"];
let tabs = Tabs::new(titles)
    .block(Block::default().borders(Borders::ALL))
    .select(selected)                                   // usize, or Into<Option<usize>> to deselect
    .style(Style::default())
    .highlight_style(Style::default().fg(Color::Yellow));
f.render_widget(tabs, area);
```

`Tabs::new` takes anything iterable into `Line`s. `.select(T: Into<Option<usize>>)` — pass a
plain `usize` or `None`/`Some(usize)`. `.divider(...)`, `.padding(...)` also exist if needed.

### D10. `Scrollbar` / `ScrollbarState`

```rust
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

let mut state = ScrollbarState::default(); // or ::new(content_length)
let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
    .begin_symbol(Some("↑"))
    .end_symbol(Some("↓"));
f.render_stateful_widget(sb, area, &mut state);
```

Render it in the *same area* as (or an inset of) the list/paragraph it scrolls — `Scrollbar`
draws only its own track/thumb, it does not clip or affect the sibling widget. `ScrollbarState`
has `.position(usize)`, `.content_length(usize)`, `.next()`, `.scroll(ScrollDirection)`.

### D11. `List` + `ListState`, indented/nested tree

```rust
use ratatui::widgets::{List, ListItem, ListState};

let items = vec![
    ListItem::new("apps"),
    ListItem::new("  deployments"),
    ListItem::new("    my-deploy"),
    ListItem::new("  replicasets"),
];
let list = List::new(items)
    .block(Block::default().borders(Borders::ALL))
    .highlight_style(Style::default().fg(Color::Yellow))
    .highlight_symbol(">> ");
f.render_stateful_widget(list, area, &mut state); // state: &mut ListState

state.select(Some(1));
state.select_next();     // and select_previous(), scroll_down_by(n), scroll_up_by(n)
```

There is **no built-in tree widget** in ratatui 0.30's own widget set (the crate has no
`Tree`/`TreeItem`) — "nested tree" rendering is done exactly as shown: flatten the tree into
a `Vec<ListItem>` yourself with leading-space (or box-drawing character) indentation encoding
depth, and track expand/collapse state alongside your own tree model. If a real tree widget
is wanted, that means `tui-tree-widget` (a third-party crate), not anything shipped with ratatui.

### D12. `Paragraph` with `Wrap` and scroll offset

```rust
use ratatui::widgets::{Paragraph, Wrap};

let p = Paragraph::new(yaml_text)
    .wrap(Wrap { trim: false })     // trim:true strips leading whitespace on wrapped lines — false preserves YAML indentation
    .scroll((scroll_y, 0))          // (vertical, horizontal) offset in Paragraph::scroll((u16, u16))
    .block(Block::default().borders(Borders::ALL));
f.render_widget(p, area);
```

For a YAML view, `Wrap { trim: false }` is what you want (trimming would destroy indentation
semantics). Both `.wrap()` and `.scroll()` are `const fn` builders.

### D13. `Clear` for a modal/popup

```rust
use ratatui::widgets::Clear;

let popup_area = Rect { x: area.x + 4, y: area.y + 2,
                         width: area.width.saturating_sub(8), height: area.height.saturating_sub(4) };
f.render_widget(Clear, popup_area);   // resets cells in popup_area first
f.render_widget(Block::default().borders(Borders::ALL).title("Popup"), popup_area);
```

`Clear` is a zero-field unit struct; render it before whatever you're overdrawing with, in
the exact target `Rect`. Note ratatui's own doc caveat: `Clear` cannot be used to clear the
*whole terminal* on the very first frame (assumes render area starts empty) — only fine for
overdrawing an already-rendered area, which is exactly the popup use case.

### D14. Per-column x-offsets for `Table` — `Layout::horizontal(constraints).split(area)` alone is **NOT** sufficient

Read `Table`'s actual internal layout code (`ratatui-widgets-0.3.2/src/table.rs`,
`get_column_widths`, lines ~1041–1065). Table's real column layout is a **two-stage** split:

```rust
// Stage 1: reserve the row-selection-symbol column (0-width unless a row is/can be selected)
let [_selection_area, columns_area] =
    Layout::horizontal([Constraint::Length(selection_width), Constraint::Fill(0)])
        .areas(Rect::new(0, 0, max_width, 1));

// Stage 2: split the remaining area by the user's widths, honoring column_spacing
let rects = Layout::horizontal(widths)
    .flex(self.flex)                 // Table's flex setting, default Flex::Start
    .spacing(self.column_spacing)    // default 1
    .split(columns_area);
```

`selection_width` is `self.highlight_symbol.width()` if `highlight_spacing.should_add(has_selection)`
is true, else `0`. `HighlightSpacing::default()` is `WhenSelected`, and `highlight_symbol`
defaults to an empty `Text` (width 0) — so **for a `Table` built with defaults and no row
currently selected, `selection_width` is 0** and the naive single-stage split happens to match.
But if the plan's table ever sets `.highlight_spacing(HighlightSpacing::Always)` or has a row
selected while using `WhenSelected` (the common case for an interactive table!), the selection
column silently eats width and `Layout::horizontal(widths).split(area)` alone will be off.

**Verified helper that reproduces Table's real layout** (compiled as probe 28):

```rust
fn table_column_offsets(area: Rect, widths: &[Constraint], column_spacing: u16, selection_width: u16) -> Rc<[Rect]> {
    let [_selection_area, columns_area] =
        Layout::horizontal([Constraint::Length(selection_width), Constraint::Fill(0)]).areas::<2>(area);
    Layout::horizontal(widths.to_vec())
        .spacing(column_spacing)
        .split(columns_area)
}
```

For clickable column headers: compute `selection_width` the same way `Table` does (mirror
`highlight_spacing`/`highlight_symbol` you pass to the real `Table`, or simplest — always pass
0 if the plan's tables never show a highlight symbol / use row background highlighting instead
of a symbol), then call this helper with the header `Rect` to get per-column click zones.

---

## E. Streaming

### E15. `Api::log_stream` with `LogParams` — confirmed still compiles under kube 4.2

```rust
use kube::api::LogParams;

let lp = LogParams {
    container: None,
    follow: true,
    limit_bytes: None,
    pretty: false,
    previous: false,
    since_seconds: Some(3600),
    since_time: None,
    tail_lines: Some(200),
    timestamps: true,
};
let stream = api.log_stream("mypod", &lp).await?;
```

Exact field list on `LogParams` (`kube-core-4.2.0/src/subresource.rs`): `container: Option<String>`,
`follow: bool`, `limit_bytes: Option<i64>`, `pretty: bool`, `previous: bool`,
`since_seconds: Option<i64>`, `since_time: Option<Timestamp>`, `tail_lines: Option<i64>`,
`timestamps: bool`. Note: there is **no `insecure_skip_tls_verify_backend` field** in kube
4.2's `LogParams` (that field does not exist in this crate version — if remembered from
somewhere, drop it). `LogParams` also implements `Default`, so `LogParams { follow: true,
..Default::default() }` works and is less brittle than a full struct literal.

---

## Design implications

1. **Server-side Table (B4) is unavailable as a first-class API** — it requires a raw
   `http::Request` plus a hand-rolled `Table`/`TableColumnDefinition`/`TableRow` deserializer
   (shown above, ~25 lines, fully verified). This is workable but means Plan 2 cannot lean on
   kube-rs to do this for free; budget for writing and testing that decode path once and
   reusing it for every kind, including CRDs (via `Api::resource_url()` off a discovered
   `ApiResource`).
2. **No YAML serializer exists in the current dependency set (C6)** — Plan 2 needs an explicit
   dependency decision for the YAML detail view (e.g. `serde_yaml` despite its deprecated
   status, or `serde_norway` as an actively-maintained fork). This should be called out and
   decided before implementation starts, not discovered mid-build.
3. **Clickable table column headers (D14) cannot use a naive `Layout::horizontal(widths).split(area)`** — it only matches `Table`'s real layout when `selection_width` is 0 (no highlight symbol / no selection with `WhenSelected` spacing). Plan 2's click-hit-testing code must replicate the two-stage split (selection column reservation, then `.spacing(column_spacing)`) shown above, or deliberately keep `highlight_symbol` empty everywhere clickable headers are needed to make the naive split valid.
