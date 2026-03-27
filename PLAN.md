# PLAN

## Tool Output Summaries

- `npx jscpd src/ --min-lines 5 --min-tokens 50`
  - 94 Rust files analyzed
  - 23,308 total lines
  - 170 clone groups
  - 1,938 duplicated lines
  - 8.31% duplicated lines / 9.33% duplicated tokens
  - Production hotspots confirmed by direct file reads: `src/store/mod.rs`, `src/store/plan_runs.rs`, `src/store/step_attempts.rs`, `src/gate/command_path_analysis.rs`, `src/session_runtime/drain.rs`, `src/agent/queue.rs`, `src/plan/executor.rs`
- `tokei src/ --sort lines`
  - 104 Rust files
  - 37,925 total lines
  - 34,144 code
  - 3,720 blanks
  - 61 comments
- Grep / structural evidence collected before planning
  - `src/store/mod.rs:183-507` is a 325-line `impl Store` block dominated by forwarding methods; the pure forwarding surface across sessions, queue, plan runs, step attempts, and subscriptions is about 303 lines.
  - `src/plan/executor.rs:1-20` is a pure wrapper around `crate::agent::shell_execute::guarded_shell_execute_call`.
  - `src/session_runtime/drain.rs:407-460`, `src/session_runtime/drain.rs:468-580`, and `src/agent/queue.rs:19-129` form a 278-line queue/drain wrapper and duplicated-role cluster.
  - `src/store/plan_runs.rs:50-137`, `src/store/plan_runs.rs:179-205`, `src/store/plan_runs.rs:208-238`, `src/store/plan_runs.rs:247-345`, `src/store/plan_runs.rs:386-418`, `src/store/plan_runs.rs:421-487`, `src/store/plan_runs.rs:519-549`, and `src/store/plan_runs.rs:551-588` form about 414 lines of repeated plan-run SQL/update/query patterns.
  - `src/store/step_attempts.rs:59-113`, `src/store/step_attempts.rs:171-267`, and `src/store/step_attempts.rs:270-313` form about 196 lines of repeated step-attempt validation/finalization logic.
  - `src/gate/command_path_analysis.rs:45-277` and `src/gate/command_path_analysis.rs:287-430` contain the write-side `identity_template_*` detection chains; `src/gate/command_path_analysis.rs:1019-1442` contains the protected/target read-side pairs.
- Signature Test output
  - `src/plan/runner.rs` contains these 5+ parameter helpers:
    - `build_step_call_id` at line 120
    - `run_check` at line 278
    - `run_checks` at line 339
    - `finalize_attempt` at line 357
    - `build_step_crash` at line 456
    - `build_waiting_t2_failure_details` at line 476
  - This scan was run. The signatures are real, but they are not a better first optimization target than the larger verified store/gate/drain duplication surfaces above.
- One More Case Test output
  - `src/session_runtime/drain.rs:483-518` and `src/session_runtime/drain.rs:542-580` duplicate the same `match message.role.as_str()` bookkeeping arms.
  - `src/store/plan_runs.rs:101-127` duplicates the same `NullableUpdate` branch shape twice.
  - `src/plan/notify.rs:116-178` repeats the same `waiting_t2` update statement across the three `NullableUpdate` arms.
  - `src/plan/patch.rs:170-197` is a one-arm status dispatcher worth leaving alone for now because it is small relative to the larger confirmed wins.

## Files Read

- Control/config/docs: `FEEDBACK.md`, `TASK.md`, previous `PLAN.md`, `Cargo.toml`, `agents.toml`, `docs/risks.md`, `docs/architecture/overview.md`
- `src/agent/`: `audit.rs`, `child_drain.rs`, `child_drain/tests.rs`, `loop_impl.rs`, `loop_impl/tests.rs`, `mod.rs`, `queue.rs`, `queue/tests.rs`, `shell_execute.rs`, `tests.rs`, `tests/common.rs`, `tests/regression_tests.rs`, `usage.rs`
- `src/app/`: `args.rs`, `mod.rs`, `plan_commands.rs`, `session_run.rs`, `subscription_commands.rs`, `tracing.rs`
- `src/`: `auth.rs`, `delegation.rs`, `identity.rs`, `lib.rs`, `logging.rs`, `main.rs`, `model_selection.rs`, `plan.rs`, `principal.rs`, `read_tool.rs`, `skills.rs`, `subscription.rs`, `template.rs`, `terminal_ui.rs`, `time.rs`, `tool.rs`
- `src/child_session/`: `completion.rs`, `create.rs`, `mod.rs`
- `src/config/`: `agents.rs`, `domains.rs`, `file_schema.rs`, `load.rs`, `mod.rs`, `models.rs`, `policy.rs`, `runtime.rs`, `spawn_runtime.rs`, `tests.rs`
- `src/context/`: `history.rs`, `identity_prompt.rs`, `mod.rs`, `skill_instructions.rs`, `skill_summaries.rs`, `subscriptions.rs`, `tests.rs`
- `src/gate/`: `budget.rs`, `command_path_analysis.rs`, `exfil_detector.rs`, `mod.rs`, `output_cap.rs`, `protected_paths.rs`, `secret_catalog.rs`, `secret_redactor.rs`, `shell_safety.rs`, `streaming_redact.rs`
- `src/llm/`: `history_groups.rs`, `mod.rs`, `openai/mod.rs`, `openai/request.rs`, `openai/sse.rs`
- `src/plan/`: `executor.rs`, `notify.rs`, `patch.rs`, `recovery.rs`, `runner.rs`
- `src/server/`: `auth.rs`, `http.rs`, `mod.rs`, `queue.rs`, `queue_worker.rs`, `session_lock.rs`, `state.rs`, `ws.rs`
- `src/session/`: `budget.rs`, `delegation_hint.rs`, `jsonl.rs`, `mod.rs`, `tests.rs`, `trimming.rs`
- `src/session_runtime/`: `drain.rs`, `factory.rs`, `mod.rs`
- `src/store/`: `message_queue.rs`, `migrations.rs`, `mod.rs`, `plan_runs.rs`, `sessions.rs`, `step_attempts.rs`, `subscriptions.rs`
- `src/turn/`: `builders.rs`, `mod.rs`, `tests.rs`, `tiers.rs`, `verdicts.rs`

## Planned Optimizations

### 1. Delete the thin plan shell-executor wrapper

- What
  - Remove `src/plan/executor.rs` and call the shared guarded shell executor directly from `src/plan/runner.rs`.
- Where
  - `src/plan/executor.rs:1-20`
  - `src/plan/runner.rs`
  - `src/plan.rs`
- Measured line count
  - 20 production lines exactly.
- Exact changes per file
  - `src/plan/runner.rs`: replace both `crate::plan::executor::guarded_shell_execute_call(...)` call sites with `crate::agent::shell_execute::guarded_shell_execute_call(...)`.
  - `src/plan/executor.rs`: delete the wrapper function.
  - `src/plan.rs`: remove `pub(crate) mod executor;`.
  - `src/agent/shell_execute.rs` or `src/plan/runner.rs`: host any relocated regression that only existed to cover the wrapper.
- Expected production lines saved
  - 20.
- Risk
  - Low.

### 2. Collapse the queue/drain wrapper layer and unify the duplicated role handlers

- What
  - Merge the duplicated fixed-turn and fresh-turn queue-processing paths and shrink `src/agent/queue.rs` to the minimum stable API surface.
- Where
  - `src/agent/queue.rs:19-129`
  - `src/session_runtime/drain.rs:407-460`
  - `src/session_runtime/drain.rs:468-580`
- Measured line count
  - 278 lines of wrapper/duplicated surface.
- Exact changes per file
  - `src/session_runtime/drain.rs`: add one shared helper for `"system"`, `"assistant"`, and unsupported roles so only the `"user"` arm differs between fixed-turn and fresh-turn processing.
  - `src/session_runtime/drain.rs`: keep `drain_queue_with_stats(...)` and `drain_queue_with_stats_fresh_turns(...)`, but route both through the same processor shape instead of two hand-written `match message.role.as_str()` blocks.
  - `src/session_runtime/drain.rs`: factor the `StoreDrainBackend::new(store)` / `SharedStoreDrainBackend::new(store)` wrappers into a smaller explicit helper so `drain_queue_with_store(...)` and `drain_queue_with_shared_store(...)` stop re-spelling the same call body.
  - `src/agent/queue.rs`: keep `pub async fn drain_queue(...)` as the stable entry point, but remove redundant wrapper bodies wherever a re-export or one-line delegating shim is enough.
- Expected production lines saved
  - About 145.
- Risk
  - Medium.

### 3. Move `Store` methods into responsibility modules and delete the forwarding block in `src/store/mod.rs`

- What
  - Keep `Store` construction, migrations, transactions, and shared types in `src/store/mod.rs`, but move the responsibility-specific `impl Store` methods into the modules that already contain the real behavior.
- Where
  - `src/store/mod.rs:183-507`
  - `src/store/sessions.rs`
  - `src/store/message_queue.rs`
  - `src/store/plan_runs.rs`
  - `src/store/step_attempts.rs`
  - `src/store/subscriptions.rs`
- Measured line count
  - 325 lines in the forwarding block, about 303 of them pure delegation.
- Exact changes per file
  - `src/store/mod.rs`: keep `Store`, constructors, migration/bootstrap wiring, transaction helpers, row decoders shared across modules, and shared clock/path helpers; delete the forwarding methods for sessions, queue, plan runs, step attempts, and subscriptions.
  - `src/store/sessions.rs`: add a local `impl Store` for session creation/listing/parent lookup methods.
  - `src/store/message_queue.rs`: add a local `impl Store` for queue enqueue/dequeue/status methods and keep `enqueue_message_in_transaction(...)` beside the queue SQL.
  - `src/store/plan_runs.rs`: add a local `impl Store` for plan-run lifecycle methods and keep the claim/recovery internals private.
  - `src/store/step_attempts.rs`: add a local `impl Store` for step-attempt lifecycle methods and keep the transaction helper private or `pub(crate)` only where needed.
  - `src/store/subscriptions.rs`: add a local `impl Store` for create/delete/list/refresh subscription methods and keep the `#[cfg(test)]` refresh helper in the same responsibility boundary.
- Expected production lines saved
  - About 285.
- Risk
  - Medium-low.

### 4. Factor the repeated plan-run SQL/update/query boilerplate

- What
  - Remove the repeated `plan_runs` SELECT projection, repeated claim query bodies, repeated status-update execution, and repeated transition helpers in `src/store/plan_runs.rs`.
- Where
  - `src/store/plan_runs.rs:50-137`
  - `src/store/plan_runs.rs:179-205`
  - `src/store/plan_runs.rs:208-238`
  - `src/store/plan_runs.rs:247-345`
  - `src/store/plan_runs.rs:386-418`
  - `src/store/plan_runs.rs:421-487`
  - `src/store/plan_runs.rs:519-549`
  - `src/store/plan_runs.rs:551-588`
- Measured line count
  - About 414 lines of repeated or near-identical SQL/update/query structure.
- Exact changes per file
  - `src/store/plan_runs.rs`: introduce one shared plan-run column projection constant or builder used by `get_plan_run`, `list_plan_runs_by_session`, `list_recent_plan_runs`, `list_recent_active_plan_runs`, and `list_stale_running_plan_runs`.
  - `src/store/plan_runs.rs`: introduce one internal helper that executes a status update SQL string and owns the `changed == 0` / `bool` handling so `update_plan_run_status(...)` and `update_plan_run_status_preserving_failed(...)` stop duplicating the same call pattern.
  - `src/store/plan_runs.rs`: collapse the two `NullableUpdate` match blocks in `build_plan_run_status_update_sql(...)` into one helper that appends a nullable column assignment.
  - `src/store/plan_runs.rs`: collapse `claim_pending_plan_run_in_transaction(...)` and `claim_next_runnable_plan_run(...)` into one parameterized claim helper with caller-specific context strings.
  - `src/store/plan_runs.rs`: collapse `resume_waiting_plan_run(...)` and `cancel_plan_run(...)` into a shared transition helper that varies only the target status and allowed source statuses.
- Expected production lines saved
  - About 140.
- Risk
  - Medium.

### 5. Factor the repeated step-attempt validation and running-attempt finalization boilerplate

- What
  - Reduce the repeated running-only update guards, repeated validation blocks, and repeated stale/crash finalization loops in `src/store/step_attempts.rs`.
- Where
  - `src/store/step_attempts.rs:59-113`
  - `src/store/step_attempts.rs:171-267`
  - `src/store/step_attempts.rs:270-313`
- Measured line count
  - About 196 lines of repeated step-attempt logic.
- Exact changes per file
  - `src/store/step_attempts.rs`: add one helper for "update a running unfinished step attempt and error if nothing changed" so `update_step_attempt_status(...)`, `update_step_attempt_child_session(...)`, and `finalize_step_attempt(...)` stop repeating the same changed-row guard.
  - `src/store/step_attempts.rs`: move the repeated field validation in `record_step_attempt(...)` and `finalize_step_attempt(...)` into small named validators.
  - `src/store/step_attempts.rs`: reuse one query helper to load the running attempts that need to be crashed or finalized, then feed them through one finalization path instead of maintaining two separate loop bodies.
- Expected production lines saved
  - About 70.
- Risk
  - Medium.

### 6. Generalize the write-side `identity_template_*` detection chains in `command_path_analysis.rs`

- What
  - Refactor the write-side heuristics into shared helpers instead of parallel command/interpreter/redirection scanners.
- Where
  - `src/gate/command_path_analysis.rs:45-277`
  - `src/gate/command_path_analysis.rs:287-430`
- Measured line count
  - About 377 lines of write-side heuristic surface.
- Exact changes per file
  - `src/gate/command_path_analysis.rs`: introduce one shared helper for inline-script interpreters so the `perl`, `python`, `ruby`, and `node` branches stop re-spelling the same "scan args after a code flag and check the script" loop.
  - `src/gate/command_path_analysis.rs`: factor direct-write command handling into smaller helpers for destination-argument commands, in-place edit commands, redirection-driven writes, and raw target mentions.
  - `src/gate/command_path_analysis.rs`: keep `command_writes_identity_template_path(...)` and `command_writes_target_path(...)` as the outward entry points, but route target-path detection through the same write-detector helpers after path-rewrite normalization.
  - `src/gate/command_path_analysis.rs`: keep the existing recursion depth cap, shell-wrapper handling, and target-path canonicalization behavior unchanged.
- Expected production lines saved
  - About 155.
- Risk
  - High.

### 7. Generalize protected-path vs target-path read analysis in `command_path_analysis.rs`

- What
  - Preserve the existing read-side public entry points but unify the protected-path and target-path matcher families behind one internal predicate shape.
- Where
  - `src/gate/command_path_analysis.rs:1019-1442`
- Measured line count
  - 424 lines of paired read-side matching logic.
- Exact changes per file
  - `src/gate/command_path_analysis.rs`: introduce one internal matcher abstraction representing either protected-path matching or target-path matching.
  - `src/gate/command_path_analysis.rs`: collapse the paired helpers `git_option_value_references_*`, `git_config_value_references_*`, `command_argument_references_*`, `grep_file_operands_refer_*`, and `simple_command_reads_*`.
  - `src/gate/command_path_analysis.rs`: keep the current public signatures `simple_command_reads_protected_path(...)` and `simple_command_reads_target_path(...)`.
  - `src/gate/command_path_analysis.rs`: preserve the current `git` option parsing rules, `READ_ONLY_GIT_SUBCOMMANDS`, `git_path_spec_argument(...)`, and `env` wrapper handling exactly.
- Expected production lines saved
  - About 135.
- Risk
  - High.

### 8. Consolidate duplicated test fixtures into shared test support

- What
  - Treat test helpers as real code, but limit this pass to true clones and high-reuse setup. Deduplicate the repeated temp-root/store builders in crate-level test support, and keep the duplicated `ServerState` builders in server-local test support so the refactor does not depend on widening `ServerState` visibility.
- Where
  - `src/lib.rs`
  - `src/test_support.rs`
  - `src/server/mod.rs`
  - `src/server/test_support.rs`
  - `src/plan/notify.rs:223-234`
  - `src/plan/patch.rs:348-359`
  - `src/plan/recovery.rs:140-151`
  - `src/server/auth.rs:107-152`
  - `src/server/http.rs:175-226`
- Measured line count
  - 134 test lines of duplicated fixture/setup surface.
- Exact changes per file
  - `src/lib.rs`: add crate-root `#[cfg(test)] mod test_support;` wiring for generic temp-root/store helpers shared across the plan tests.
  - `src/test_support.rs`: add the shared temp-root/store helper used by `notify`, `patch`, and `recovery` tests.
  - `src/server/mod.rs`: add `#[cfg(test)] mod test_support;` so server-local test helpers stay inside the `server` visibility boundary.
  - `src/server/test_support.rs`: add the shared server-state builder used by the server test modules without forcing broader `ServerState` visibility.
  - `src/plan/notify.rs`, `src/plan/patch.rs`, and `src/plan/recovery.rs`: replace the local `test_store(...)` clones with `crate::test_support` helpers and keep the unique per-file fixtures local.
  - `src/server/auth.rs` and `src/server/http.rs`: replace the duplicated `test_state()` setup with the shared `server::test_support` helper.
  - Keep test names and assertions intact; do not move one-off helpers such as `test_plan_run(...)`, `create_waiting_plan_run(...)`, `valid_definition(...)`, or `stale_claim(...)` unless the implementation diff creates a second real call site.
- Expected test lines saved
  - About 85 test lines.
- Risk
  - Low.

## Tests To Write

- Shell executor regression
  - Assert plan shell steps and shell checks still execute through `crate::agent::shell_execute::guarded_shell_execute_call`.
  - Assert approval-allowed and approval-denied behavior is unchanged after deleting `src/plan/executor.rs`.
- Queue/drain parity regressions
  - Run the same queued `"system"`, `"assistant"`, and unsupported-role messages through both the fixed-turn path and the fresh-turn-builder path.
  - Assert identical `QueueOutcome`, identical persisted history, and identical queue row terminal states.
  - Assert the turn builder is never invoked for bookkeeping-only rows.
  - Assert child-completion enqueueing still happens only after a non-denied agent turn.
  - Run equivalent queue drains through both `drain_queue_with_store(...)` and `drain_queue_with_shared_store(...)` and assert identical claim, mark, and child-completion behavior.
  - For the `*_with_stats*` wrappers, assert `processed_any` and `last_assistant_response` parity in addition to the verdict and queue-state parity.
- Store relocation smoke test
  - Exercise one method from each moved `Store` responsibility through `Store` itself: session create/read, queue enqueue/dequeue/mark, plan-run create/read, step-attempt record/finalize, subscription create/list/delete.
  - Assert the public method names and behavior are unchanged after moving the `impl Store` blocks.
- `create_child_session_with_task(...)` atomicity regression
  - Force the enqueue leg to fail.
  - Assert there is no orphaned child session row and no queued task row after rollback.
- Subscription refresh override regression
  - Assert `refresh_subscription_timestamps_with(...)` still uses the injected modified-time callback in tests and does not fall back to filesystem metadata.
- Plan-run SQL regressions
  - Assert `claim_next_pending_plan_run(...)` and `claim_next_runnable_plan_run(...)` preserve ordering, stale-claim semantics, and returned row contents.
  - Assert `update_plan_run_status_preserving_failed(...)` still refuses to update a failed run.
  - Assert `resume_waiting_plan_run(...)` only resumes `waiting_t2`.
  - Assert `cancel_plan_run(...)` only cancels `pending`, `running`, or `waiting_t2`.
  - Add explicit coverage for all three `NullableUpdate` arms so the helperized update path preserves `Unchanged`, `Null`, and `Value`.
- Step-attempt regressions
  - Assert running-only guards still reject updates for finalized attempts.
  - Add a direct regression for `update_step_attempt_child_session(...)` so the shared running-attempt helper cannot break child-session writes while status/finalization tests still pass.
  - Assert `crash_running_step_attempts_for_run_in_transaction(...)` and `finalize_stale_step_attempts(...)` preserve status, `finished_at`, summary JSON, and checks JSON behavior.
  - Add one more case for "already finalized" vs "missing attempt" so the shared update helper does not collapse distinct error conditions incorrectly.
- `command_path_analysis.rs` write-side regressions
  - Add table-driven cases for `touch`, `rm`, `rmdir`, `tee`, `chmod`, `chown`, `cp`, `install`, `ln`, `dd`, `mv`, `sed -i`, and `git checkout` / `git restore`.
  - Preserve positive and negative interpreter cases for `perl`, `python`, `ruby`, and `node`.
  - Assert redirection parsing still catches real writes and ignores quoted or malformed `>` text.
  - Add explicit coverage for `env` wrappers, `busybox sh -c`, single-token inner-script reparse, and the recursion-depth cap so the helperized detector cannot silently broaden or narrow those heuristics.
  - Preserve the existing symlink/alias target-path rewrite cases.
- `command_path_analysis.rs` read-side regressions
  - Add table-driven protected-vs-target parity cases for `cat`, `head`, `tail`, `sed`, `awk`, `grep -f`, `grep --file`, plain grep file operands, and read-only `git` commands.
  - Assert `git -c`, `--git-dir`, `--work-tree`, and alias/config shell-command cases preserve current behavior exactly.
  - Add explicit positive `env`-wrapped read cases such as `env cat ...` and `env grep ...` so the unified matcher preserves the existing wrapped-read match path.
  - Add explicit positive and negative git pathspec cases so the unified matcher preserves both the `git_path_spec_argument(...)` match path and the false-path behavior.
  - Add explicit negative cases for non-read-only git subcommands and `env`-expanded commands so the unified matcher cannot overmatch on the false paths.
- Test-support regressions
  - Assert shared temp-root helpers still produce isolated roots and cleanly separate queue/session state across tests.
  - Keep the total test count at 595.

## Order Of Operations

1. Delete `src/plan/executor.rs` and switch `src/plan/runner.rs` to the shared shell executor.
   - Smallest deletion-first change.
   - Land the shell-path regression in the same commit.
2. Refactor `src/session_runtime/drain.rs` and then shrink `src/agent/queue.rs`.
   - Keep this as one queue/drain-only commit.
   - Land the fixed-turn vs fresh-turn parity tests in the same commit.
3. Move the `Store` impl blocks out of `src/store/mod.rs` and into the responsibility modules without changing SQL behavior.
   - This is the module-boundary cleanup commit.
   - Land the `Store` smoke test, `create_child_session_with_task(...)` rollback test, and subscription refresh override regression in the same commit.
   - Update any inline unit-test imports in the touched store modules in the same commit so visibility changes do not leave the tree temporarily broken.
4. Refactor `src/store/plan_runs.rs` only.
   - Keep claim/update/list SQL dedup isolated from `step_attempts`.
   - Land the plan-run transition and `NullableUpdate` regression cases in the same commit.
   - Keep any `src/store/plan_runs.rs` inline unit-test edits in this same commit rather than deferring them.
5. Refactor `src/store/step_attempts.rs` only.
   - Keep the running-attempt/finalization changes isolated from `plan_runs`.
   - Land the running-only and stale-finalization regressions in the same commit.
   - Keep any `src/store/step_attempts.rs` inline unit-test edits in this same commit rather than deferring them.
6. Refactor the write-side `identity_template_*` detection code in `src/gate/command_path_analysis.rs`.
   - Ship the write-side table-driven regression cases in the same commit.
7. Refactor the protected-vs-target read-side matcher family in `src/gate/command_path_analysis.rs`.
   - Ship the read-side table-driven regression cases in the same commit.
8. Consolidate the test fixtures into shared `#[cfg(test)]` support.
   - Keep this test-only.
   - Do not mix it into a production refactor commit.
   - Add the crate-root generic helper wiring and the server-local helper wiring in the same commit as the shared helpers so the test tree compiles at every step without widening `ServerState` visibility first.
9. After each commit:
   - Run targeted tests for the touched surface first.
   - Then run `cargo fmt --check`.
   - Then run `cargo clippy -- -D warnings`.
   - Then run `cargo build --release`.
   - Then run `cargo test`.
   - Confirm the total test count stays at 595.

## Risk Assessment

- Low risk
  - Delete `src/plan/executor.rs`
  - Consolidate test fixtures
- Medium risk
  - Queue/drain consolidation
  - `Store` impl relocation
  - `src/store/plan_runs.rs` SQL dedup
  - `src/store/step_attempts.rs` finalization/validation dedup
- High risk
  - `command_path_analysis.rs` write-side heuristic refactor
  - `command_path_analysis.rs` read-side matcher unification

Primary regression risks to watch:

- Queue rows must still always end as `processed` or `failed`.
- Child-session completion messages must still enqueue only when the current semantics say they should.
- Fresh turn construction must still happen only for queued user messages.
- `create_child_session_with_task(...)` must stay atomic.
- `update_plan_run_status_preserving_failed(...)` must not resurrect failed plan runs.
- Stale/crashed step attempts must keep the same terminal status and JSON payload behavior.
- Protected-path and target-path detection must not weaken for `git` alias/config execution paths, pathspecs, symlinks, or shell-wrapper forms.

## Total Expected Savings

- Production
  - thin plan executor wrapper deletion: 20
  - queue/drain consolidation: about 145
  - `Store` forwarding removal: about 285
  - `plan_runs.rs` SQL/update/query dedup: about 140
  - `step_attempts.rs` validation/finalization dedup: about 70
  - `command_path_analysis.rs` write-side dedup: about 155
  - `command_path_analysis.rs` read-side dedup: about 135
  - total expected production savings: about 950
- Tests
  - shared plan/server test support: about 85
- Combined tracked savings
  - about 1,035 total lines
