# Namespace picker: list real namespaces, with a working fallback when listing is forbidden

## The gap

`cargo run -- -A` against a corporate cluster (`prd-ld7`) whose RBAC forbids
listing pods at cluster scope loads 0 objects. Pressing `n` opened the
namespace picker, but `namespace_picker_items` built its list solely from
namespaces observed in already-loaded objects — a circular dependency: with
0 objects, the picker was empty exactly when it was needed to escape that
state.

## What changed

- **`src/cluster/namespaces.rs`** (new): `list_namespaces(client) -> Result<Vec<String>, NamespaceListError>`
  fetches every namespace via `Api::<Namespace>::all` /
  `ListParams::default()`, sorted alphabetically. `NamespaceListError` has
  `Forbidden(String)` and `Other(String)` variants, carrying display text
  (not the `kube::Error` itself) so it stays `Clone`/`Eq` and can travel
  through `AppEvent`. The 403-vs-anything-else classification is done by
  delegating to `store::rbac::classify_kube_error` (see next point), not by
  re-deriving the `Status`-code check. `is_valid_namespace_name` implements
  the DNS-1123 label rule used by the type-to-enter guard.
- **`src/store/rbac.rs`**: pulled `classify_kube_error(&kube::Error) -> WatchFailure`
  out of `classify(&watcher::Error)` so both a watch (which has a
  `watcher::Error` to unwrap) and a one-shot `Api::list` call (which only
  ever has a bare `kube::Error`) share one 403/404/500/transport
  classification instead of two copies of "unwrap into
  `kube::Error::Api(Status)` and check the code."
- **`src/app/event.rs`**: new `AppEvent::NamespacesListed(Result<Vec<String>, NamespaceListError>)`,
  coalesced into `Coalesced::namespace_list` keeping only the latest result
  in a batch (same rule `WatchStatus` already uses per-kind).
- **`src/app/session.rs`**: `Session` gained `namespaces_from_api: Option<Result<Vec<String>, NamespaceListError>>`,
  written under the *same lock* as `client`/`namespace` — not a second,
  independently-updated place to hold this. `switch_cluster`'s success path
  resets it to `None`: a listing fetched against the outgoing cluster names
  namespaces that may not even exist on the new one.
- **`src/main.rs`**:
  - `merge_namespace_names` unions the three sources (API listing, loaded
    objects, current namespace), deduplicated and sorted.
  - `namespace_picker_items` builds the picker's items from that merge, plus
    an explanation appended to the always-present "all namespaces" entry
    when the API result is an `Err` (see picker contents below).
  - `resolve_confirm` (new, replacing inline logic previously duplicated for
    the cluster and namespace pickers) resolves a confirmed picker index: if
    it names an existing item, that item wins; only when nothing matched is
    the *typed filter text* tried as a namespace name, guarded by
    `is_valid_namespace_name` before it ever becomes a request.
  - `Action::OpenNamespacePicker` opens the picker immediately from whatever
    is already known, then spawns `list_namespaces` on a task whose answer
    comes back through `AppEvent::NamespacesListed`.
- **`src/store/watch.rs`**: no wording change. `forbidden_message`'s
  "press n to pick one" is now checked true in both the listing-permitted
  and listing-forbidden cases (new test + doc comment); it wasn't accurate
  before this fix on exactly the cluster it fires for.

## Where the fetched list lives, and why

`Session::namespaces_from_api`, written under the same `Mutex<Session>` lock
as `client` and `namespace`. `list_namespaces` is async I/O; the render
closure in `main.rs` stays synchronous and acquires no locks, unchanged.
The fetch is spawned via `tokio::spawn` when `Action::OpenNamespacePicker`
fires, using the client cloned from that iteration's single session-lock
read (`client` is now part of the same one-lock-acquisition tuple as
`store`/`namespace`/etc.), and its result is delivered back through the
existing `AppEvent` channel — the same pattern `store::watch::spawn_watch`
already uses for live watches. The main loop applies a landed
`AppEvent::NamespacesListed` to `session.namespaces_from_api` *before* that
iteration's snapshot read, so an answer arriving in the same batch as the
open action is visible immediately; otherwise it lands on the next redraw.
Putting it anywhere else (a local in the event loop, a side `Arc<Mutex<..>>`)
would have been a second source of truth alongside `client`/`namespace`,
which the project has already been burned by twice.

## TDD evidence

- `fd4c2ac` — `test: namespace picker lists real namespaces (failing)`.
  Added the new module (`cluster::namespaces`) and all new production
  functions (`classify_list_result`, `is_valid_namespace_name`,
  `merge_namespace_names`, `namespace_picker_items`'s forbidden-note
  handling, `resolve_confirm`, `coalesce`'s `NamespacesListed` arm) as
  compiling stubs with deliberately wrong bodies, plus the full test suite
  for the feature. Confirmed real failures before committing:
  - lib: `cargo test --lib` → **245 passed, 9 failed** (8 in
    `cluster::namespaces`, 1 in `app::event`).
  - bin: `cargo test --bin kube` → **45 passed, 8 failed** (all in
    `tests::overlay_wiring`).
- `74203b1` — `fix: list real namespaces in the picker, with a working
  fallback when listing is forbidden`. Replaced each stub with its real
  implementation. Final state: **255 lib + 53 bin tests pass** (was 239 lib
  + 42 bin at the start of this task), 4 ignored (pre-existing
  cluster-required integration tests, untouched), `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings` both clean.

## Mutation checks (run before the implementation commit)

For each, the described mutation was re-applied to the already-correct
code, the specific test was run and observed to fail, then reverted and
re-confirmed green.

1. **Merge uses only the loaded-objects source.**
   `merge_namespace_names_deduplicates_sorts_and_keeps_every_sources_own_name` →
   ```
   assertion `left == right` failed: expected the sorted union of all three sources, deduplicated
     left: ["alpha", "zeta"]
    right: ["alpha", "mercury", "venus", "zeta"]
   test result: FAILED. 0 passed; 1 failed
   ```
   Restored → `test result: ok. 1 passed`.

2. **The name-validity guard always returns true.**
   `cluster::namespaces` invalid-name tests (5 of them) →
   ```
   assertion failed: !is_valid_namespace_name("-abc")
   assertion failed: !is_valid_namespace_name(&"a".repeat(64))
   assertion failed: !is_valid_namespace_name("kube/system")
   assertion failed: !is_valid_namespace_name("")
   assertion failed: !is_valid_namespace_name("Default")
   test result: FAILED. 4 passed; 5 failed
   ```
   and downstream, `resolve_confirm_rejects_unmatched_invalid_filter_text` →
   ```
   assertion `left == right` failed: a name that could never be valid must be rejected, not sent to the apiserver
     left: NamespaceChosen(Some("Not Valid!"))
    right: InvalidNamespaceTyped("Not Valid!")
   test result: FAILED. 0 passed; 1 failed
   ```
   Restored → both green.

3. **Enter always uses the typed text, ignoring a matching item.**
   `resolve_confirm_selects_the_matching_item_over_the_typed_filter_text` →
   ```
   assertion `left == right` failed: an item the user can see and pick must win over the typed filter
     left: NamespaceChosen(Some("e"))
    right: NamespaceChosen(Some("dev"))
   test result: FAILED. 0 passed; 1 failed
   ```
   Restored → `test result: ok. 1 passed`.

4. **The forbidden case yields an empty/unexplained list rather than the
   explanatory state.**
   `a_forbidden_listing_shows_an_explanation_instead_of_an_empty_picker` →
   ```
   the picker must explain that listing failed; got ["watch every namespace  ·  current"]
   test result: FAILED. 0 passed; 1 failed
   ```
   Restored → `test result: ok. 1 passed`.

Final full-suite confirmation after restoring all four:
`cargo fmt --check` → clean; `cargo test` → 255 lib + 53 bin passed, 0
failed, 4 ignored; `cargo clippy --all-targets -- -D warnings` → clean.

## Exact picker contents, by case

All examples assume a `payments` namespace is *not* current unless stated;
`ALL_NAMESPACES_LABEL = "all namespaces"`.

**1. Listing permitted** — API returned `["default", "kube-system", "payments"]`,
current namespace `Some("payments")`:

```
all namespaces        watch every namespace
default
kube-system
payments               current            <- marked, distinct accent colour
```

**2. Listing forbidden, nothing loaded** (the exact reported bug: 0 pods,
`-A` scope, cluster-scope RBAC denies both pods and namespaces):

```
all namespaces        watch every namespace  ·  current  ·  namespaces could not be listed (forbidden) — type a name and press Enter
```

One item, but never an empty list — the explanation and the type-to-enter
instruction are always visible, in the same line the user is already
looking at. Typing e.g. `payments` and pressing Enter switches straight to
it (validated locally; the request itself needs no listing permission).

**3. Nothing loaded yet** (namespace picker opened for the very first time,
before the spawned `list_namespaces` fetch has answered; 0 objects loaded):

```
all namespaces        watch every namespace  ·  current
```

Fills in on the next redraw once the fetch answers (permitted → full list
appears; forbidden → the explanation from case 2 appears) — never blocks
the picker from opening.

## Final 403 (forbidden-watch) message text

Unchanged wording — re-verified as accurate rather than edited:

```
{kind_plural} forbidden at cluster scope — try -n <namespace>, or press n to pick one: {detail}
```

e.g. `pods forbidden at cluster scope — try -n <namespace>, or press n to
pick one: pods is forbidden: User "u" cannot list resource "pods" at the
cluster scope`. "press n to pick one" is now true in both cases: when
listing namespaces is permitted, `n` opens a real list; when it's also
forbidden, `n` opens a picker that says so and still accepts a typed name.
Before this fix it was false on exactly the cluster it fires for. Namespace-
scoped forbidden message (already-narrowed watch denied too) is unchanged:
`{kind_plural} forbidden in namespace {ns} — you don't have access to this
namespace either: {detail}`.

## Notes / concerns

- `is_valid_namespace_name` implements the DNS-1123 label rule as specified
  (lowercase alphanumerics and `-`, 1-63 chars, no leading/trailing `-`); it
  does not additionally require the apiserver's stricter "must not be one of
  the reserved prefixes" or similar cluster-specific admission rules — those
  can still reject a syntactically valid typed name server-side, which is
  reported through the normal watch/connect error path, not specially here.
- Repeatedly opening/closing the namespace picker re-fetches every time
  (no caching beyond the last answer surviving on `Session` until the next
  fetch or cluster switch) — matches "fetch when the picker is opened"
  literally; a cluster where listing is denied pays one extra always-403'd
  GET per open, which is cheap but worth knowing about.
