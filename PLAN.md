# PLAN

## Files Read

### Task / config / docs

- `TASK.md`
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`

### `src/` tree

- `src/auth.rs`
- `src/delegation.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/logging.rs`
- `src/main.rs`
- `src/model_selection.rs`
- `src/plan.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/skills.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/terminal_ui.rs`
- `src/time.rs`
- `src/tool.rs`
- `src/agent/audit.rs`
- `src/agent/child_drain.rs`
- `src/agent/child_drain/tests.rs`
- `src/agent/loop_impl.rs`
- `src/agent/loop_impl/tests.rs`
- `src/agent/mod.rs`
- `src/agent/queue.rs`
- `src/agent/queue/tests.rs`
- `src/agent/shell_execute.rs`
- `src/agent/tests.rs`
- `src/agent/tests/common.rs`
- `src/agent/tests/regression_tests.rs`
- `src/agent/usage.rs`
- `src/app/args.rs`
- `src/app/mod.rs`
- `src/app/plan_commands.rs`
- `src/app/session_run.rs`
- `src/app/subscription_commands.rs`
- `src/app/tracing.rs`
- `src/child_session/completion.rs`
- `src/child_session/create.rs`
- `src/child_session/mod.rs`
- `src/config/agents.rs`
- `src/config/domains.rs`
- `src/config/file_schema.rs`
- `src/config/load.rs`
- `src/config/mod.rs`
- `src/config/models.rs`
- `src/config/policy.rs`
- `src/config/runtime.rs`
- `src/config/spawn_runtime.rs`
- `src/config/tests.rs`
- `src/context/history.rs`
- `src/context/identity_prompt.rs`
- `src/context/mod.rs`
- `src/context/skill_instructions.rs`
- `src/context/skill_summaries.rs`
- `src/context/subscriptions.rs`
- `src/context/tests.rs`
- `src/gate/budget.rs`
- `src/gate/command_path_analysis.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/protected_paths.rs`
- `src/gate/secret_catalog.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/gate/tests.rs`
- `src/llm/history_groups.rs`
- `src/llm/mod.rs`
- `src/llm/openai/mod.rs`
- `src/llm/openai/request.rs`
- `src/llm/openai/sse.rs`
- `src/observe/mod.rs`
- `src/observe/otel.rs`
- `src/observe/sqlite.rs`
- `src/plan/notify.rs`
- `src/plan/patch.rs`
- `src/plan/recovery.rs`
- `src/plan/runner.rs`
- `src/server/auth.rs`
- `src/server/http.rs`
- `src/server/mod.rs`
- `src/server/queue.rs`
- `src/server/queue_worker.rs`
- `src/server/session_lock.rs`
- `src/server/state.rs`
- `src/server/ws.rs`
- `src/session/budget.rs`
- `src/session/delegation_hint.rs`
- `src/session/jsonl.rs`
- `src/session/mod.rs`
- `src/session/tests.rs`
- `src/session/trimming.rs`
- `src/session_runtime/drain.rs`
- `src/session_runtime/factory.rs`
- `src/session_runtime/mod.rs`
- `src/store/message_queue.rs`
- `src/store/migrations.rs`
- `src/store/mod.rs`
- `src/store/plan_runs.rs`
- `src/store/sessions.rs`
- `src/store/step_attempts.rs`
- `src/store/subscriptions.rs`
- `src/turn/builders.rs`
- `src/turn/mod.rs`
- `src/turn/tests.rs`
- `src/turn/tiers.rs`
- `src/turn/verdicts.rs`

## Baseline / Current-State Notes

- Ran `tokei src Cargo.toml agents.toml --output json`.
- Ran `jscpd src --reporters json --silent`.
- Current baseline from `tokei`:
  - `src/`: 36,604 Rust code lines
  - `Cargo.toml` + `agents.toml`: 67 TOML code lines
- `jscpd` confirms the named hotspots are real in the current tree, especially:
  - `src/session_runtime/drain.rs`: 378 duplicated lines
  - `src/store/step_attempts.rs`: 127 duplicated lines
  - `src/store/plan_runs.rs`: 30 duplicated lines
  - `src/plan/notify.rs`: 75 duplicated lines
  - `src/plan/patch.rs`: 163 duplicated lines
  - `src/plan/recovery.rs`: 234 duplicated lines
  - `src/server/auth.rs`: 100 duplicated lines
  - `src/server/http.rs`: 147 duplicated lines
- Task/spec drift already present on 2026-03-27:
  - Optimization 1 is already satisfied: `src/plan/executor.rs` does not exist.
  - Optimization 3 is already effectively satisfied: `src/store/mod.rs` only holds `Store`, `new()`, `with_transaction()`, shared types/helpers, and tests; the store submodules already own their `impl Store` blocks.
  - Optimization 7 is already effectively satisfied: `src/gate/command_path_analysis.rs` already routes read analysis through `PathReference`, `command_argument_references_path(...)`, and `simple_command_reads_path(...)`.
- Because of that drift, commits 1, 3, and 7 are verification/no-op checkpoints unless you want literal empty commits.

## Exact Changes Per File

### Optimization 1

- No file change planned.
- Verification only: confirm `src/plan/executor.rs` is absent and skip the commit, exactly as the task text allows.

### Optimization 2

- `src/session_runtime/drain.rs`
  - Add one internal helper that owns the role dispatch for queued messages.
  - The helper will:
    - append `system` messages with the correct principal
    - append `assistant` text-only messages with the correct principal
    - return `QueueOutcome::UnsupportedRole(...)` for every other non-user role
    - call a supplied closure only for `"user"` messages
  - Replace the duplicated non-user/user split currently open-coded in:
    - `process_queued_message_with_observer(...)`
    - `process_queued_message_with_turn_builder_observed(...)`
  - Keep the queue state machine untouched: row claiming, `processed`/`failed` marking, denial handling, `completed_agent_turn`, and child-completion enqueue rules stay byte-for-byte equivalent in behavior.
- `src/agent/queue.rs`
  - Reduce the file to the thinnest possible store-backed adapter over `src/session_runtime/drain.rs`.
  - Keep `pub async fn drain_queue(...)` exactly as-is at the signature level.
  - Collapse wrapper duplication so observer creation / forwarding logic is not repeated more than necessary.
- `src/agent/mod.rs`
  - If `src/agent/queue.rs` sheds private wrappers, retarget internal calls from `process_message(...)`, `process_message_with_turn_builder(...)`, and `drain_queue_with_stats_fresh_turns(...)` without changing any public signatures.

### Optimization 3

- `src/store/mod.rs`
  - No production edit planned.
  - Re-verify that `Store` and `Store::new(...)` remain in `mod.rs`, and that the public methods already live in:
    - `src/store/sessions.rs`
    - `src/store/message_queue.rs`
    - `src/store/plan_runs.rs`
    - `src/store/step_attempts.rs`
    - `src/store/subscriptions.rs`
  - If literal “8 commits” is mandatory, make this an empty verification commit; otherwise skip.

### Optimization 4

- `src/store/plan_runs.rs`
  - Extract shared validation from `build_plan_run_status_update_sql(...)` and `create_plan_run(...)`:
    - plan-run status validation
    - positive `revision`
    - non-negative `current_step_index`
    - non-empty `definition_json`
  - Replace the two repeated `NullableUpdate` matches with one small helper that appends `SET column = ?N` and pushes the matching `PlanRunUpdateValue::{Text, Null}` binding.
  - Add one local helper for repeated `super::format_system_time(SystemTime::now())`.
  - Add one shared collector for `SELECT {PLAN_RUN_COLUMNS_SQL} ...` queries so `list_plan_runs_by_session(...)`, `list_recent_plan_runs(...)`, `list_recent_active_plan_runs(...)`, and `list_stale_running_plan_runs(...)` stop repeating the same prepare/query/collect shape.
  - Collapse the duplicated claim path so `claim_next_pending_plan_run(...)` and `claim_next_runnable_plan_run(...)` share one execution helper and differ only in transaction boundary / error-context strings.
  - Route the timestamp-only update sites through the same local helper without changing their behavior:
    - `release_plan_run_claim(...)`
    - `recover_stale_plan_runs(...)`
    - `resume_waiting_plan_run(...)`
    - `cancel_plan_run(...)`
  - Keep all SQL predicates, optimistic-lock behavior, `status != 'failed'` guard behavior, and public `Store` method signatures unchanged.
- `src/store/mod.rs`
  - Only adjust imports/tests if helper visibility requires it; no API shape change.

### Optimization 5

- `src/store/step_attempts.rs`
  - Extract one shared validator for terminal step-attempt updates:
    - terminal status is one of `passed` / `failed` / `crashed`
    - `finished_at` is non-empty
    - optional payload fields (`summary_json`, `checks_json`) are non-empty when the caller requires them
  - Reuse one shared “running row update” path for:
    - `update_step_attempt_status(...)`
    - `update_step_attempt_child_session(...)`
    - `finalize_step_attempt(...)`
  - Factor the repeated step-attempt column list into one SQL constant/helper so `crash_running_step_attempts_for_run_in_transaction(...)` and `get_step_attempts(...)` do not repeat the full `SELECT id, plan_run_id, ... finished_at` list.
  - Keep transaction boundaries identical for `crash_running_step_attempts_for_run(...)` and `finalize_stale_step_attempts(...)`.
  - Preserve current error messages and zero-row failure behavior so `plan::runner` and existing store tests do not observe any change.

### Optimization 6

- `src/gate/command_path_analysis.rs`
  - Replace the current write-side “normalize to `identity-templates/...` and then reuse identity-specific helpers” approach with a real shared target-aware pipeline.
  - Introduce a small write-target matcher/context that can answer:
    - does this token mention the protected target?
    - does this direct command destination resolve into the protected target?
    - does this redirection / inline script / open-mode invocation target the protected path?
  - Convert the current `identity_template_*` helper chain to matcher-accepting helpers instead of hard-coded identity-template helpers.
  - Delete the rewrite-only bridge helpers if the generic matcher subsumes them:
    - `rewrite_target_path_mentions(...)`
    - `collect_target_path_replacements(...)`
    - `collect_target_path_replacements_into(...)`
    - `rewrite_target_path_argument(...)`
  - Keep the outward entry points stable:
    - `command_writes_identity_template_path(...)`
    - `command_writes_target_path(...)`
  - Preserve the current semantics for:
    - direct file mutation commands
    - shell wrappers (`bash -c`, `env -S`, busybox `sh`, etc.)
    - redirections
    - Python / Perl / Ruby / Node inline write APIs
    - git restore/checkout
    - symlink-target resolution
    - non-existent child paths created under a target whose parent resolves through an alias/canonical path

### Optimization 7

- `src/gate/command_path_analysis.rs`
  - No production edit planned unless a second pass finds leftover paired protected/target read helpers that are not already routed through `PathReference`.
  - Current code already has the shared matcher shape the task asks for:
    - `PathReference`
    - `command_argument_references_path(...)`
    - `grep_file_operands_refer_path(...)`
    - `git_option_value_references_path(...)`
    - `git_config_value_references_path(...)`
    - `simple_command_reads_path(...)`
  - Treat this as a verification/no-op step to avoid churning security-sensitive code for no LOC win.

### Optimization 8

- `src/test_support.rs` (new, `#[cfg(test)]`)
  - Add one crate-local test-only support module.
  - Provide:
    - `new_test_store(prefix: &str) -> (crate::store::Store, std::path::PathBuf)`
    - `new_test_server_state(prefix: &str) -> (crate::server::ServerState, std::path::PathBuf)`
  - Centralize the repeated temp-root creation, `queue.sqlite` path setup, `sessions/` directory setup, default `Config`, API keys, operator key, and shared `reqwest::Client`.
- `src/lib.rs`
  - Register the new support module behind `#[cfg(test)]`.
- `src/plan/notify.rs`
  - Remove the local `test_store()` helper.
  - Import and use `crate::test_support::new_test_store(...)`.
  - Keep `test_plan_run(...)` local because it is behavior-specific.
- `src/plan/patch.rs`
  - Remove the local `test_store(prefix)` helper.
  - Switch each test to the shared helper with the existing per-test prefix strings.
- `src/plan/recovery.rs`
  - Remove the local `test_store()` helper.
  - Switch tests to the shared helper.
- `src/server/auth.rs`
  - Remove the local `test_state()` helper.
  - Switch tests to the shared server-state helper and discard the queue path where it is unused.
- `src/server/http.rs`
  - Remove the local `test_state()` helper.
  - Switch tests to the shared server-state helper.
- `src/server/queue.rs`
  - Remove the local `test_state()` helper.
  - Switch queue tests to the shared server-state helper so the server-side fixture has one source of truth.
- `src/store/step_attempts.rs`
  - Remove the local `test_store()` helper from the module test block.
  - Switch the local test(s) to the shared store helper so the temp-store fixture has one source of truth.
- Keep this commit test-only.

## Tests To Write

- Optimization 1
  - No new tests. Verification only that the file is already absent.
- Optimization 2
  - Add one focused regression around the new shared queue-message dispatcher:
    - both fixed-turn and fresh-turn-builder paths must treat `system`, `assistant`, and unsupported roles identically
    - provider factory must not be called for non-user rows
    - unsupported roles must still become `processed`, not `failed`
    - bookkeeping-only drains must still avoid child completion messages
  - Re-run existing queue tests in:
    - `src/agent/queue/tests.rs`
    - `src/server/queue.rs` test module
- Optimization 3
  - No new tests if this remains a no-op verification step.
- Optimization 4
  - Add helper-level tests in `src/store/plan_runs.rs` or extend store tests to assert:
    - `NullableUpdate::Unchanged`, `Null`, and `Value` still produce the same updates
    - empty `definition_json` still fails before SQL runs
    - optimistic-lock behavior in `update_plan_run_status_preserving_failed(...)` is unchanged
    - claim ordering and stale-claim recovery remain unchanged
    - `resume_waiting_plan_run(...)` still only revives `waiting_t2` rows and clears `claimed_at` / `last_failure_json`
    - `cancel_plan_run(...)` still only cancels `pending` / `running` / `waiting_t2` rows and leaves terminal rows untouched
- Optimization 5
  - Add targeted tests asserting:
    - `update_step_attempt_child_session(...)` still only updates running unfinished attempts
    - `finalize_step_attempt(...)` preserves the current “already finalized” failure mode
    - `finalize_stale_step_attempts(...)` still turns only running unfinished attempts into `crashed` and keeps summary/check payloads intact
    - negative/empty validation failures remain identical
- Optimization 6
  - Add paired write-detection regressions that exercise both public entry points:
    - direct write commands
    - `env -S` / `bash -c` wrappers
    - redirection
    - Python `open(..., 'w'/'a'/'x')`
    - Perl `-i`, `-pi`, `unlink`, `rename`
    - Node `writeFileSync` / `appendFileSync`
    - git restore/checkout
    - symlinked custom-target paths
    - a new file created under a target whose leaf does not yet exist but whose parent resolves through a symlink/canonical alias
  - Every new generic helper must be covered by at least one `identity-templates` case and one custom-target case.
- Optimization 7
  - No new tests unless this step unexpectedly requires code movement.
- Optimization 8
  - No new behavior tests required.
  - The fixture-migration pass must cover every duplicated store/server fixture moved in this step:
    - `src/plan/notify.rs`
    - `src/plan/patch.rs`
    - `src/plan/recovery.rs`
    - `src/server/auth.rs`
    - `src/server/http.rs`
    - `src/server/queue.rs`
    - `src/store/step_attempts.rs`
  - Optional one-line smoke tests only if the shared test-support module introduces logic not already exercised by the migrated tests.
- After each real commit
  - `cargo fmt --check`
  - `cargo clippy -- -D warnings`
  - `cargo test`
- Final pass
  - rerun `tokei`
  - rerun `jscpd`
  - confirm test count is still at least 624 before any final delivery

## Order Of Operations

1. Capture and save the current baseline (`tokei`, `jscpd`, current test count) and explicitly record that optimizations 1, 3, and 7 are already in the requested end-state.
2. Commit 1: verification/no-op for missing `src/plan/executor.rs` (skip if empty commits are not required).
3. Commit 2: queue/drain consolidation in `src/session_runtime/drain.rs`, `src/agent/queue.rs`, and only the minimum internal call-site adjustments needed in `src/agent/mod.rs`.
4. Run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
5. Commit 3: verification/no-op for store forwarding removal already being complete (skip if empty commits are not required).
6. Commit 4: `src/store/plan_runs.rs` dedup only.
7. Run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
8. Commit 5: `src/store/step_attempts.rs` dedup only.
9. Run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
10. Commit 6: write-side `command_path_analysis.rs` refactor only.
11. Run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
12. Commit 7: verification/no-op for read-side matcher already being shared (skip if empty commits are not required).
13. Commit 8: shared test fixture extraction (`src/test_support.rs`, `src/lib.rs`, `src/plan/notify.rs`, `src/plan/patch.rs`, `src/plan/recovery.rs`, `src/server/auth.rs`, `src/server/http.rs`, `src/server/queue.rs`, `src/store/step_attempts.rs`).
14. Run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
15. Final full verification:
  - rerun `tokei`
  - rerun `jscpd`
  - if/when the full implementation task resumes, run `cargo build --release` too, since `AGENTS.md` requires it even though the per-commit task text only names fmt/clippy/test
16. Only after all eight optimization steps are complete and verified, run:
  - `openclaw system event --text "Done: LOC reduction — 8 optimizations committed" --mode now`
17. If literal no-op commits are created for steps 1, 3, or 7, run the same `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` trio after those commits too so the per-commit validation rule is unambiguous.

## Risk Assessment

- Highest risk: `src/gate/command_path_analysis.rs`
  - This is security-sensitive heuristic code.
  - The main failure mode is a false negative that weakens write-path denial.
  - Mitigation: parameterize carefully, keep public entry points stable, and add paired identity/custom-target regressions before removing the rewrite bridge.
- Medium risk: `src/session_runtime/drain.rs`
  - Queue-row lifecycle is safety-critical.
  - The invariant from `AGENTS.md` and the code comment must hold: every claimed row ends `processed` or `failed`, and child completion is emitted only after a non-denied agent turn.
  - Mitigation: do not change the state machine, only refactor dispatch.
- Medium risk: `src/store/plan_runs.rs` and `src/store/step_attempts.rs`
  - `src/plan/runner.rs`, `src/plan/notify.rs`, and `src/plan/recovery.rs` rely on current transaction boundaries, update guards, and error strings.
  - Mitigation: preserve SQL predicates and existing failure messages; extend store-level tests around optimistic locking and finalized-row rejection.
- Low risk: optimization 8
  - Test-only helper extraction should not affect production behavior.
  - Mitigation: keep the new module behind `#[cfg(test)]` and avoid changing any assertions.
- Process risk: task/spec drift
  - Optimizations 1, 3, and 7 appear already complete in the checked-in code.
  - If you require literal “8 commits, one per optimization”, three commits will be empty bookkeeping/verification commits unless we intentionally churn already-correct code, which would be the wrong engineering tradeoff.
