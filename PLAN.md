# Bug Hunt Plan

## Bugs

### 1. P1 - `Session::append` is non-atomic and the tests currently codify the broken state
Files: `src/session.rs:290-313`, `src/session.rs:1499-1542`

Description: `Session::append` mutates in-memory history and token counters before writing the JSONL row. If the file append fails, memory and persistence diverge immediately, and `budget_snapshot()` reports token totals that do not exist on disk. The regression test `append_failure_keeps_memory_and_persistence_separate` currently asserts this broken behavior instead of rejecting it.

Fix: Make append transactional from the session API boundary. Either write first and mutate memory only after success, or stage the mutations and roll them back on write failure. Rewrite the regression test to assert that failed appends leave memory, JSONL, and budget snapshots unchanged.

### 2. P1 - Inbound approval and approval-denial paths can target the wrong prompt
Files: `src/agent/loop_impl.rs:195-205`, `src/agent/loop_impl.rs:233-255`

Description: After `turn.check_inbound()` assembles context, the code rediscovers the "current" prompt by scanning the assembled message list. That works only while the newest text block is still the actual inbound user message. If a context source appends history or extra text at the tail, the approval prompt can display the wrong content, and the wrong user message can be persisted on approval denial or allow/modify.

Fix: Carry the current inbound user message as an explicit value through the loop. Persist and present that value after inbound guards, instead of mining the assembled context vector.

### 3. P1 - Queue draining stops on the first denied turn and can strand later rows
Files: `src/agent/queue.rs:141-148`, `src/server/queue.rs:111-113`, `src/server/queue.rs:177-187`, `src/server/ws.rs:159-176`

Description: `drain_queue_with_stats()` returns immediately on the first denied turn. The HTTP worker just logs that denial and exits; the WebSocket path sends a terminal denial and breaks. Any later queued rows for the same session stay pending until some future enqueue or stale-message recovery happens.

Fix: Decouple user-visible denial reporting from queue draining. Either continue draining after recording the first denial, or explicitly reschedule another worker when a denial short-circuits with more rows still pending.

### 4. P1 - Child-session completion can be enqueued even when no child turn completed
Files: `src/agent/queue.rs:109-168`, `src/server/queue.rs:70-138`, `src/spawn.rs:240-267`

Description: `processed_any` flips to `true` for every dequeued row, including stored `system`/`assistant` rows and unsupported roles. `should_enqueue_child_completion()` is just `processed_any`, so a child session can tell its parent "completed" after processing only bookkeeping rows, or after producing no fresh agent response at all.

Fix: Track whether a child agent turn actually executed and finished successfully. Gate completion enqueueing on that explicit signal, and prefer the fresh `last_assistant_response` produced by that run instead of any preexisting history.

### 5. P2 - `read_file` has a TOCTOU gap between validation and read
Files: `src/read_tool.rs:159-176`, `src/read_tool.rs:238-253`

Description: The tool validates `canonical_requested`, then calls `metadata()` and `read()` by path afterward. A raced rename or symlink swap can change what the final read resolves to after the allowed-root and protected-path checks have already passed.

Fix: Open the file once and validate/read through the descriptor. On Unix, use `O_NOFOLLOW` for the terminal component and validate descriptor metadata instead of re-walking the path.

### 6. P2 - Shell timeout cleanup silently assumes `setpgid` succeeded
Files: `src/tool.rs:373-376`

Description: The `pre_exec` closure ignores the return value of `libc::setpgid(0, 0)`. If process-group creation fails, the timeout path still calls `killpg()` as if the child owned its own group, which can leak descendants or miss the intended subtree.

Fix: Check the `setpgid` return code in `pre_exec` and return `io::Error::last_os_error()` when it fails, so the command never runs under a false containment assumption.

### 7. P1 - OpenAI streaming accepts truncated SSE streams as successful turns
Files: `src/llm/openai.rs:518-584`

Description: `stream_completion()` returns a `StreamedTurn` even if the HTTP stream ends before `[DONE]` or any other explicit terminal condition. A transient connection drop can therefore be persisted as a valid assistant reply, and partial text can leak through as a completed turn.

Fix: Require an explicit terminal event before returning success. EOF before a terminal marker should become an error, not a partially successful turn.

### 8. P1 - Missing/corrupt identity files can panic at runtime instead of returning an error
Files: `src/context.rs:45-61`, `src/turn.rs:281-283`

Description: `build_turn_for_config()` always uses `Identity::strict()`, and `Identity::load_prompt()` panics on load/render failure in strict mode. A missing file, bad template, or unreadable prompt can crash CLI execution or a server worker task instead of producing a normal error path.

Fix: Make context assembly fallible and propagate identity load failures as `Result`s. Do not panic from runtime prompt loading.

### 9. P1 - Identity assembly overwrites the first real system message
Files: `src/context.rs:70-91`

Description: If the first persisted history message is already `system`, identity assembly clears its content and replaces it with the rendered identity prompt. Operator-authored or server-authored system notes at the head of history are therefore silently dropped from model input.

Fix: Preserve real system history as separate messages. Insert a synthetic identity system message when needed, or tag/generated-detect the identity block so only that block is replaced on subsequent turns.

### 10. P1 - Production `History` trimming can split assistant/tool round-trips
Files: `src/context.rs:329-360`

Description: The production `History::assemble()` method trims one message at a time and does not respect assistant/tool grouping. The pair-aware version exists only in the `#[cfg(test)]` helper, so the safe logic is unavailable in real builds.

Fix: Move the pair-aware grouping logic into production and remove the test-only fork so the same behavior is exercised in tests and shipped code.

### 11. P1 - Budget enforcement is still post-turn instead of acting as a ceiling
Files: `src/agent/loop_impl.rs:179-189`, `src/gate/budget.rs:24-49`, `src/session.rs:531-545`, `src/agent/tests/regression_tests.rs:87-198`

Description: The budget guard checks only already-committed totals before the next inbound turn. The current turn can exceed every configured limit, and the denial happens only on the following user message. The regression test `budget_ceiling_is_enforced_on_the_next_turn` currently locks that deferred behavior in.

Fix: Add prospective/in-flight accounting so a turn that would exceed the configured ceiling is denied before or during execution, not one turn later. Update the regression test to assert same-turn enforcement.

### 12. P2 - `turn_tokens` undercounts multi-batch tool turns
Files: `src/session.rs:493-503`, `src/agent/loop_impl.rs:415-455`

Description: `budget_snapshot().turn_tokens` is derived from only the latest agent assistant message. Tool-heavy turns can append multiple assistant messages with provider metadata, and the earlier batches are dropped from the per-turn total.

Fix: Track explicit turn boundaries or accumulate assistant metadata until the next user message. The per-turn budget snapshot should reflect the whole completed turn, not just its last assistant batch.

### 13. P2 - Plan attempt numbering is inconsistent and can reuse an existing attempt index
Files: `src/plan/runner.rs:252-258`, `src/store.rs:953-961`, `src/store.rs:1538-1586`

Description: `plan::runner::next_attempt_index()` uses a count of same-revision rows, while the store helper and recovery path use `max(attempt) + 1`. Sparse or manually repaired histories can therefore reuse an existing attempt number. The schema also has no uniqueness constraint to catch the duplicate.

Fix: Remove the runner-local helper and use the store helper everywhere, or compute `max(attempt) + 1` directly. Add a unique index on `(plan_run_id, revision, step_index, attempt)` and a migration that rejects or repairs duplicates.

### 14. P1 - Successful plan-step paths ignore the "preserving failed" lost-race signal
Files: `src/plan/runner.rs:531-544`, `src/plan/runner.rs:745-758`, `src/plan/runner.rs:960-977`

Description: `update_plan_run_status_preserving_failed()` returns `bool`, but the completed/advanced paths ignore it. If another actor marks the run failed concurrently, the runner can still return `Completed` or `Advanced` and finalize the step attempt as passed while the plan row remains failed.

Fix: Treat `Ok(false)` as a lost race. Stop advancing, reload the plan row, and do not finalize a passed attempt after the plan has already been failed elsewhere.

## Files Read

Configs and docs:

- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`

Source files:

- `src/agent/loop_impl.rs`
- `src/agent/loop_impl/tests.rs`
- `src/agent/mod.rs`
- `src/agent/queue.rs`
- `src/agent/queue/tests.rs`
- `src/agent/shell_execute.rs`
- `src/agent/spawn.rs`
- `src/agent/spawn/tests.rs`
- `src/agent/tests.rs`
- `src/agent/tests/common.rs`
- `src/agent/tests/regression_tests.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/delegation.rs`
- `src/gate/budget.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/secret_patterns.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/main.rs`
- `src/model_selection.rs`
- `src/plan.rs`
- `src/plan/executor.rs`
- `src/plan/notify.rs`
- `src/plan/patch.rs`
- `src/plan/recovery.rs`
- `src/plan/runner.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/server/auth.rs`
- `src/server/http.rs`
- `src/server/mod.rs`
- `src/server/queue.rs`
- `src/server/ws.rs`
- `src/session.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## Exact Changes Per File

- `src/session.rs`: make append atomic; replace `latest_turn_tokens()` with explicit whole-turn accounting; rewrite the append-failure regression and add multi-batch turn-token tests.
- `src/agent/loop_impl.rs`: stop rediscovering inbound user text from assembled context; thread the current inbound message through allow/approve/deny paths; integrate same-turn budget checks if enforcement stays in the loop layer.
- `src/agent/queue.rs`: separate "a row was dequeued" from "an agent turn completed"; do not return early in a way that strands later rows; return richer drain stats if needed.
- `src/server/queue.rs`: keep HTTP draining semantics aligned with the queue fix; avoid exiting workers with pending rows still queued.
- `src/server/ws.rs`: preserve user-visible denial signaling without abandoning later queued work; make the post-denial behavior explicit.
- `src/spawn.rs`: gate child-completion enqueueing on an actual completed child turn and a fresh assistant response.
- `src/read_tool.rs`: replace path-based revalidation/read with descriptor-based open/verify/read logic; add nofollow or equivalent hardening.
- `src/tool.rs`: fail fast when `setpgid` fails in `pre_exec`; keep timeout cleanup assumptions honest.
- `src/llm/openai.rs`: require a terminal stream event before success; add explicit handling/tests for truncated SSE streams.
- `src/context.rs`: make identity loading fallible; preserve non-identity system messages; move pair-aware history trimming into production code.
- `src/turn.rs`: plumb fallible context assembly or fallible turn construction so identity failures do not panic.
- `src/gate/budget.rs`: switch from snapshot-only preflight checks to real ceiling enforcement based on projected/in-flight totals.
- `src/plan/runner.rs`: use monotonic attempt numbering, honor `update_plan_run_status_preserving_failed()` return values, and add lost-race handling.
- `src/store.rs`: expose one canonical next-attempt helper, add a uniqueness constraint for step attempts, and cover sparse attempt histories in tests.
- `src/agent/tests/regression_tests.rs`: replace the budget-next-turn regression with same-turn ceiling tests; add inbound approval tests with appended context/history.
- `src/agent/queue/tests.rs`: add multi-row denial/drain tests and child-completion gating tests.
- `src/llm/openai.rs` tests: add EOF-before-DONE failure tests and terminal-event success tests.
- `src/context.rs` tests: add coverage for preserving leading system notes and pair-safe history replay in production code.
- `src/plan/runner.rs` and `src/store.rs` tests: add sparse attempt numbering and concurrent fail-vs-success race coverage.

## What Tests To Write

- `session_append_failure_rolls_back_memory_and_budget_snapshot`
- `budget_snapshot_sums_all_assistant_batches_in_one_turn`
- `inbound_approval_uses_current_user_message_even_when_history_context_appends_tail_messages`
- `inbound_approval_denial_persists_current_user_message_not_history_tail`
- `drain_queue_continues_or_reschedules_after_denial_when_more_rows_exist`
- `child_completion_is_enqueued_only_after_completed_child_agent_turn`
- `read_file_rejects_symlink_swap_after_validation` or a lower-level descriptor/nofollow regression around the new helper
- `shell_pre_exec_errors_when_setpgid_fails` using a factored helper
- `openai_stream_completion_errors_on_eof_before_done`
- `openai_stream_completion_accepts_completed_stream_only_after_terminal_event`
- `identity_context_preserves_existing_system_message_at_history_head`
- `history_context_never_splits_assistant_tool_roundtrip_in_production`
- `budget_guard_denies_same_turn_when_projected_usage_exceeds_limit`
- `plan_runner_next_attempt_uses_max_plus_one_for_sparse_attempts`
- `plan_runner_does_not_finalize_passed_attempt_when_plan_already_failed`

Invariants those tests should assert:

- Memory, JSONL replay, and budget snapshots stay identical after failed persistence.
- Every queued row reaches a terminal state even when an earlier row is denied.
- Parent sessions receive child-completion messages only after a real child turn completes.
- SSE EOF without a terminal marker is never treated as a valid assistant turn.
- Identity/context assembly never destroys operator/system history.
- Attempt numbers are monotonic within `(plan_run_id, revision, step_index)`.
- A failed plan run cannot be advanced or completed by a stale success path.

## Order Of Operations

1. Fix `Session::append` atomicity and replace the regression that currently asserts divergence.
2. Fix inbound prompt selection and identity/system-message preservation in `context.rs` and `agent/loop_impl.rs`.
3. Fix queue drain semantics and child-completion gating so message delivery invariants stabilize before touching plan handoffs.
4. Harden `read_tool`, shell `pre_exec`, and OpenAI SSE termination handling.
5. Rework budget accounting so same-turn ceilings and multi-batch turn totals are correct.
6. Unify plan attempt numbering, add the uniqueness guard, and then fix the lost-race handling in `plan::runner`.
7. Run the full repo gate after each tranche: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, then `cargo build --release` before merge.

## Risk Assessment

- Highest behavioral risk: the queue/drain and budget fixes change control flow, so they can surface latent assumptions in CLI, HTTP, WebSocket, and spawn flows.
- Highest migration risk: adding a uniqueness constraint for plan attempts needs a safe migration or pre-migration cleanup for existing duplicate rows.
- Highest prompt-behavior risk: preserving real system messages and making identity loading fallible will change prompt ordering and failure modes; tests need to lock the intended ordering down.
- Highest platform risk: the `read_file` TOCTOU fix and any `O_NOFOLLOW` solution need a portable fallback for non-Unix builds.
- Highest operational risk: strict SSE termination will convert some previously silent partial successes into explicit errors. That is the correct behavior, but it will be user-visible once deployed.
# Review Amendments

These amendments supersede any narrower file lists, test scopes, or rollout ordering stated later in this document.

## Affected Files Added

- Bug 3 queue-drain/denial-contract work also affects `src/agent/spawn.rs` and `src/main.rs`, because both rely on the same drain/return behavior and must stay consistent with `src/agent/loop_impl.rs`.
- Bug 8 context/identity error-path work is not isolated to a single module. The compile surface includes at minimum `src/agent/loop_impl.rs`, `src/agent/spawn.rs`, `src/server/queue.rs`, `src/server/ws.rs`, `src/main.rs`, and all tests/helpers constructing `Turn`, `ContextSource`, or identity/context assembly inputs.

## Exact Changes Per File

- `src/agent/loop_impl.rs`
  - Bug 2: update the non-approval allow/modify persistence path at lines 195-205 so it matches the denial/approval persistence rules and does not leave state unsaved on successful non-approval guard outcomes.
  - Bug 3: keep queue-drain semantics aligned with `spawn.rs` and `main.rs`; any changed return contract here must be reflected at every caller.
  - Bug 8: thread fallible context/identity assembly through the turn-building path without creating a split API between CLI/server call sites.
  - Bug 14: fix all three success branches that currently ignore `preserving_failed`; do not stop after patching a single branch.
- `src/agent/spawn.rs`
  - Bug 3: update for any changed queue-drain/denial contract so spawned agent execution does not diverge from the direct path.
  - Bug 8: propagate fallible `Turn`/context construction changes through the spawn entrypoint and its tests.
- `src/main.rs`
  - Bug 3: update any direct consumer of the queue-drain/denial contract so CLI wiring stays behaviorally identical to spawned/server execution.
  - Bug 8: propagate fallible startup/turn-construction boundaries where `ContextSource` or identity assembly is initialized.
- `src/server/queue.rs`
  - Bug 8: update queue-driven turn construction to compile with the fallible context/identity boundary and preserve current HTTP/queue error mapping.
- `src/server/ws.rs`
  - Bug 8: update websocket turn/context initialization and approval flow wiring for the same fallible boundary.
- Tests/helpers that construct `Turn` or context/identity state
  - Bug 8: update fixtures/builders to compile after the fallible API change instead of papering over the new error path.

## What Tests To Write

- Bug 2
  - Add coverage for the non-approval allow path at `src/agent/loop_impl.rs:195-205`.
  - Add coverage for the non-approval modify path at the same site.
  - Assert that successful non-approval guard outcomes still persist the expected session/queue state and do not bypass cleanup.
- Bug 4
  - Extend the child-completion matrix with negative cases for stored `system` rows, stored `assistant` rows, and unsupported roles.
  - Assert that only valid parent/child role combinations advance completion state.
- Bug 8
  - Add compile-surface tests or fixture updates covering CLI, spawned-agent, queue, and websocket call paths so the new fallible boundary is exercised from every constructor path, not just one module-local unit test.
  - Assert that errors from context/identity assembly surface as typed/orchestrated failures instead of panics or silent drops.
- Bug 14
  - Add a race/regression test for each success branch that previously ignored `preserving_failed`.
  - Assert identical preservation behavior regardless of which success branch is taken.
  - Include a crash-recovery/regression assertion so preserved failures are not lost after restart/replay.

## Order Of Operations

1. Land duplicate-row detection and cleanup for plan attempts before adding any uniqueness index or migration for that data.
2. Add/adjust tests for the pre-migration duplicate cleanup path so the repository stays green while duplicate historical rows still exist.
3. Apply the uniqueness constraint only after the codebase can tolerate and clean pre-existing duplicates.
4. Make the Bug 8 fallible API boundary change as a single propagated step across `loop_impl.rs`, `spawn.rs`, `main.rs`, `server/queue.rs`, `server/ws.rs`, and shared test fixtures to avoid an intermediate non-compiling state.
5. Keep Bug 3 queue-drain contract changes synchronized across `loop_impl.rs`, `spawn.rs`, and `main.rs` in the same increment so tests do not pass on one execution path while failing on another.
6. Run `cargo test --features integration` when auth is available before considering the plan complete.
7. Update docs/specs required by repo policy before merge so the implementation does not leave `src/` and `docs/` out of sync.

## Risk Assessment Addendum

- Highest compile risk: Bug 8, because it changes a shared construction boundary used by CLI, spawn, queue, websocket, and tests.
- Highest rollout risk: plan-attempt uniqueness enforcement, because adding the index before duplicate cleanup can fail migration or break existing state.
- Highest regression risk: Bug 14, because partial fixes will appear green if only one success branch is tested.
# Canonical Replacement For Bug 3 And Bug 8

This section is the only authoritative plan text for Bug 3 and Bug 8. Any earlier Bug 3/Bug 8 bullets elsewhere in this document are obsolete and must not be used for implementation.

## Bug 3 Canonical Scope

### Affected files

- `src/agent/loop_impl.rs`
- `src/agent/spawn.rs`
- `src/main.rs`
- Any direct tests/helpers that assert queue-drain or denial-return behavior

### Exact changes

- Keep the denial/queue-drain contract identical across the direct loop path and the spawned path.
- Do not change the contract in one caller and adapt others later; all affected call sites move together.
- Preserve the invariant that every claimed queue row reaches a terminal state and that denial paths do not skip required cleanup or persistence.

### Tests

- Add an explicit direct-path regression test for the denial/queue-drain contract.
- Add an explicit spawned-path regression test for the same contract.
- Assert that both paths produce the same terminal queue/session state for denial, allow, and modified-allow outcomes where the bug applies.
- Fixture updates are not a substitute for these tests.

## Bug 8 Canonical Scope

### Affected files

- `src/turn.rs` as the shared `build_turn_for_config()` / `Turn` construction owner
- `src/agent/loop_impl.rs`
- `src/agent/spawn.rs`
- `src/main.rs`
- `src/server/queue.rs`
- `src/server/ws.rs`
- Any additional module that directly calls the shared turn builder
- All test/helpers that construct `Turn`, `ContextSource`, or identity/context assembly inputs

### Exact changes

- Make the context/identity assembly boundary fallible in the shared owner (`src/turn.rs`), not in per-caller wrappers.
- Propagate that fallible boundary outward through every caller without re-implementing builder logic in CLI/server-specific modules.
- Keep error ownership in the core/orchestration layer. Server modules may translate core errors into transport-specific responses, but the builder boundary must not depend on server-only error types or create a reverse dependency from core code into server code.
- Update tests/fixtures/builders in the same increment as the API change so the tree does not sit in a non-compiling intermediate state.

### Tests

- Add a unit/integration-style test at the shared turn-construction boundary (`build_turn_for_config()` / `src/turn.rs`) that asserts context/identity assembly failures are surfaced as errors rather than panics or silent drops.
- Add at least one CLI-path regression test that exercises the new fallible turn-construction boundary.
- Add at least one server-path regression test that exercises the same boundary through a queue or websocket entrypoint.
- Update fixtures/builders as needed, but do not count fixture maintenance as verification.

## Ordering Constraints

1. For Bug 3, land the direct-path and spawned-path contract tests before or alongside the implementation change so divergence is caught immediately.
2. For Bug 8, change the shared turn builder and its core error boundary first, together with fixture updates and the shared-boundary test.
3. Propagate the Bug 8 signature/error changes through CLI and server call sites in the same increment; do not leave wrapper-specific compatibility shims behind.
4. Only after the shared boundary compiles and its tests pass should path-specific translation logic be updated at the server edge.
5. If any earlier section in this document conflicts with this canonical Bug 3/Bug 8 block, this block wins.
# Final Review Corrections

This block supersedes all earlier Bug 3 scope text and all earlier Bug 8 test/ordering text in this document.

## Bug 3 Scope Clarification

- Queue-backed/server-driven execution is in scope for Bug 3 whenever it can claim, drain, or finalize queue rows.
- Treat the denial/queue-drain contract as shared across `src/agent/loop_impl.rs`, `src/agent/spawn.rs`, `src/main.rs`, and `src/server/queue.rs`.
- There are no path-specific exceptions unless a code path is proven to be a thin wrapper over already-tested shared logic.

### Bug 3 Tests

- Add a direct-path regression test for the denial/queue-drain contract.
- Add a spawned-path regression test for the same contract.
- Add a queue-backed/server-driven regression test for the same contract, unless `src/server/queue.rs` is a proven zero-logic wrapper over a shared helper already covered by tests; in that case, keep one thin integration test proving the wrapper preserves the shared behavior.

## Bug 8 Test Scope And Rollout

### Bug 8 Tests

- Require a shared-boundary test at `build_turn_for_config()` / `src/turn.rs`.
- Require a CLI-path regression test.
- Require a queue-server-path regression test.
- Require a websocket-server-path regression test.
- Fixture updates remain mandatory maintenance, but they do not count as verification.

### Bug 8 Ordering

- Replace the earlier split Step 2/Step 3 rollout with one atomic implementation step: update the shared turn builder, all affected callers (`loop_impl.rs`, `spawn.rs`, `main.rs`, `server/queue.rs`, `server/ws.rs`, and any direct builder callers), all affected fixtures/builders, and the shared-boundary tests together.
- Do not land an intermediate state where the shared builder signature changes before callers compile.
- If an incremental rollout is unavoidable, the only acceptable sequence is: introduce a temporary compatibility shim in the shared/core layer, migrate every caller and test, then remove the shim immediately in the next step. Do not leave the shim as the final state.
# Consolidated Corrections

This is the only authoritative correction block for this plan. The earlier amendment blocks (`Review Amendments`, `Canonical Replacement For Bug 3 And Bug 8`, and `Final Review Corrections`) are historical context only and must be ignored during implementation.

## Corrected Scope

- Bug 2 persistence fix: `src/agent/loop_impl.rs` at the non-approval allow/modify path (`195-205`) plus any tests covering that path.
- Bug 3 queue-drain/denial contract: `src/agent/loop_impl.rs`, `src/agent/spawn.rs`, `src/main.rs`, `src/server/queue.rs`, and any tests/helpers asserting queue terminal-state behavior.
- Bug 4 child-completion validation: the completion-state module plus tests covering stored role combinations.
- Bug 8 fallible turn/context boundary: `src/turn.rs` as the shared `build_turn_for_config()` / `Turn` construction owner, `src/agent/loop_impl.rs`, `src/agent/spawn.rs`, `src/main.rs`, `src/server/queue.rs`, `src/server/ws.rs`, any other direct turn-builder caller, and all tests/helpers constructing `Turn`, `ContextSource`, or identity/context assembly inputs.
- Bug 14 preserved-failure handling: every success branch that currently ignores `preserving_failed`, plus restart/replay tests.
- Plan-attempt uniqueness rollout: the data-cleanup path, migration/index path, and tests covering pre-existing duplicate rows.

## Corrected Changes

- Bug 2: persist state for successful non-approval allow and modify outcomes so they do not bypass cleanup or state writes.
- Bug 3: keep denial/queue-drain semantics identical across direct, spawned, CLI, and queue-backed/server-driven execution. Every claimed queue row must still reach a terminal state.
- Bug 4: reject invalid child-completion role combinations, including stored `system`, stored `assistant`, and unsupported-role cases.
- Bug 8: make context/identity assembly fallible in the shared turn builder (`src/turn.rs`), propagate that boundary through every caller, and keep the new error ownership in the core/orchestration layer rather than server-specific modules.
- Bug 14: patch all success branches that drop `preserving_failed`, not just one branch.
- Plan-attempt uniqueness: clean duplicates before adding the uniqueness constraint.

## Corrected Tests

- Bug 2: add explicit tests for the non-approval allow path and the non-approval modify path; assert persistence and cleanup both happen.
- Bug 3: add direct-path, spawned-path, and queue-backed/server-driven regression tests for the denial/queue-drain contract; assert equivalent terminal state for denial, allow, and modified-allow outcomes where applicable.
- Bug 4: add the negative child-completion matrix for stored `system`, stored `assistant`, and unsupported roles.
- Bug 8: add a shared-boundary test at `build_turn_for_config()` / `src/turn.rs`, a CLI-path regression test, a queue-server-path regression test, and a websocket-server-path regression test. Fixture updates are required, but they are not test coverage.
- Bug 14: add one regression test per success branch that previously ignored `preserving_failed`, plus a restart/replay assertion that preserved failures survive recovery.
- Plan-attempt uniqueness: add tests covering duplicate-row cleanup before migration and success after the uniqueness constraint is applied.

## Corrected Order Of Operations

1. Add tests for duplicate-row cleanup, then implement duplicate detection/cleanup for plan attempts.
2. Apply the uniqueness constraint only after duplicate cleanup exists and its tests pass.
3. Add or update Bug 2, Bug 3, Bug 4, and Bug 14 tests before or alongside each implementation change so the target invariant is pinned first.
4. For Bug 8, make one atomic change that updates the shared turn builder, every affected caller, all affected fixtures/builders, and the shared-boundary tests together. Do not land an intermediate broken signature.
5. If Bug 8 cannot be landed atomically, the only acceptable fallback is a temporary compatibility shim in the shared/core layer, followed immediately by caller migration and shim removal.
6. Keep Bug 3 direct, spawned, CLI, and queue-backed/server-driven contract changes synchronized in the same increment.
7. Run `cargo test --features integration` when auth is available.
8. Update required docs/specs before merge so the implementation and docs stay in sync.
# Bug 3 CLI Clarification

This note amends the consolidated Bug 3 section below.

- `src/main.rs` is treated as CLI wiring over the direct execution path unless the implementation uncovers path-specific queue/drain logic there.
- Minimum coverage requirement: keep the direct-path Bug 3 regression test, and add one thin CLI smoke/regression test that proves the `main.rs` wrapper preserves the same denial/queue-drain contract instead of altering terminal queue/session state.
