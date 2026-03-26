# PE-6 Plan

## 1. Files Read

- All files under `src/`
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/specs/plan-engine.md`
- `docs/architecture/overview.md`
- `docs/roadmap.md`

Key files reviewed closely for this task:

- `src/plan/runner.rs`
- `src/plan/notify.rs`
- `src/store.rs`

## 2. Exact Changes Per File

### `src/plan.rs`

- Export the new `recovery` module in the same change that adds `src/plan/recovery.rs` so the crate stays compiling the entire time.
- Keep the public plan module surface small: `runner`, `notify`, and `recovery` remain siblings, with no duplicated recovery logic spread across unrelated modules.

### `src/plan/notify.rs`

- Keep `notify_plan_failure(...)` as the canonical crash-notification path, but factor its SQLite write logic so it can run inside a caller-owned transaction.
- Add a small internal helper that both:
  - the existing `notify_plan_failure(...)` wrapper can use in the normal path; and
  - recovery/runner can use when they need `waiting_t2` state change plus notification enqueue to commit atomically.
- Do not change the user-visible notification semantics; this is strictly to avoid partial state where the run is `waiting_t2` but no notification was queued.

### `src/plan/recovery.rs` (new)

- Add `recover_crashed_plans(store, stale_after_secs: u64) -> Result<u64>`.
- Implement startup recovery as an attempt-aware handoff, not as a blind status reset:
  - fetch stale `plan_runs` with `status='running'` and `claimed_at` older than the threshold;
  - for each stale run, call a store helper that finds every `plan_step_attempts` row for that run with `status='running'`, marks it `crashed`, stamps `finished_at`, and returns the original rows with their stored metadata;
  - derive `PlanFailureDetails`;
  - atomically move the `plan_run` to `waiting_t2` and enqueue the crash notification to the owner T2 session via the shared `notify_plan_failure(...)` write path.
- Notification payload selection:
  - if one or more crashed running attempts were found, use the newest crashed attempt as the source for `PlanFailureDetails` and keep its stored `checks`;
  - if no running attempt rows exist, derive failure details from the run itself:
    - primary path: derive `step_id` from `definition_json` + `current_step_index`, and derive `attempt` as `max(existing attempt for that step in the same run/revision) + 1`, or `0` if none exist;
    - corruption fallback: if `definition_json` is malformed or `current_step_index` does not resolve to a step, synthesize `step_id="__unknown_step__"` and derive `attempt` as `max(attempt across the run) + 1`, or `0` if none exist;
    - use empty `checks`.
- Partial-failure semantics are explicit and internally consistent:
  - recover runs sequentially;
  - increment the returned counter only after the per-run atomic `waiting_t2 + notification` commit succeeds;
  - if any run fails before that commit, return `Err` immediately;
  - earlier successful runs remain recovered;
  - the failing run remains `status='running'` if the atomic transition/notification step fails, so a retry can safely pick it up again.
- Expose a small `pub(crate)` helper for the shared crash-to-`waiting_t2` handoff so `runner.rs` can fail closed without duplicating the atomic transition logic.

### `src/plan/runner.rs`

- Remove the current blind replay behavior where stale/preexisting running attempts are finalized and the step is executed anyway.
- Tighten `run_plan_step(...)` so it fails closed:
  - if it detects preexisting `running` step attempts for the current plan run, it must route through the shared crash handoff helper, atomically move the run to `waiting_t2`, notify T2, and return `StepOutcome::WaitingT2` without executing the step body;
  - do not reuse the old “finalize then continue” path.
- Keep this guard even after startup recovery is added so a startup ordering regression cannot silently replay a crashed step.
- Make terminal state monotonic for CLI cancel:
  - if the run has already been marked `failed` by `plan cancel`, runner-side state transitions must not overwrite that terminal status on later writes;
  - treat a failed/cancelled run as a benign terminal no-op when encountered after local state was already loaded.
- Update existing runner tests that currently expect stale attempts to be resumed; the new invariant is “handoff to T2, never blind replay.”

### `src/store.rs`

- Add or refactor store helpers so recovery is fully supported without abusing `finalize_step_attempt(...)`:
  - `list_stale_running_plan_runs(stale_after_secs: u64) -> Result<Vec<PlanRun>>`
  - `crash_running_step_attempts_for_run(plan_run_id: &str) -> Result<Vec<PlanStepAttempt>>`
  - a helper to compute the next derived attempt index for the current step
  - a helper to compute `max(attempt)` across the run for the `__unknown_step__` fallback
  - read helpers for CLI status/list output if no existing query already returns current step + retry information cleanly
  - mutation helpers for CLI resume/cancel if no existing method already performs the exact transition safely
  - one transactional helper that moves a run to `waiting_t2` and enqueues the failure notification in the same SQLite transaction
- `crash_running_step_attempts_for_run(...)` must:
  - update only `status='running'` rows for the target run;
  - set `status='crashed'` and `finished_at`;
  - preserve existing `summary_json` / `checks_json`;
  - return the pre-finalization rows so recovery/runner can build `PlanFailureDetails`.
- Change `claim_next_runnable_plan_run(...)` semantics:
  - only `pending` runs are claimable by the normal runner;
  - stale `running` rows become recovery-only input and must not be reclaimed for execution.
- Either delete the old stale-attempt replay helper path or refactor it to delegate to the new crash helper. Do not leave two inconsistent recovery mechanisms in the store layer.
- CLI-side mutations:
  - `resume` should transition only `waiting_t2 -> pending` and clear/refresh claim fields as needed;
  - `cancel` should transition any non-terminal run (`pending`, `running`, `waiting_t2`) to `failed`, stamp terminal metadata, and clear claim fields;
  - status-update helpers used by the runner must preserve terminal `failed` rows so `cancel` wins deterministically if it races with an in-flight worker;
  - read-only list/status queries stay SQLite-only and do not touch LLM/provider paths.

### `src/main.rs`

- Add the new clap `plan` subcommands here and keep `main.rs` as the single command-tree owner for this feature.
- Add:
  - `autopoiesis plan status [plan-run-id]`
  - `autopoiesis plan list`
  - `autopoiesis plan resume [plan-run-id]`
  - `autopoiesis plan cancel [plan-run-id]`
- `plan status` behavior:
  - with `plan-run-id`, show that run’s status, current step, retry count, and timestamps;
  - without `plan-run-id`, show the most recently updated non-terminal run (`pending`, `running`, `waiting_t2`); if none exist, fall back to the most recently updated run overall.
- `plan list` behavior:
  - print active/recent runs in a stable, plain-text table/list format suitable for tests.
- `plan resume` behavior:
  - only allows `waiting_t2` rows to move back to `pending`;
  - prints a clear success/no-op message.
- `plan cancel` behavior:
  - sets the run status to `failed` through SQLite only, including currently `running` rows;
  - relies on terminal-state-preserving runner/store writes so later worker activity cannot overwrite `failed`;
  - prints a clear success/no-op message;
  - performs no tool, network, LLM, or auth work.
- Startup wiring:
  - call `recover_crashed_plans(...)` on the server startup path before the process starts accepting connections or scheduling plan work;
  - use the same stale threshold already used for queue/message recovery.

### `src/server/mod.rs`

- Thread the stale-threshold/bootstrap call so startup order is explicit:
  - open store / initialize runtime;
  - recover stale queue/message work;
  - recover crashed plan runs;
  - only then bind or accept incoming connections / start background loops.
- Keep server startup responsible for sequencing, not for re-implementing recovery logic.
- Add the startup-order test here so the file list stays exact and the recovery-before-accept invariant is exercised in one place.

### `docs/specs/plan-engine.md`

- Update the Crash Recovery section to match the actual implementation plan:
  - stale `running` rows are recovery-only input;
  - recovery crashes all in-flight attempts for the run, moves the run to `waiting_t2`, and notifies T2;
  - the `waiting_t2` transition and failure notification enqueue are atomic from the recovery/runner point of view;
  - the runner fails closed if it still encounters a preexisting in-flight attempt instead of replaying the step;
  - when definition lookup is corrupted, recovery uses a deterministic `__unknown_step__` fallback instead of aborting startup.
- Update the CLI Commands section:
  - document `status`, `list`, `resume`, and `cancel`;
  - document the chosen omitted-id behavior for `plan status`;
  - document that `cancel` applies to non-terminal rows and that terminal `failed` state wins over later worker writes;
  - keep the commands SQLite-only.

### `docs/architecture/overview.md`

- Add the `plan` module and the new recovery path to the module map / architecture overview.
- Note that startup recovery now covers both queue/message state and plan-run state.

### `docs/roadmap.md`

- Mark the Phase 6 / PE-6 items as complete.
- Keep the wording aligned with the final behavior in the spec: crash recovery, CLI controls, startup wiring, and tests.

## 3. Tests To Write

### Notification/store transaction tests (`src/plan/notify.rs` and `src/store.rs`)

- `transition_run_to_waiting_t2_and_notify_failure_is_atomic`
  - forces the notification insert path to fail inside the transaction;
  - asserts the run does not become `waiting_t2` on failure;
  - asserts no partial notification row is committed.
- `crash_running_step_attempts_for_run_marks_rows_crashed_and_returns_original_metadata`
  - inserts a run with one or more `running` attempts;
  - asserts the helper returns the original rows with preserved `checks_json` / `summary_json`;
  - asserts DB rows are now `crashed` with `finished_at` set.
- `claim_next_runnable_plan_run_does_not_claim_stale_running_rows`
  - inserts both `pending` and stale `running` runs;
  - asserts only the `pending` run is claimable.
- `cancel_plan_run_marks_failed_even_if_row_is_running`
  - asserts `pending`, `running`, and `waiting_t2` rows all move to `failed`;
  - asserts claim fields are cleared and terminal metadata is written.
- `runner_status_updates_do_not_overwrite_failed_runs`
  - asserts the conditional write path preserves terminal `failed` if cancel raced with worker completion.
- Add direct tests for both derived-attempt helpers:
  - current-step lookup uses `max + 1` for that step;
  - `__unknown_step__` fallback uses `max + 1` across the run.

### Recovery tests (`src/plan/recovery.rs`)

- `recover_crashed_plans_marks_running_attempts_crashed_and_sets_waiting_t2`
  - asserts every stale `running` attempt for the run becomes `crashed`;
  - asserts the run becomes `waiting_t2`;
  - asserts the function returns the recovered count.
- `recover_crashed_plans_notifies_owner_t2_session`
  - verifies the notification is queued for the owning T2 session, not just any session.
- `recover_crashed_plans_derives_step_and_attempt_when_no_running_attempt_exists`
  - covers the partial-recovery / broken-invariant case where the run is still `running` but no `running` attempt row remains;
  - asserts derived `step_id`, derived `attempt`, empty `checks`, and `waiting_t2`.
- `recover_crashed_plans_uses_unknown_step_fallback_when_definition_lookup_is_invalid`
  - corrupts `definition_json` or `current_step_index`;
  - asserts recovery still notifies T2 and moves the run to `waiting_t2` using `step_id="__unknown_step__"`.
- `recover_crashed_plans_fails_fast_without_partial_waiting_t2_state_on_notification_error`
  - forces the atomic `waiting_t2 + notification` transaction to fail for one stale run after an earlier run already recovered;
  - asserts the function returns `Err` immediately;
  - asserts the earlier successful run stays recovered;
  - asserts the failing run remains `status='running'` so a retry can safely pick it up.
- `recover_crashed_plans_is_idempotent_after_successful_pass`
  - first call recovers stale runs, second call recovers `0`.

### Runner safety tests (`src/plan/runner.rs`)

- `run_plan_step_detects_preexisting_running_attempt_and_returns_waiting_t2`
  - asserts no step execution happens once a preexisting in-flight attempt is detected.
- `recover_then_tick_plan_runner_does_not_replay_recovered_step`
  - recover a stale run first, then call `tick_plan_runner(...)`;
  - assert the run is not reclaimed or executed again;
  - assert it remains a T2 handoff case.
- `cancelled_run_is_not_reopened_by_late_runner_write`
  - simulates a cancel race against a worker that already loaded local state;
  - asserts the final persisted state remains `failed`.
- Update any existing stale-attempt tests so they assert “handoff/no replay” instead of “resume execution.”

### CLI tests (`src/main.rs`)

- Parser tests for:
  - `plan status`
  - `plan list`
  - `plan resume`
  - `plan cancel`
- Output-format tests for:
  - `plan status [id]`
  - `plan status` without `id`
  - `plan list`
- SQLite behavior tests for:
  - `status` without `id` prefers the most recently updated non-terminal run, then falls back to the most recent run overall;
  - `resume` moves `waiting_t2 -> pending`;
  - `cancel` marks the row failed, including when the row started as `running`.
- Assertions for all CLI tests:
  - output is stable and plain-text;
  - the command path is SQLite-only;
  - no LLM/provider/auth path is invoked.

### Startup/bootstrap test (`src/server/mod.rs`)

- Add one focused test that proves startup runs queue recovery, then plan recovery, before listeners/background plan work start.
- The assertion should fail if bootstrap order regresses and a stale `running` plan can be claimed before `recover_crashed_plans(...)` runs.

## 4. Order of Operations

1. Update `src/plan/notify.rs` and `src/store.rs` first: add the transactional notification/write path, the crash helper, the derived-attempt helpers, the conditional terminal-state-preserving updates, and the CLI read/write helpers. Add the notification/store tests in the same step.
2. Add `src/plan/recovery.rs` and export it from `src/plan.rs` in the same change. Add the recovery tests at the same time so the module is compiled and covered immediately.
3. Tighten `src/plan/runner.rs` so preexisting running attempts cause a handoff to T2 instead of replay, and so terminal `failed` rows are not overwritten after `plan cancel`. Update runner tests in the same change.
4. Add `Commands::Plan` plus the parser/output helpers in `src/main.rs` together with parser tests. This keeps the parser tests green as soon as they are added.
5. Add the CLI command handlers and their SQLite behavior/output tests.
6. Wire startup recovery into `src/server/mod.rs` before connections are accepted or plan work is scheduled, and add the startup-order test in the same change.
7. Update the docs: `docs/specs/plan-engine.md`, `docs/architecture/overview.md`, and `docs/roadmap.md`.
8. Run validation: `cargo fmt --check`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo build --release`.

## 5. Risk Assessment

- The biggest correctness risk is silent step replay after a crash. This plan removes stale `running` rows from the normal claim path and makes `run_plan_step(...)` fail closed if a preexisting in-flight attempt is still present.
- Recovery and notification are coupled. This plan resolves the prior contradiction by requiring the `waiting_t2` transition and notification enqueue to commit atomically; if that combined step fails, the run stays `running` and can be retried safely.
- `docs/risks.md` already warns that queue/approval invariants are imperfect. Recovery must therefore handle multiple simultaneous `running` attempts for one run by crashing all of them and sending one deterministic handoff notification based on the newest crashed attempt, or derived fallback metadata if none remain.
- Corrupted step metadata is now handled explicitly. If `definition_json` or `current_step_index` cannot identify a real step during recovery, the plan uses a deterministic `__unknown_step__` placeholder instead of aborting startup.
- The omitted-id behavior for `autopoiesis plan status` is a product choice, not an implicit default. This plan makes the choice explicit, tests it, and updates `docs/specs/plan-engine.md` so implementation and docs do not drift.
- `plan cancel` is intentionally SQLite-only, but it is now explicit that cancel applies to any non-terminal run and that terminal `failed` state must win over later worker writes. That choice needs store-level conditional updates and tests so the race is deterministic instead of best-effort.
