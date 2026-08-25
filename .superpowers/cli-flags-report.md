# CLI Namespace Flags Implementation Report

## Problem Statement

User has 20+ Kubernetes clusters with hundreds of pods in each. Their kubeconfig contexts don't set the `namespace:` field, so the TUI falls back to watching the "default" namespace. On their clusters, "default" is empty — all real workloads live in named namespaces. When they run the TUI, they see an empty table with no explanation.

Solution: Add CLI flags to override namespace selection + a hint to guide users toward `-A` when they're watching an empty default namespace.

## What Was Built

### 1. CLI Argument Parsing
Added CLI argument parsing to support namespace selection via flags, matching kubectl's muscle memory:

- `-n <namespace>` / `--namespace <namespace>` — watch a specific namespace
- `-A` / `--all-namespaces` — watch all namespaces
- `-h` / `--help` — print usage to stdout and exit 0
- No flags — use kubeconfig context's namespace, falling back to "default" (original behavior)
- Unknown flags or missing values — print error to stderr and exit 2

### 2. Helpful Hint for Empty Default Namespace
When the table shows zero items AND the user is watching the default namespace (because it fell back from an unset kubeconfig), the status bar shows:
```
 prod-eu · default · 0 items · live   no pods here — try -A for all namespaces
```

This immediately helps users understand why they see an empty table.

The hint:
- Only appears when BOTH conditions are true: fallback to default + zero items
- Disappears as soon as items appear (namespace isn't actually empty) or an error occurs
- Does NOT appear when user explicitly selected a namespace (via `-n` or kubeconfig), because then an empty namespace is exactly what they asked for

## Implementation

### New Module: `src/cli.rs`
- **`NamespaceScope` enum**: `One(String)`, `All`, `FromContext`
- **`CliOutcome` enum**: `Run(scope)`, `Help`, `Error(msg)`
- **`parse_args<I, S>(args: I) -> CliOutcome`**: Pure, testable parser using `std::env::args()` directly (no external dependencies)

### Updated `src/lib.rs`
- Added `pub mod cli;` to expose the CLI module

### Updated `src/main.rs`
- Parse CLI args before terminal setup (stdout/stderr print before raw mode)
- Handle Help: print usage to stdout, exit 0
- Handle Error: print to stderr, exit 2
- Handle Run: resolve scope to namespace and display string, pass to watch
- Created `run_with_scope(cli_scope: NamespaceScope)` async function
- Resolve scopes to `Option<String>` for `spawn_watch`:
  - `One(ns)` → `Some(ns)`
  - `All` → `None` (existing API behavior for all-namespaces)
  - `FromContext` → `Some(context namespace or "default")`

### Added `should_hint_all_namespaces()` Helper in `src/cli.rs`
- Pure function `should_hint_all_namespaces(was_fallback: bool, item_count: usize) -> bool`
- Decides whether to show "try -A for all namespaces" hint in status bar
- Only returns true when both conditions are met:
  1. Namespace was the default fallback (context didn't specify a namespace)
  2. Watch has zero items (empty table)

### Updated `src/main.rs` (namespace resolution)
- Track whether context explicitly specified a namespace vs falling back to "default"
- Pass `is_fallback_namespace` boolean through to render logic
- Call `should_hint_all_namespaces()` to decide whether to display hint

### Updated `src/ui/views/status.rs`
- Added parameter `show_all_namespaces_hint: bool` to `render_status()`
- When hint is true and no error, append "no pods here — try -A for all namespaces" to status bar
- Error messages take precedence over hints (only show one)
- Added 4 tests covering hint logic:
  - `shows_hint_when_fallback_namespace_is_empty()` — hint displays correctly
  - `hides_hint_when_namespace_has_items()` — no hint when table has data
  - `hides_hint_when_error_is_present()` — error takes precedence
  - `hint_shows_only_on_fallback_with_zero_items()` in `src/cli.rs` — pure logic test

## TDD Evidence: RED then GREEN

### RED Phase (Mutation Test)
Made `parse_args` ignore `-A` and always return `FromContext`:

```
test cli::tests::short_all_flag_gives_all stdout ----
thread 'cli::tests::short_all_flag_gives_all' (669426) panicked at src/cli.rs:120:9:
assertion `left == right` failed
  left: Run(FromContext)
 right: Run(All)
```

### GREEN Phase (Restoration)
Restored logic, test passes:
```
test cli::tests::short_all_flag_gives_all ... ok
test result: ok. 1 passed; 0 failed; 0 ignored
```

## Conflict Resolution: `-n` + `-A` Behavior

**Decision: `-A` takes precedence over `-n`**

**Reasoning**: When a user explicitly passes both `-A` and `-n`, they may have intended to watch all namespaces but accidentally included `-n`. The more explicit "all namespaces" declaration should win. Additionally, if both flags are present, `-A` is unambiguous (all namespaces) while `-n` with an argument is compound. Giving `-A` final say is safer and simpler than rejecting the combination as an error.

**Tests pinning this behavior**:
- `all_takes_precedence_when_combined_with_namespace()` — `-n payments -A` → `All`
- `all_takes_precedence_even_if_namespace_comes_last()` — `-A -n payments` → `All`

## Test Coverage

### CLI Parser Tests (13 tests)
1. `no_args_gives_from_context` — empty args → FromContext
2. `short_namespace_flag_gives_one` — `-n payments` → One("payments")
3. `long_namespace_flag_gives_one` — `--namespace payments` → One("payments")
4. `short_all_flag_gives_all` — `-A` → All
5. `long_all_flag_gives_all` — `--all-namespaces` → All
6. `help_short_flag_gives_help` — `-h` → Help
7. `help_long_flag_gives_help` — `--help` → Help
8. `namespace_flag_without_value_is_error` — `-n` alone → Error
9. `long_namespace_flag_without_value_is_error` — `--namespace` alone → Error
10. `unknown_flag_is_error` — `--nope` → Error
11. `all_takes_precedence_when_combined_with_namespace` — `-n payments -A` → All
12. `all_takes_precedence_even_if_namespace_comes_last` — `-A -n payments` → All
13. `hint_shows_only_on_fallback_with_zero_items` — pure function test covering all 4 combinations

### Status Bar Tests (5 tests)
- `shows_context_and_namespace()` — original test, still passes
- `shows_the_object_count()` — original test, still passes
- `shows_all_namespaces_when_watching_all()` — renders "all namespaces" string correctly
- `shows_hint_when_fallback_namespace_is_empty()` — hint displays when default namespace is empty
- `hides_hint_when_namespace_has_items()` — no hint when table has data
- `hides_hint_when_error_is_present()` — error takes precedence over hint

**Test execution result**: 112 passing tests (108 lib, 4 bin), 2 integration tests ignored

## Files Changed

| File | Change |
|------|--------|
| `src/cli.rs` | NEW: CLI parser + hint decision logic; `parse_args()`, `should_hint_all_namespaces()` |
| `src/lib.rs` | Added `pub mod cli;` |
| `src/main.rs` | Parse CLI args first; track namespace fallback; call hint function; pass flag to render |
| `src/ui/views/status.rs` | Added `show_all_namespaces_hint` parameter; render hint text when appropriate; 4 new tests |

## Self-Review Checklist

### CLI Flags Feature
- ✅ No new dependencies (manual parsing of `std::env::args()`)
- ✅ Parser is pure and testable (takes iterator, no global state)
- ✅ No `unwrap()`/`expect()` outside tests
- ✅ Terminal setup order correct: parse args → handle Help/Error → setup terminal → run loop
- ✅ Help and errors print before raw mode (no terminal corruption)
- ✅ Mutation testing confirms tests catch real behavior changes

### Namespace Selection
- ✅ `-n <namespace>` works (tests: `short_namespace_flag_gives_one`, etc.)
- ✅ `-A` / `--all-namespaces` works (tests: `short_all_flag_gives_all`, etc.)
- ✅ `-A` takes precedence over `-n` (tests: `all_takes_precedence_*`)
- ✅ Status bar displays "all namespaces" when watching all

### Empty Namespace Hint
- ✅ Hint appears when: default fallback + zero items (test: `shows_hint_when_fallback_namespace_is_empty`)
- ✅ Hint hidden when: items present (test: `hides_hint_when_namespace_has_items`)
- ✅ Hint hidden when: error displayed (test: `hides_hint_when_error_is_present`)
- ✅ Pure hint decision function (test: `hint_shows_only_on_fallback_with_zero_items`)
- ✅ Namespace fallback tracking works correctly

### Code Quality
- ✅ All 112 tests pass (108 lib + 4 bin)
- ✅ `cargo fmt --check` passes
- ✅ `cargo clippy --all-targets -- -D warnings` passes
- ✅ 2 integration tests remain ignored (as before)
- ✅ Backward compatible: no flags uses original context behavior

## Commits

1. **50c89b1** — Add CLI flags for namespace selection: `-n/--namespace` and `-A/--all-namespaces`
   - 12 CLI parser tests
   - 1 status bar test (all namespaces display)
   - Mutation testing confirms tests work

2. **41dbc40** — Add 'try -A for all namespaces' hint when default namespace is empty
   - Pure function `should_hint_all_namespaces()` with 1 test covering all 4 combinations
   - 3 status bar rendering tests (hint appears, hint hidden with items, hint hidden with error)
   - Tracks namespace provenance (fallback vs explicit)

## Concerns

**None identified.** Both features are minimal, focused, and well-tested. Hints only appear in appropriate contexts (fallback + empty). Namespace selection is backward-compatible (no flags = current behavior). Parser is completely decoupled from runtime. All constraints satisfied: no dependencies, no unsafe unwraps, clean clippy/fmt, comprehensive test coverage including mutation proof and pure logic tests.
