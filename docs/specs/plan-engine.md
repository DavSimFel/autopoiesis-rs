# Plan Engine

> Status: implemented.
> Purpose: T2 emits structured plans; the runtime persists and executes them.

## Summary

Plans are T2's structured execution format. T2 emits a fenced `plan-json` block, the runtime validates it, stores it in SQLite, and then executes the steps one by one. Failed steps are reported back to T2 so it can patch or replace the remaining suffix of the plan.

The implementation reuses existing primitives:

- `spawn_child()` for child-session work
- `src/agent/shell_execute.rs` for shell steps and shell checks
- the message queue for T2 notifications
- SQLite for durable plan state

## Core Types

- `PlanAction` is the parsed T2 control message.
- `PlanActionKind` is `plan`, `done`, or `escalate`.
- `PlanStepSpec` is either `spawn` or `shell`.
- `SpawnStepSpec` holds task, tier, optional model override, optional reasoning override, and requested skills.
- `ShellCheckSpec` holds a command and a typed expectation.
- `ShellExpectation` can check exit code, stdout substring/equality, or stderr substring.

## Parsing

T2 writes a fenced block in its assistant response:

````markdown
```plan-json
{ ... }
```
````

The runtime:

1. Extracts the fenced payload from the last assistant message.
2. Parses it with serde.
3. Rejects malformed or incomplete actions.
4. Creates a new plan run or patches the existing one.

## Durable State

The implementation uses two plan tables:

- `plan_runs`
- `plan_step_attempts`

`plan_runs` owns the current definition, status, revision, and current step index.

- `plan_runs.id`
- `plan_runs.owner_session_id`
- `plan_runs.topic`
- `plan_runs.trigger_source`
- `plan_runs.status` (`pending`, `running`, `waiting_t2`, `completed`, `failed`)
- `plan_runs.revision`
- `plan_runs.current_step_index`
- `plan_runs.active_child_session_id`
- `plan_runs.definition_json`
- `plan_runs.last_failure_json`
- `plan_runs.claimed_at`
- `plan_runs.created_at`
- `plan_runs.updated_at`

`plan_step_attempts` records execution history, check outcomes, and crash state for each step attempt.

- `plan_step_attempts.id`
- `plan_step_attempts.plan_run_id`
- `plan_step_attempts.revision`
- `plan_step_attempts.step_index`
- `plan_step_attempts.step_id`
- `plan_step_attempts.attempt`
- `plan_step_attempts.status` (`running`, `passed`, `failed`, `crashed`)
- `plan_step_attempts.child_session_id`
- `plan_step_attempts.summary_json`
- `plan_step_attempts.checks_json`
- `plan_step_attempts.started_at`
- `plan_step_attempts.finished_at`
- `plan_step_attempts` cascades on delete from `plan_runs`

Important fields and behaviors:

- `definition_json` is the current plan snapshot.
- `revision` increments when T2 patches a run.
- `current_step_index` tracks the next executable step.
- `claimed_at` implements the lease used for recovery.
- `last_failure_json` carries the structured failure payload back to T2.

## Execution Model

The runner processes one plan run at a time:

1. Claim a pending or running plan row.
2. Read the current step from `definition_json`.
3. Execute the step.
4. Run postcondition checks.
5. Record the attempt in `plan_step_attempts`.
6. Advance on success.
7. Move to `waiting_t2` on failure and notify T2.

### Spawn Steps

- Build a spawn request from the step spec.
- Create a child session.
- Drain the child queue.
- Run the configured checks.
- Record the attempt and either advance or wait for T2.

### Shell Steps

- Execute the command through the guarded shell executor.
- Run the configured checks through the same path.
- Persist the outcome and either advance or wait for T2.

## Patch Semantics

T2 can patch a running plan instead of starting over.

- `plan_run_id` targets the existing run.
- `replace_from_step` replaces only the remaining suffix, not the completed prefix.
- The runner increments the revision when applying a patch.
- A patched `waiting_t2` run returns to an executable state.
- Completed attempts remain in history; patching does not erase them.

## Failure Handling

When a step fails, the runtime:

1. Marks the run as `waiting_t2`.
2. Stores a structured failure payload.
3. Enqueues a message to T2 with the failure context.

The failure message includes the run id, revision, step index, step id, attempt number, and check outcomes.

## Crash Recovery

Startup recovery claims stale plan runs, marks the running attempt as `crashed`, moves the run to `waiting_t2`, and notifies the owner T2 session. Fresh in-flight work is left alone. The recovery path exists so a crash does not lose the current step or the failure context, but the runtime does not blindly replay the step.

## CLI Integration

The plan lifecycle is visible in the CLI:

- `plan status`
- `plan list`
- `plan resume`
- `plan cancel`

## Current Boundary

`max_attempts` is parsed and validated, but it is not an automatic retry engine. The docs should not claim retry behavior that the runner does not actually perform.
