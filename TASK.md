# Task: Four Targeted Fixes

Baseline: 594 tests. All 594 must still pass after each commit. No regressions.
Commit each fix separately. Each commit must pass `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.

---

## Fix 1: Delete src/plan/executor.rs (pure pass-through wrapper)

`src/plan/executor.rs` is a pure pass-through wrapper for `crate::agent::shell_execute::guarded_shell_execute_call` with zero independent logic. This violates the no-redundant-wrapper rule.

Changes:
- Update `src/plan/runner.rs`: replace the 2 call sites of `crate::plan::executor::guarded_shell_execute_call(...)` with `crate::agent::shell_execute::guarded_shell_execute_call(...)` directly
- Move the test from `src/plan/executor.rs` into `src/plan/runner.rs` (inline) or `src/agent/shell_execute.rs` so coverage is preserved
- Delete `src/plan/executor.rs`
- Remove `pub(crate) mod executor;` from `src/plan.rs` (or `src/plan/mod.rs` — check which file declares it)
- Commit: `refactor: delete plan/executor.rs pass-through wrapper`

---

## Fix 2: Turn constructor bypass in session_runtime

`src/session_runtime/drain.rs` constructs turns directly instead of going through `build_turn_for_config()` (the unified turn constructor in `src/turn/builders.rs`). This creates an asymmetric execution environment — sessions drained via the server path get a differently-constructed turn than sessions run via the CLI path.

Changes:
- Find where `drain.rs` constructs a `Turn` directly
- Replace with a call to `build_turn_for_config()` using the session's config, passing the appropriate tier/config
- Ensure the resulting turn is functionally equivalent (same guards, same tools, same subscriptions)
- Verify the existing queue drain tests still pass
- Commit: `refactor: route session_runtime drain through build_turn_for_config`

---

## Fix 3: Queue/drain deduplication

`src/agent/queue.rs` and `src/session_runtime/drain.rs` contain massively duplicated queue draining and SQLite bookkeeping logic. The two `match message.role.as_str()` blocks handle system/assistant/unknown roles identically; only the user-turn construction differs.

Changes:
- In `src/session_runtime/drain.rs`: extract a shared helper for the `"system"`, `"assistant"`, and unsupported-role branches that is shared between the two drain paths
- The only injection point should be how the user-turn is built (fixed turn vs fresh-turn-builder)
- Remove the duplicated code from `src/agent/queue.rs` — make it delegate to the shared helper
- Keep `pub async fn drain_queue(...)` signature stable (it's called from tests and server)
- Commit: `refactor: deduplicate queue/drain role-handling logic`

---

## Fix 4: Replace hardcoded mock secrets with fixtures

Hardcoded mock secrets like `"sk-proj-abcdefghijklmnopqrstuvwxyz012345"`, `"sk-test-key"`, `"test-key"` etc. are embedded in source files including `src/server/auth.rs`, `src/server/http.rs`, `src/server/queue.rs`, `src/gate/mod.rs`, `src/llm/openai/mod.rs`. This trains secret scanners to ignore these patterns.

Changes:
- Find all occurrences of `sk-` patterns, `"test-key"`, and similar mock secrets in non-test production paths
- Replace with clearly non-secret-shaped fixtures: e.g. `"mock-api-key"`, `"test-operator-key"`, `"dummy-token"`, `"example-key-abc123"` — anything that won't pattern-match real secret formats
- Update the pre-commit hook in `.git/hooks/pre-commit` to NOT flag `#[cfg(test)]` blocks for the `sk-` pattern (test data shouldn't be treated as secrets)
- Commit: `fix: replace sk-* mock secrets with non-secret-shaped fixtures`

---

## After all 4 commits

Run `cargo test` and confirm count is 594 (or higher if new tests were added).
Run `cargo build --release` to confirm clean build.

When completely finished, run: openclaw system event --text "Done: 4 fixes committed — executor deleted, turn constructor unified, queue deduped, mock secrets replaced" --mode now
