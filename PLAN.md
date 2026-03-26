# Session 5 Plan: store split + drain dedup

## 1. Files read

Read first:

- `CODE_STANDARD.md`

Config and architecture context:

- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`

Read every Rust source file under `src/`:

```text
src/agent/loop_impl.rs
src/agent/loop_impl/tests.rs
src/agent/mod.rs
src/agent/queue.rs
src/agent/queue/tests.rs
src/agent/shell_execute.rs
src/agent/spawn.rs
src/agent/spawn/tests.rs
src/agent/tests.rs
src/agent/tests/common.rs
src/agent/tests/regression_tests.rs
src/auth.rs
src/cli.rs
src/config/agents.rs
src/config/domains.rs
src/config/file_schema.rs
src/config/load.rs
src/config/mod.rs
src/config/models.rs
src/config/policy.rs
src/config/runtime.rs
src/config/spawn_runtime.rs
src/config/tests.rs
src/context.rs
src/delegation.rs
src/gate/budget.rs
src/gate/command_path_analysis.rs
src/gate/exfil_detector.rs
src/gate/mod.rs
src/gate/output_cap.rs
src/gate/protected_paths.rs
src/gate/secret_catalog.rs
src/gate/secret_redactor.rs
src/gate/shell_safety.rs
src/gate/streaming_redact.rs
src/identity.rs
src/lib.rs
src/llm/history_groups.rs
src/llm/mod.rs
src/llm/openai.rs
src/main.rs
src/model_selection.rs
src/plan.rs
src/plan/executor.rs
src/plan/notify.rs
src/plan/patch.rs
src/plan/recovery.rs
src/plan/runner.rs
src/principal.rs
src/read_tool.rs
src/server/auth.rs
src/server/http.rs
src/server/mod.rs
src/server/queue.rs
src/server/ws.rs
src/session/budget.rs
src/session/delegation_hint.rs
src/session/jsonl.rs
src/session/mod.rs
src/session/tests.rs
src/session/trimming.rs
src/skills.rs
src/spawn.rs
src/store.rs
src/subscription.rs
src/template.rs
src/time.rs
src/tool.rs
src/turn.rs
src/util.rs
```

## 2. Exact changes per file

Delete list:

- Delete `src/store.rs` after its contents are fully split into `src/store/`.
- Delete the duplicated queue-drain loop bodies from `src/agent/queue.rs` and `src/server/queue.rs`, leaving only delegation wrappers.
- Delete one of the duplicated denial formatter implementations so there is exactly one shared formatter.
- Delete the duplicated dynamic SQL builder bodies used by `update_plan_run_status()` and `update_plan_run_status_preserving_failed()`.

Files to add:

- `src/store/mod.rs`: final facade module created only in the last rename/delete sweep, after `src/store.rs` is removed as the temporary root. Keep `Store`, `QueuedMessage`, `SubscriptionRow`, `PlanRun`, `StepAttempt`, `StepAttemptRecord`, `NullableUpdate`, `PlanRunUpdateFields`, and `format_system_time` available from `crate::store::*`. Reexport submodule-owned helpers only as needed. Keep `Store { conn: Connection }` here so impl blocks in submodules can share private access.
- `src/store/migrations.rs`: own schema bootstrap and migration helpers now embedded in `Store::new()`. Move `ensure_messages_claimed_at_column`, `ensure_sessions_parent_session_id_column`, `ensure_plan_runs_table`, `ensure_plan_run_column`, `ensure_plan_step_attempt_column`, `cleanup_legacy_plan_rows`, `has_column`, and `has_table` test helper here. `Store::new()` in the facade should call one `initialize_schema()`/`run_migrations()` entrypoint.
- `src/store/sessions.rs`: move session CRUD and transaction boundary methods: `create_session`, `create_child_session`, `create_child_session_with_task`, `list_sessions`, `get_parent_session`, `get_session_metadata`, `list_child_sessions`, and `with_transaction`.
- `src/store/message_queue.rs`: move queue persistence methods: `enqueue_message`, `enqueue_message_in_transaction`, `dequeue_next_message`, `mark_processed`, `mark_failed`, `recover_stale_messages`, plus shared `unix_timestamp()`. Add a security/invariant comment above `dequeue_next_message()` explaining that the single `UPDATE ... RETURNING` statement is the queue claim atomicity boundary.
- `src/store/plan_runs.rs`: move `create_plan_run`, `get_plan_run`, both plan-run status update methods, plan-run claim/release/recovery methods, listing methods, `claim_pending_plan_run`, `plan_run_from_row`, and `validate_plan_run_status`. Introduce one typed internal update helper for both status-update entrypoints so null-vs-unchanged semantics are encoded once.
- `src/store/step_attempts.rs`: move step-attempt read/write/finalization methods, `step_attempt_from_row`, and attempt status validators. Keep transaction-aware crash recovery helper here because plan recovery depends on its atomic behavior.
- `src/store/subscriptions.rs`: move subscription CRUD, session/global dedup logic, timestamp refresh logic, and `format_system_time()`.
- `src/session_runtime/mod.rs`: facade for shared runtime helpers.
- `src/session_runtime/drain.rs`: single queue-drain state machine. This owns claim/process/mark/enqueue-completion flow and the `completed_agent_turn` / `first_denial` / `last_assistant_response` bookkeeping. Add a comment describing the state machine invariant: each claimed row exits as `processed` or `failed`, and completion messages are emitted only after at least one non-denied agent turn.
- `src/session_runtime/factory.rs`: shared subscription loading plus production turn/provider factory setup. Centralize `list_subscriptions_for_session()` materialization into `SubscriptionRecord`s, standard `build_turn_for_config_with_subscriptions()` setup, and OpenAI provider construction with `auth::get_valid_token()`. Do not move the generic injected-provider test seam here; this module is for reusable production helpers, not for replacing `spawn_and_drain_with_provider()`.
- `src/agent/denial.rs`: one shared `format_denial_message(reason, gate_id)` helper. Keep the existing `crate::agent::format_denial_message` path by reexporting from `src/agent/mod.rs`.

Files to change:

- `src/lib.rs`: export `pub mod session_runtime;`. If the denial helper lives under `src/agent/denial.rs`, no new top-level export is needed.
- `src/agent/mod.rs`: reexport the shared formatter from `denial.rs`. Keep the current public queue API intact by forwarding to the new shared drain implementation, and retarget `process_message()` plus `process_message_with_turn_builder()` to the relocated shared role-processing helpers so the current public entrypoints still compile and behave the same way.
- `src/agent/queue.rs`: reduce to thin wrappers around `session_runtime::drain`. Keep the current signatures for `drain_queue`, `drain_queue_with_stats`, and `drain_queue_with_stats_fresh_turns`, but remove the duplicated loop bodies and move message-role processing helpers into the shared drain module or a shared private helper used by it.
- `src/agent/loop_impl.rs`: remove the local denial formatter definition. Keep only verdict/audit logic that actually belongs to the agent loop.
- `src/cli.rs`: remove the duplicate denial formatter and call the shared formatter via `crate::agent::format_denial_message`. Keep approval/token presentation here only.
- `src/server/queue.rs`: keep server-specific session locking and `SessionLockLease`, but delegate the actual queue drain to `session_runtime::drain`. Replace inline subscription loading and standard provider setup in `spawn_http_queue_worker()` with `session_runtime::factory` helpers. Preserve the existing behavior that the store mutex is not held across provider execution.
- `src/server/ws.rs`: replace inline subscription loading and standard provider factory construction with `session_runtime::factory` helpers. Keep websocket token sink and approval handling local.
- `src/main.rs`: replace both inline CLI subscription-loading / turn-builder / provider-factory blocks with shared `session_runtime::factory` helpers. Keep CLI prompting, token sink, and approval handler local.
- `src/agent/spawn.rs`: replace inline subscription loading and the production non-T3 turn-builder/provider setup with `session_runtime::factory` helpers. Keep the spawned-T3 special case explicit so full skill instructions still flow only to T3 children. Preserve the existing generic test seam by keeping `spawn_and_drain_with_provider()` and `finish_spawned_child_drain()` provider-injected and auth-free; only the top-level production wrapper should depend on the OpenAI/auth factory helpers.
- `src/store` tests: split the current monolithic `src/store.rs` test module into submodule-local tests owned by the new files. Queue atomicity tests belong with `message_queue.rs`; plan-run update/claim tests with `plan_runs.rs`; step-attempt tests with `step_attempts.rs`; session CRUD/transaction tests with `sessions.rs`; subscription dedup/refresh tests with `subscriptions.rs`; migration/cleanup tests with `migrations.rs`.

Temporary staging to keep the repo green while refactoring:

- For the split itself, keep `src/store.rs` temporarily as the module root and point it at `src/store/*.rs` via `#[path = "store/..."]` submodules while code is being moved.
- Do not create `src/store/mod.rs` during the staged split. Create it only in the final sweep when `src/store.rs` is renamed/deleted, so there is never a turn where both files exist as competing roots for `mod store`.
- After all moved code compiles and tests pass through the facade, rename that root to `src/store/mod.rs` and delete `src/store.rs` in the final sweep.

## 3. What tests to write

Store split invariants:

- Rehome the existing queue atomicity test so `dequeue_next_message()` still proves exactly one concurrent claimant wins the oldest pending row.
- Rehome the stale-message recovery tests so only stale `processing` rows requeue and fresh claims stay `processing`.
- Add a direct test for the shared typed plan-run update helper covering every field mode: unchanged, set value, explicit null, empty-definition rejection, preserve-failed guard, and unchanged timestamps/claims outside requested fields.
- Keep the existing migration cleanup tests and make sure they still prove legacy rows are repaired or dropped before live use.

Drain dedup invariants:

- Move the current `agent/queue.rs` behavior tests to `session_runtime/drain.rs` or to wrappers that hit the shared implementation: unsupported roles are marked `processed`, agent-loop errors mark `failed`, denials do not stop later rows from processing, and a later success suppresses returning the earlier denial.
- Add an explicit regression for queued `assistant` rows so the shared role-processing path still appends assistant content to session history and marks the queue row `processed`.
- Add one shared test proving the state machine emits a parent completion message only after a non-denied agent turn and does not emit one for bookkeeping-only rows.
- Keep the server wrapper tests that are server-specific: same-session serialization, different-session parallelism, lock eviction, and the store mutex release during provider execution.
- Add one regression proving the fixed-turn path and the fresh-turn-builder path both hit the same shared drain bookkeeping and return the same `(verdict, processed_any, last_assistant_response)` semantics.
- Add a thin-wrapper regression for `agent::process_message()` and `agent::process_message_with_turn_builder()` so the public agent entrypoints are still wired to the relocated shared helpers after the queue refactor.

Factory/helper invariants:

- Add tests for `session_runtime::factory::load_subscriptions_for_session()` covering row-to-record conversion failure, session override beating global subscription for the same `(path, filter)`, and stable sort order by effective timestamp.
- Add tests for the standard turn factory proving subscriptions are still injected into T1 turns only, and that the refactor does not accidentally start injecting them into the existing T2 or standard T3 paths.
- Add a regression that the production spawn wrapper still uses shared factory helpers while the injected-provider spawn path remains mockable and does not require auth/OpenAI setup.
- Add a small formatter test at the new shared denial-helper location and remove duplicate formatter tests from callers.
- Add transport-specific failure-path regressions after factory extraction: CLI subscription/provider setup failure still propagates as an error return from the CLI path rather than degrading into a warning-and-continue flow, websocket setup failure still sends an error frame followed by `Done`, and the HTTP queue worker still logs-and-returns without draining on setup failure.

End-to-end verification target:

- `cargo test`
- `cargo clippy -- -D warnings`
- `cargo fmt --check`
- `xtask/lint.sh`

## 4. Order of operations

1. Create `src/session_runtime/{mod.rs,drain.rs,factory.rs}` and move the queue-drain bookkeeping into `drain.rs` behind a generic adapter that can work with both `&mut Store` and the server’s `Arc<Mutex<Store>>` path without holding the store lock across model execution.
2. Repoint `src/agent/queue.rs` to the shared drain first, because it already has direct `&mut Store` access and is the lowest-risk caller.
3. Repoint `src/server/queue.rs` next, preserving session locking and the existing server concurrency tests. This is the step that proves the adapter boundary is correct.
4. Centralize subscription loading and standard turn/provider construction in `src/session_runtime/factory.rs`, then update `src/main.rs`, `src/server/ws.rs`, `src/server/queue.rs`, and the non-T3 path in `src/agent/spawn.rs` to use it.
5. Introduce the single shared denial formatter and remove duplicate definitions from `src/cli.rs` and `src/agent/loop_impl.rs`.
6. Start the store split with `src/store.rs` still acting as the temporary root. Create only `src/store/{migrations.rs,sessions.rs,message_queue.rs,plan_runs.rs,step_attempts.rs,subscriptions.rs}` during this phase, never `src/store/mod.rs`. Move queue code first, then sessions, then subscriptions, then plan runs, then step attempts, then migrations/tests, compiling after each move.
7. Replace the duplicated plan-run dynamic SQL builders with one typed helper inside `src/store/plan_runs.rs`, then rerun the plan-run update tests before deleting the old copies.
8. When the store facade is stable and tests are green, rename the module root to `src/store/mod.rs` and delete `src/store.rs`.
9. Run the full verification matrix only after all moves are in place, because the delete/rename step is intentionally last.

## 5. Risk assessment

Highest risk:

- Queue-drain semantic drift. The current behavior around first denial retention, later-success suppression of the returned denial, and parent completion enqueue timing is subtle. Mitigation: move the existing queue tests with the logic and add one explicit shared state-machine test before deleting old loops.
- Store-lock lifetime regression on the server path. A naive shared drain could accidentally hold `state.store.lock().await` across `run_agent_loop()`. Mitigation: design the shared drain around short-lived claim/mark/enqueue callbacks and keep the existing `store_mutex_is_not_held_across_agent_turn` server test untouched.
- Store split root rename. Rust cannot finish with both `src/store.rs` and `src/store/mod.rs` as the same module root. Mitigation: stage the split with `src/store.rs` temporarily delegating into `src/store/*.rs`, then do the rename/delete as the final cleanup step.

Medium risk:

- Plan-run update helper refactor. The `NullableUpdate` semantics and the preserve-failed variant are easy to change by accident. Mitigation: make the shared helper typed, not stringly, and keep the existing update matrix tests plus one direct helper-focused regression.
- Spawned-child turn construction. T3 children intentionally bypass subscription summary loading and instead receive full skill instructions. Mitigation: keep the spawned-T3 branch explicit in `src/agent/spawn.rs` or expose a separate factory helper for it rather than forcing the standard turn path onto every tier.
- Test relocation churn. Splitting `src/store.rs` tests across submodules can hide failures if coverage gets dropped during the move. Mitigation: migrate tests with the code they prove, then compare old/new test names against the original file before deleting the monolith.

Success criteria at merge time:

- `src/store/` exists with the requested submodules and `src/store.rs` is gone.
- Exactly one queue-drain implementation remains and it lives in `src/session_runtime/drain.rs`.
- Exactly one denial formatter remains.
- `crate::store::*` call sites do not need path changes.
- Queue claim, processed/failed terminal states, stale-message recovery, and child-completion enqueue behavior all remain unchanged.
