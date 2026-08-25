# CLI Namespace Flags Implementation Report

## What Was Built

Added CLI argument parsing to support namespace selection via flags, matching kubectl's muscle memory:

- `-n <namespace>` / `--namespace <namespace>` — watch a specific namespace
- `-A` / `--all-namespaces` — watch all namespaces
- `-h` / `--help` — print usage to stdout and exit 0
- No flags — use kubeconfig context's namespace, falling back to "default" (original behavior)
- Unknown flags or missing values — print error to stderr and exit 2

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

### Updated `src/ui/views/status.rs`
- Added test `shows_all_namespaces_when_watching_all()` to verify status bar displays "all namespaces" correctly
- No signature changes; display string passed as namespace parameter

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

12 CLI parser tests:
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

1 Status bar test:
- `shows_all_namespaces_when_watching_all()` — renders "all namespaces" string correctly

**Test execution result**: 108 passing tests (104 lib, 4 bin), 2 integration tests ignored

## Files Changed

| File | Change |
|------|--------|
| `src/cli.rs` | NEW: CLI parser module with pure, testable `parse_args()` function |
| `src/lib.rs` | Added `pub mod cli;` |
| `src/main.rs` | Parse CLI args first; handle Help/Error/Run; resolve scope to namespace; pass display string to UI |
| `src/ui/views/status.rs` | Added test for "all namespaces" display |

## Self-Review Checklist

- ✅ No new dependencies (manual parsing of `std::env::args()`)
- ✅ No `unwrap()`/`expect()` outside tests
- ✅ Parser is pure and testable (takes iterator, no global state)
- ✅ Tests require no cluster, network, or TTY
- ✅ `cargo fmt --check` passes
- ✅ `cargo clippy --all-targets -- -D warnings` passes
- ✅ All 108 tests pass (104 + 4)
- ✅ Mutation testing confirms tests catch real behavior changes
- ✅ Terminal setup order correct: parse args → handle Help/Error → setup terminal → run loop
- ✅ Help and errors print before raw mode (no terminal corruption)
- ✅ Status bar displays "all namespaces" when appropriate
- ✅ Backward compatible: no flags uses original context behavior

## Concerns

**None identified.** Implementation is minimal, focused, and well-tested. The `-A` precedence rule over `-n` is documented and pinned by tests. Parser is completely decoupled from runtime (pure function with iterator input makes future testing trivial). All constraints satisfied: no dependencies, no unsafe unwraps, clean clippy/fmt, strong test coverage including mutation proof.
