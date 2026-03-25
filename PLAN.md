# PE-1 Plan

## 1. Files Read

Docs and config:
- `docs/specs/plan-engine.md`
- `docs/risks.md`
- `docs/architecture/overview.md`
- `agents.toml`
- `Cargo.toml`

All source under `src/`:
- `src/agent/loop_impl.rs`, `src/agent/loop_impl/tests.rs`, `src/agent/mod.rs`, `src/agent/queue.rs`, `src/agent/queue/tests.rs`, `src/agent/spawn.rs`, `src/agent/spawn/tests.rs`, `src/agent/tests.rs`, `src/agent/tests/common.rs`, `src/agent/tests/regression_tests.rs`
- `src/auth.rs`, `src/cli.rs`, `src/config.rs`, `src/context.rs`, `src/delegation.rs`, `src/identity.rs`, `src/lib.rs`, `src/main.rs`, `src/model_selection.rs`, `src/principal.rs`, `src/read_tool.rs`, `src/session.rs`, `src/skills.rs`, `src/spawn.rs`, `src/store.rs`, `src/subscription.rs`, `src/template.rs`, `src/tool.rs`, `src/turn.rs`, `src/util.rs`
- `src/gate/budget.rs`, `src/gate/exfil_detector.rs`, `src/gate/mod.rs`, `src/gate/output_cap.rs`, `src/gate/secret_patterns.rs`, `src/gate/secret_redactor.rs`, `src/gate/shell_safety.rs`, `src/gate/streaming_redact.rs`
- `src/llm/mod.rs`, `src/llm/openai.rs`
- `src/server/auth.rs`, `src/server/http.rs`, `src/server/mod.rs`, `src/server/queue.rs`, `src/server/ws.rs`

## 2. Exact Changes Per File

### `src/store.rs`

- Add two new row structs beside `QueuedMessage` and `SubscriptionRow`:
  - `PlanRun` with every persisted column from `plan_runs`, including nullable fields and `claimed_at`.
  - `StepAttempt` with every persisted column from `plan_step_attempts`.
- Add a small update payload type for `update_plan_run_status(...)`.
  - It should support optional updates to `revision`, `current_step_index`, and `definition_json`.
  - It should use tri-state handling for nullable columns such as `active_child_session_id` and `last_failure_json` so callers can distinguish `leave unchanged` from `set NULL`.
- Extend `Store::new()` schema init with:
  - `CREATE TABLE IF NOT EXISTS plan_runs (...)`
  - `CREATE TABLE IF NOT EXISTS plan_step_attempts (...)`
  - supporting indexes for the planned query shapes:
    - `plan_runs(owner_session_id, created_at, id)` for per-session listing
    - `plan_runs(status, claimed_at, id)` for lease scans
    - `plan_step_attempts(plan_run_id, step_index, attempt, id)` for history lookup
- Add `ensure_plan_runs_table(&Connection) -> Result<()>`.
  - Idempotent.
  - Called from `Store::new()` after the existing schema batch and existing migration helpers.
  - Responsible for creating both plan tables and their indexes when opening an older DB.
  - Likely needs a small `has_table(...)` helper in the same file, parallel to `has_column(...)`.
- Add private row-mapper helpers so query code does not duplicate `row.get(...)` chains for `PlanRun` and `StepAttempt`.
- Add private status validation helpers for the fixed status sets:
  - `plan_runs.status`: `pending | running | waiting_t2 | completed | failed`
  - `plan_step_attempts.status`: `running | passed | failed | crashed`
- Define the `plan_runs` lease lifecycle explicitly so the storage API has no ambiguous states:
  - `claim_next_pending_plan_run(...)` only claims rows currently in `status = 'pending'`.
  - A claimed row becomes `status = 'running'` with non-null `claimed_at`.
  - `release_plan_run_claim(...)` is only valid after the caller has already moved the row out of `running`; if the row is still `running`, the method returns an error instead of creating `running + NULL claimed_at`.
  - `recover_stale_plan_runs(...)` handles stale `running` rows by moving them to `waiting_t2`, clearing `claimed_at`, and bumping `updated_at`.
  - `recover_stale_plan_runs(...)` also clears leaked stale claims on `waiting_t2`, `completed`, and `failed` rows without changing their status.
  - Stale claimed `pending` rows are intentionally not touched by `recover_stale_plan_runs(...)`; they are recovered lazily by `claim_next_pending_plan_run(...)`, which already treats stale `pending` claims as claimable.
- Add `Store` methods for `plan_runs`:
  - `create_plan_run(...)`
    - insert row with `revision = 1`, `current_step_index = 0`, `status = 'pending'`
    - set `created_at` and `updated_at` from `utc_timestamp()`
  - `get_plan_run(...)`
    - fetch one row or `None`
  - `update_plan_run_status(...)`
    - single `UPDATE` that always writes `status` and `updated_at`
    - only mutates columns explicitly present in the update payload
  - `claim_next_pending_plan_run(stale_after_secs)`
    - same atomic lease style as `dequeue_next_message()`: one `UPDATE ... RETURNING`
    - claimable rows are `status = 'pending'` and either unclaimed or stale (`claimed_at <= now - stale_after_secs`)
    - ordered by `created_at ASC, id ASC`
    - sets `status = 'running'`, `claimed_at = unix_timestamp()`, and bumps `updated_at`
  - `release_plan_run_claim(...)`
    - clears `claimed_at` and bumps `updated_at`
    - rejects rows still in `status = 'running'`
  - `list_plan_runs_by_session(...)`
    - deterministic order by `created_at ASC, id ASC`
  - `recover_stale_plan_runs(stale_after_secs)`
    - storage-only lease recovery helper
    - stale `running` rows become `waiting_t2` with `claimed_at = NULL`
    - stale `waiting_t2`, `completed`, and `failed` rows only have the leaked claim cleared
    - stale `pending` rows are left alone because the claim helper already reclaims them
    - always bumps `updated_at` on touched rows
    - no attempt-row mutation, notification, enqueue, or plan-execution behavior in this slice
- Add `Store` methods for `plan_step_attempts`:
  - `record_step_attempt(...)`
    - start-of-attempt insert only
    - accepts `status = 'running'` only
    - inserts `started_at = utc_timestamp()` and `finished_at = NULL`
    - return `last_insert_rowid()`
  - `update_step_attempt_status(...)`
    - terminal transition only
    - accepts `status = 'passed' | 'failed' | 'crashed'`
    - updates `status` and `finished_at`
  - `get_step_attempts(...)`
    - fetch attempts for one `(plan_run_id, step_index)`
    - deterministic order by `attempt ASC, id ASC`
- Keep `definition_json`, `last_failure_json`, `summary_json`, and `checks_json` opaque `TEXT`.
  - No serde parsing in the store layer for PE-1.
- Extend the existing `#[cfg(test)] mod tests` at the bottom of the file.
  - No new integration tests.
  - No changes to `main.rs`, `server/mod.rs`, `server/queue.rs`, `turn.rs`, or plan execution code in this task.

### `PLAN.md`

- Add this plan document only.

## 3. Tests To Write

- `store_new_creates_plan_tables`
  - Assert `plan_runs` and `plan_step_attempts` exist after `Store::new()`.
  - Assert required indexes exist.
- `ensure_plan_runs_table_migrates_legacy_store`
  - Create a legacy DB with only `sessions` and `messages`.
  - Reopen through `Store::new()`.
  - Assert both plan tables now exist and the helper is idempotent when called again.
- `create_and_get_plan_run_round_trips`
  - Insert a parent session first.
  - Call `create_plan_run(...)`.
  - Assert defaults: `status = pending`, `revision = 1`, `current_step_index = 0`, `claimed_at = NULL`.
  - Assert `created_at` and `updated_at` are populated, and `definition_json/topic/trigger_source` round-trip.
- `create_plan_run_rejects_missing_owner_session`
  - Do not create the owner session first.
  - Assert the FK-backed insert fails with store context.
- `get_plan_run_returns_none_for_missing_id`
  - Assert the missing-row path is covered directly.
- `list_plan_runs_by_session_returns_only_that_owner_in_creation_order`
  - Insert runs for two sessions.
  - Assert filtering and stable ordering.
- `update_plan_run_status_updates_only_requested_fields`
  - Update status plus selected mutable fields.
  - Assert untouched fields remain unchanged.
  - Assert nullable fields can be explicitly cleared.
  - Assert `updated_at` changes while `created_at` does not.
- `claim_next_pending_plan_run_is_atomic_across_workers`
  - Same pattern as the existing message-claim concurrency test.
  - Two `Store` instances race on one DB.
  - Assert only one worker claims the same run.
- `claim_next_pending_plan_run_marks_row_running_and_claimed`
  - Assert claimed row now has `status = running` and non-null `claimed_at`.
- `claim_next_pending_plan_run_skips_fresh_running_rows`
  - Seed a non-stale `running` row with a recent `claimed_at`.
  - Assert the claim helper does not return it.
- `claim_next_pending_plan_run_returns_none_when_only_non_pending_rows_exist`
  - Seed `waiting_t2`, `completed`, and `failed` rows.
  - Assert the method returns `None`.
- `release_plan_run_claim_clears_claim_without_losing_state`
  - Move a claimed row from `running` to a non-running status first, then release it.
  - Assert `claimed_at = NULL`, `updated_at` advances, and non-lease fields stay intact.
- `release_plan_run_claim_rejects_running_rows`
  - Claim a run and try to release before changing status.
  - Assert the method returns an error instead of creating `running + NULL claimed_at`.
- `recover_stale_plan_runs_respects_age_threshold`
  - Make one stale claimed run and one fresh claimed run.
  - Assert only stale rows are recovered.
- `recover_stale_plan_runs_moves_stale_running_rows_to_waiting_t2`
  - Seed a stale `running` row.
  - Assert recovery clears `claimed_at`, changes status to `waiting_t2`, and advances `updated_at`.
- `recover_stale_plan_runs_clears_stale_claims_on_non_running_rows_without_status_change`
  - Seed stale claimed rows already in `waiting_t2`, `completed`, or `failed`.
  - Assert recovery clears the claim but preserves status.
- `recover_stale_plan_runs_leaves_stale_pending_rows_for_claim_path`
  - Seed a stale claimed `pending` row.
  - Assert recovery does not touch it.
  - Assert `claim_next_pending_plan_run(...)` can still reclaim it.
- `recover_stale_plan_runs_advances_updated_at_on_non_running_claim_cleanup`
  - Seed a stale claimed row already in `waiting_t2`, `completed`, or `failed`.
  - Assert the cleanup path also bumps `updated_at`, not just the `running -> waiting_t2` path.
- `record_and_get_step_attempts_round_trip`
  - Insert a plan run, record multiple attempts for the same step.
  - Assert `summary_json`, `checks_json`, `child_session_id`, and ordering by `attempt`.
- `record_step_attempt_rejects_missing_plan_run`
  - Assert the FK-backed insert fails when the parent `plan_run` is absent.
- `record_step_attempt_rejects_terminal_status_on_insert`
  - Assert insert only accepts `running`.
- `record_step_attempt_rejects_unknown_status_on_insert`
  - Assert insert also rejects arbitrary unknown status strings, not just terminal ones.
- `update_step_attempt_status_sets_finished_at`
  - Insert an attempt with `finished_at = NULL`.
  - Update to `passed` or `failed`.
  - Assert the new status and stored finish timestamp.
- `update_step_attempt_status_rejects_running`
  - Insert a `running` attempt.
  - Assert the update helper rejects `running` as a non-terminal transition target.
- `get_step_attempts_returns_empty_for_missing_step`
  - Assert the empty-list path is covered directly.
- `step_attempts_cascade_when_plan_run_is_deleted`
  - Delete the parent `plan_run` directly via `store.conn` inside the unit test.
  - Assert its `plan_step_attempts` rows disappear due to `ON DELETE CASCADE`.
- `invalid_plan_run_status_rejects_before_sql`
  - Assert `update_plan_run_status(...)` fails fast on an unknown status.
- `invalid_step_attempt_status_rejects_before_sql`
  - Assert `update_step_attempt_status(...)` fails fast on an unknown terminal status.

## 4. Order Of Operations

1. Add the new structs, helper types, and schema/index creation in `src/store.rs`, plus the fresh-schema and legacy-migration tests in the same slice so the file compiles and the new tests pass immediately.
2. Add `plan_runs` CRUD methods together with the missing-row and missing-owner tests, and keep the whole test suite green before moving on.
3. Add the lease helpers with the explicit lifecycle contract above: pending claim, no releasing `running`, stale `running -> waiting_t2`, and leaked non-running claims cleared. Add the concurrency, `None`, and timestamp assertions in the same slice.
4. Add `plan_step_attempts` with the strict `running`-on-insert and terminal-on-update contract, plus FK, empty-list, and cascade tests in the same slice.
5. Run `cargo fmt`, `cargo test`, and `cargo clippy -- -D warnings`.

## 5. Risk Assessment

- The sharp edge is stale-run recovery semantics. This plan now fixes the state transition to `running -> waiting_t2` instead of blind replay, but PE-1 still does not record a `crashed` attempt row or enqueue a notification. Later plan-engine slices must add those before any runtime wiring treats recovery as complete.
- `update_plan_run_status(...)` is easy to get wrong if nullable fields use plain `Option<T>`. A tri-state update representation is important so `None` does not accidentally mean both `clear this column` and `leave this column alone`.
- Lease helpers must stay atomic. `claim_next_pending_plan_run(...)` should follow the existing `messages` pattern and avoid a separate `SELECT` then `UPDATE`, or concurrency bugs will reappear immediately.
- JSON columns should remain opaque storage in PE-1. Pulling serde parsing into `store.rs` would make this slice larger, couple storage to plan execution too early, and complicate future migration work.
