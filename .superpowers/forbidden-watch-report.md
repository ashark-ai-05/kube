# Forbidden-watch fix report

Branch: `fix/forbidden-watch`

## What changed

1. **`src/store/rbac.rs` (new file)** — `WatchFailure` enum (`Forbidden { detail }`,
   `NotFound { detail }`, `Retryable`) and `classify(&watcher::Error) -> WatchFailure`.
   Unwraps one level past `watcher::Error` into `kube::Error::Api(Status)` and checks
   `status.is_forbidden()` / `status.is_not_found()`, per
   `docs/superpowers/plan3-api-reference.md` section B6. Matched through every
   `watcher::Error` variant that can carry a `kube::Error::Api`
   (`InitialListFailed`, `WatchStartFailed`, `WatchFailed`) plus `WatchError`, which
   carries a bare `Status` from a mid-stream `WatchEvent::Error` frame. Defaults to
   `Retryable` for everything else (500s, raw transport errors, 410 Gone,
   `NoResourceVersion`).

2. **`src/store/mod.rs`** — declared `pub mod rbac;` and re-exported
   `WatchFailure`/`classify`.

3. **`src/store/watch.rs`**:
   - `drive_watch` (the loop, extracted from `spawn_watch` so it's generic over any
     `Stream<Item = watcher::Result<...>>` and testable without a cluster) now
     branches on `classify(&e)`. `Forbidden`/`NotFound` set `WatchStatus::Failed`,
     emit one final `WatchStatus` + an `AppEvent::Error` with the actionable message,
     then `return` — not `break` — so the pre-existing "stream ended unexpectedly"
     epilogue (which assumes a died watch, not one stopped on purpose) never fires
     and buries the actionable message. `Retryable` keeps the original
     escalate-after-3 behavior unchanged.
   - `forbidden_message(kind_plural, namespace, detail)` and
     `not_found_message(kind_plural, detail)` build the status-bar text, remedy
     first (see truncation note below).
   - `spawn_watch` is now a thin wrapper: builds the `Api`, starts the stream, calls
     `drive_watch`.

4. **`src/main.rs`** — added a test only (`the_forbidden_watch_remedy_survives_truncation`);
   no production code changed here, since `truncate_error` already truncates by
   taking the first N chars, which is what makes "lead with the remedy" work.

## TDD evidence

- **Red commit** `7da37c8` — "test: classify forbidden watches (failing)". Added the
  full test suites for `rbac.rs` and `watch.rs` plus the truncation test in
  `main.rs`, against intentionally-wrong-but-compiling stubs (`classify` always
  `Retryable`, `drive_watch` never distinguishing/breaking, `forbidden_message`/
  `not_found_message` with no remedy). Result: **10 lib tests + 1 bin test failed**,
  229 lib tests passed, all for the intended reasons (verified by reading each
  panic message — none were test-design bugs).
- **Green commit** `be497b8` — "fix: stop retrying forbidden/gone watches, show an
  actionable remedy". Replaced the stubs with the real `classify`, the real
  `drive_watch` branching (`return` on permanent failure), and the real message
  builders. Result: **239 lib + 42 bin tests pass, 0 failed** (264 baseline + 17 new).

## Mutation checks (all four performed, restored, confirmed green after each)

1. **`classify` → unconditional `Retryable`.** Ran `cargo test --lib store::rbac`:
   the 5 forbidden/not-found tests failed (`forbidden_via_initial_list_failed_...`,
   `forbidden_via_watch_start_failed_...`, `forbidden_via_watch_failed_...`,
   `forbidden_via_watch_error_mid_stream_...`, `not_found_is_distinguishable_...`);
   the other 4 (500/transport/410/NoResourceVersion) stayed green. Restored →
   `test result: ok. 9 passed; 0 failed`.

2. **`classify` → unconditional `Forbidden { detail: String::new() }`.** Ran
   `cargo test --lib store::rbac`: 6 failed (`a_transport_error_is_retryable`,
   `a_410_gone_status_is_retryable_not_forbidden_or_not_found`,
   `a_500_is_retryable_not_forbidden`, `no_resource_version_is_retryable`,
   `not_found_is_distinguishable_from_forbidden`, and
   `forbidden_via_initial_list_failed_is_forbidden_not_retryable` — the last because
   the mutated detail was empty and no longer contained `"forbidden"`). Restored →
   `test result: ok. 9 passed; 0 failed`.

3. **Loop does not return/break on a permanent failure** (removed the `return` in
   the `Forbidden` arm of `drive_watch`). Ran `cargo test --lib store::watch`:
   `a_forbidden_error_stops_the_watch_and_marks_it_failed` failed (12 other watch
   tests stayed green, confirming the mutation was isolated). Restored →
   `test result: ok. 13 passed; 0 failed`.

4. **Remedy text dropped** (`forbidden_message` → `format!("{kind_plural} forbidden: {detail}")`,
   no `-n` remedy). Ran the full suite: 3 lib tests failed
   (`a_forbidden_error_emits_the_actionable_message_not_the_raw_one`,
   `cluster_scope_forbidden_message_leads_with_the_namespace_remedy`,
   `namespace_scoped_forbidden_message_does_not_suggest_a_flag_already_used`) plus
   the bin test `the_forbidden_watch_remedy_survives_truncation`. Restored → full
   suite green again (239 lib + 42 bin).

Final state after restoring: `cargo test` → 239 lib + 42 bin passed, 0 failed, 4
ignored (cluster-only integration tests, unaffected); `cargo fmt --check` clean;
`cargo clippy --all-targets -- -D warnings` clean.

## Exact message text a user now sees

Built from `forbidden_message("pods", <namespace>, <apiserver detail>)`, then run
through `truncate_error` (200-char budget) exactly as it would reach the status bar.

**Cluster scope** (`-A`, what the user in the bug report actually hit):

```
pods forbidden at cluster scope — try -n <namespace>, or press n to pick one: pods is forbidden: User "7dcd7309-ed14-4669-a6b6-ce3596c3dd07" cannot list resource "pods" in API group "" at the cluster …
```

(the apiserver's own detail — including its embedded OIDC subject UUID, passed
through unmodified per the credentials constraint — is what gets truncated; the
remedy up front survives intact.)

**Namespace scope** (watch was started with `-n payments` and still got denied):

```
pods forbidden in namespace payments — you don't have access to this namespace either: pods is forbidden: User "7dcd7309-ed14-4669-a6b6-ce3596c3dd07" cannot list resource "pods" in namespace "payments…
```

## Notes for Plan 3 Task 1

- Plan 3's B6 finding (unwrap into `kube::Error::Api(Status)`, check
  `is_forbidden()`/`code == 403`) is now implemented as `store::rbac::classify` and
  can be reused as-is for the eager multi-kind watching feature (B4/B5) — Task 1
  should **not** re-derive this classification; it should call `classify()` per
  kind and only spawn/keep a "no access" sidebar entry instead of a live watch when
  it returns `Forbidden`/`NotFound`.
- One thing this pulled forward that Task 1 should account for: `drive_watch` is
  now a free function generic over `Stream<Item = watcher::Result<...>>`, separate
  from `spawn_watch`'s `Api`/`Client` setup. If Task 1 wants per-kind watch
  supervision (e.g. a registry of handles, as hinted at by B4), it can reuse
  `drive_watch` directly per spawned task rather than re-deriving the loop.
- Not addressed here, left for Task 1/Plan 3 proper: a UI-visible "no access" state
  distinct from `WatchStatus::Failed` (both currently render the same way in the
  status bar) — B4/B5's sidebar-with-live-counts design will likely want to
  distinguish "denied" kinds visually (e.g. greyed out, no retry) from kinds that
  are `Failed` for other reasons. Nothing in this change blocks that; it's just not
  built, since the bug fix here only had a single status bar line to update, not a
  sidebar.
- Confirmed while doing this: `ApiResource.plural` (e.g. `"pods"`) is a reliable,
  already-available source for the lowercase-plural noun in messages — no guessed
  pluralization needed, and it matches the apiserver's own wording convention.
