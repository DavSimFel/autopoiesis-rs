# PLAN

Current-tree note: `TASK.md` is partially stale relative to this checkout. `src/plan/executor.rs` is already gone, `src/plan.rs` already omits `mod executor`, `src/plan/runner.rs` already calls `crate::agent::shell_execute::guarded_shell_execute_call(...)` directly, `src/agent/queue.rs` already delegates into `src/session_runtime::drain`, and the fresh-turn session drain path already flows through `build_turn_builder_for_subscriptions()` -> `build_turn_for_config_with_subscriptions()`. The plan below therefore separates true pending work from verification/doc-sync work that may be needed to make the requested commit sequence non-empty.

Unavoidable contract note: `.git/hooks/pre-commit` is outside git history. The plan can make the substantive secret-scan logic tracked and reviewable, but the hook-wrapper edit itself remains a required local step that cannot literally be captured inside one of the git commits. That mismatch with the task wording is real and must be stated explicitly, not treated as solved away.

Blocking prerequisite: before implementation starts, the user must accept one of these two interpretations for Fix 4:

- the `.git/hooks/pre-commit` wrapper edit is allowed to remain a required local-only step outside git history, while the substantive secret-scan logic lives in tracked files and in the Fix 4 commit, or
- the user provides a tracked hook source-of-truth path that the repo is allowed to version, so the hook behavior can be captured inside the Fix 4 commit itself.

Without one of those two approvals, the task cannot be completed exactly as written because git cannot commit files inside `.git/hooks/`.

## 1. Files Read

- Inputs and supporting files: `TASK.md`, `agents.toml`, `Cargo.toml`, `.git/hooks/pre-commit`, `docs/risks.md`, `docs/architecture/overview.md`
- `src/agent/audit.rs`, `src/agent/child_drain.rs`, `src/agent/child_drain/tests.rs`, `src/agent/loop_impl.rs`, `src/agent/loop_impl/tests.rs`, `src/agent/mod.rs`, `src/agent/queue.rs`, `src/agent/queue/tests.rs`, `src/agent/shell_execute.rs`, `src/agent/tests.rs`, `src/agent/tests/common.rs`, `src/agent/tests/regression_tests.rs`, `src/agent/usage.rs`
- `src/app/args.rs`, `src/app/mod.rs`, `src/app/plan_commands.rs`, `src/app/session_run.rs`, `src/app/subscription_commands.rs`, `src/app/tracing.rs`
- `src/auth.rs`
- `src/child_session/completion.rs`, `src/child_session/create.rs`, `src/child_session/mod.rs`
- `src/config/agents.rs`, `src/config/domains.rs`, `src/config/file_schema.rs`, `src/config/load.rs`, `src/config/mod.rs`, `src/config/models.rs`, `src/config/policy.rs`, `src/config/runtime.rs`, `src/config/spawn_runtime.rs`, `src/config/tests.rs`
- `src/context/history.rs`, `src/context/identity_prompt.rs`, `src/context/mod.rs`, `src/context/skill_instructions.rs`, `src/context/skill_summaries.rs`, `src/context/subscriptions.rs`, `src/context/tests.rs`
- `src/delegation.rs`
- `src/gate/budget.rs`, `src/gate/command_path_analysis.rs`, `src/gate/exfil_detector.rs`, `src/gate/mod.rs`, `src/gate/output_cap.rs`, `src/gate/protected_paths.rs`, `src/gate/secret_catalog.rs`, `src/gate/secret_redactor.rs`, `src/gate/shell_safety.rs`, `src/gate/streaming_redact.rs`
- `src/identity.rs`, `src/lib.rs`
- `src/llm/history_groups.rs`, `src/llm/mod.rs`, `src/llm/openai/mod.rs`, `src/llm/openai/request.rs`, `src/llm/openai/sse.rs`
- `src/logging.rs`, `src/main.rs`, `src/model_selection.rs`
- `src/plan.rs`, `src/plan/notify.rs`, `src/plan/patch.rs`, `src/plan/recovery.rs`, `src/plan/runner.rs`
- `src/principal.rs`, `src/read_tool.rs`
- `src/server/auth.rs`, `src/server/http.rs`, `src/server/mod.rs`, `src/server/queue.rs`, `src/server/queue_worker.rs`, `src/server/session_lock.rs`, `src/server/state.rs`, `src/server/ws.rs`
- `src/session/budget.rs`, `src/session/delegation_hint.rs`, `src/session/jsonl.rs`, `src/session/mod.rs`, `src/session/tests.rs`, `src/session/trimming.rs`
- `src/session_runtime/drain.rs`, `src/session_runtime/factory.rs`, `src/session_runtime/mod.rs`
- `src/skills.rs`
- `src/store/message_queue.rs`, `src/store/migrations.rs`, `src/store/mod.rs`, `src/store/plan_runs.rs`, `src/store/sessions.rs`, `src/store/step_attempts.rs`, `src/store/subscriptions.rs`
- `src/subscription.rs`, `src/template.rs`, `src/terminal_ui.rs`, `src/time.rs`, `src/tool.rs`
- `src/turn/builders.rs`, `src/turn/mod.rs`, `src/turn/tests.rs`, `src/turn/tiers.rs`, `src/turn/verdicts.rs`

## 2. Exact Changes Per File

### Fix 1: Delete `src/plan/executor.rs`

- `src/plan.rs`: no code change expected in this checkout; verify it still exposes only `notify`, `patch`, `recovery`, and `runner`.
- `src/plan/runner.rs`: no code change expected; verify both current shell call sites stay on `crate::agent::shell_execute::guarded_shell_execute_call(...)`.
- `src/agent/shell_execute.rs`: no code change expected unless a missing regression test is easier to place here than in `src/plan/runner.rs`.
- `docs/architecture/overview.md`: this is the planned non-empty tracked delta for Fix 1 if the code already matches the task. Remove the stale `src/plan/executor.rs` module-map entry and refresh the snapshot counts so the commit is deterministic instead of depending on optional spec churn.

### Fix 2: Route session drain through `build_turn_for_config()`

- `src/session_runtime/drain.rs`: likely no production diff; confirm there is no remaining direct `Turn::new()` or equivalent turn assembly in the fresh-turn path. If anything direct is found during implementation, replace it with builder injection only, not ad hoc turn construction.
- `src/session_runtime/factory.rs`: likely no production diff; the existing `build_turn_builder_for_subscriptions()` already closes over `turn::build_turn_for_config_with_subscriptions(&config, &subscriptions)`.
- `src/app/session_run.rs`: likely no production diff; keep the CLI path using `build_turn_builder_for_subscriptions(...)` and `drain_queue_with_store(...)`.
- `src/server/queue_worker.rs`: likely no production diff; keep the HTTP/WS path using the same builder factory and shared drain entrypoint.
- `src/server/queue.rs`: add the fresh-turn regression here, because the task is specifically about the shared server/session drain path. Use this file as the single concrete location for the tier/subscription parity test rather than splitting the coverage between multiple candidate test modules.

### Fix 3: Queue/drain deduplication

- `src/session_runtime/drain.rs`: likely no production diff; `process_non_user_message(...)` already centralizes the shared `"system"`, `"assistant"`, and unsupported-role branches. Only tighten naming/tests if a remaining duplicate branch is discovered.
- `src/agent/queue.rs`: likely no production diff; it already delegates message processing and drain logic into `src/session_runtime/drain.rs` while keeping `pub async fn drain_queue(...)` stable.
- `src/agent/queue/tests.rs`: extend or add a regression that explicitly exercises the shared non-user role behavior and queue bookkeeping through the public `agent::queue` surface.
- `src/server/queue.rs`: optionally mirror the same invariant on the shared-store/server drain path so both fixed-turn and fresh-turn routes prove identical handling for non-user rows.

### Fix 4: Replace hardcoded mock secrets with non-secret-shaped fixtures

- `src/server/auth.rs`: replace test fixtures `"test-key"` -> `"mock-api-key"` and `"operator-key"` -> `"test-operator-key"` in `test_state()`, token assertions, and WS query assertions.
- `src/server/http.rs`: replace the same fixtures in test state, request headers, and helper arguments so the HTTP tests stop embedding secret-ish placeholders.
- `src/server/queue.rs`: replace the same fixtures in test state setup.
- `src/llm/openai/mod.rs`: replace `"test-key"` in the SSE HTTP test provider construction with a clearly synthetic non-secret-shaped value such as `"mock-api-key"`.
- `src/gate/mod.rs`: remove the inline `sk-...` fixture-bearing tests from the production file path and replace them with `#[cfg(test)] mod tests;` so the moved coverage remains compiled as a child test module.
- `src/gate/tests.rs` (new): host the moved redaction tests with the real `sk-...` samples so default-catalog/OpenAI-pattern coverage stays intact in a test-only file.
- `scripts/pre_commit_secret_scan.sh` (new tracked file): hold the real secret-scan logic so the Fix 4 behavior lives in git history. It should still scan `ghp_`, `AKIA...`, and `PRIVATE KEY` across the full staged diff, while `sk-[a-zA-Z0-9_-]{20,}` is skipped for Rust test-only contexts: inline `#[cfg(test)]` blocks and test-only Rust files such as `*/tests.rs` / `tests/**/*.rs`.
- `tests/pre_commit_secret_scan.rs` (new tracked integration test): create fixture-driven coverage for `scripts/pre_commit_secret_scan.sh` by staging sample files in a temporary git repo and asserting pass/fail for inline `#[cfg(test)]`, `src/**/tests.rs`, non-test `sk-...`, and always-blocked secret patterns.
- `.git/hooks/pre-commit`: update the local hook to delegate its secret-scan step to the tracked helper before attempting the Fix 4 commit. The local wrapper change is still required because `.git/hooks` is untracked, but the substantive logic will now be reviewable and committable.

## 3. Tests To Write

### Fix 1

- If commit 1 is verification/doc-sync only, no new Rust test is strictly required.
- Keep existing `src/plan/runner.rs` and `src/agent/shell_execute.rs` tests passing to preserve shell-step coverage after confirming the wrapper is already gone.

### Fix 2

- Add a regression around the fresh-turn drain path.
- Invariant: the builder closure is invoked once per user message.
- Invariant: the built turn, not a direct `Turn::new()`, determines the tool surface.
- Invariant: subscription context still reaches provider input on the fresh-turn/server drain path.
- Invariant: guard composition matches the unified turn builder for the same config and subscriptions.
- Test matrix: include at least one non-default active-tier config (`t2` with subscriptions) and one shell-backed config (`t3` or budgeted `t1`) so tier resolution is actually exercised.
- Assertion set: processed user rows end in `processed`, the verdict shape is unchanged, tool definitions match a reference turn from `build_turn_for_config_with_subscriptions(...)`, budget behavior matches (`needs_budget_context()` / budget probe outcome), and representative tool-call verdicts still reflect the same shell/exfil guard stack as the reference turn.

### Fix 3

- Add or extend a regression covering both drain entry modes with mixed roles.
- Assertions:
- `"system"` rows append a system message with `Principal::from_source(&message.source)`.
- `"assistant"` rows append assistant text without invoking the provider.
- Unsupported roles still return `QueueOutcome::UnsupportedRole(...)` and are marked `processed`.
- Queue bookkeeping remains consistent: every claimed row ends `processed` or `failed`, and non-user rows do not trigger child-completion side effects by themselves.

### Fix 4

- Update the affected Rust tests to the new fixture names/prefixes.
- Add an automated integration test for the tracked helper script:
- `tests/pre_commit_secret_scan.rs` should create a temporary git repo, stage fixture files, run `scripts/pre_commit_secret_scan.sh`, and assert the intended pass/fail matrix.
- Required automated assertions:
- the moved `src/gate/tests.rs` cases must still prove real `sk-...` redaction coverage,
- a staged Rust addition inside a `#[cfg(test)]` block containing an `sk-...` sample must not fail the helper,
- a staged addition in a test-only Rust file such as `src/**/tests.rs` or `tests/**/*.rs` containing an `sk-...` sample must not fail the helper,
- a staged non-test Rust addition containing an `sk-...` sample must still fail,
- `ghp_`, `AKIA...`, and `PRIVATE KEY` must still fail regardless of block context.
- Local smoke verification is still required for the wrapper itself because `.git/hooks/pre-commit` is outside the cargo test harness.
- Required gate after each commit: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- Final validation after all four commits: `cargo test` still reports `594` or more passing tests, and `cargo build --release` succeeds with zero warnings.

## 4. Order Of Operations

1. Resolve the Fix 4 contract prerequisite above before touching code. If neither interpretation is accepted, stop; the task is not satisfiable exactly as written.
2. Re-verify current-tree vs `TASK.md` once the Fix 4 contract is resolved, but do not leave the commit strategy undecided: Fix 1 will use tracked architecture-doc sync as its non-empty delta, and Fixes 2-3 will use tracked regression coverage if the production code is already correct.
3. Fix 1 commit: confirm `src/plan/executor.rs` is already absent, confirm `src/plan/runner.rs` is already direct, and update `docs/architecture/overview.md` so the repo docs match the current tree.
4. Fix 2 commit: harden the fresh-turn/session-drain path with a regression proving unified builder parity, including tool surface, subscriptions, and guard behavior against a reference turn from `build_turn_for_config_with_subscriptions(...)`.
5. Fix 3 commit: harden or minimally simplify the shared non-user role path while keeping `pub async fn drain_queue(...)` stable and preserving queue bookkeeping.
6. Fix 4 preparation step before staging the commit: add the tracked `scripts/pre_commit_secret_scan.sh` helper and `tests/pre_commit_secret_scan.rs`, update the local `.git/hooks/pre-commit` wrapper to call the helper if the user accepted the local-step interpretation, and verify the wrapper no longer blocks Rust test-only `sk-...` fixtures.
7. Fix 4 commit: replace the tracked mock-secret fixtures, move the real `sk-...` redaction cases into `src/gate/tests.rs`, and include the tracked helper script plus its automated integration test so the substantive hook logic is part of git history.
8. After each tracked commit: run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
9. After the fourth commit and the hook-path resolution chosen in step 1: rerun `cargo test`, confirm the count is `594` or higher, run `cargo build --release`, and record the agreed Fix 4 interpretation in the implementation log. Then run `openclaw system event --text "Done: 4 fixes committed — executor deleted, turn constructor unified, queue deduped, mock secrets replaced" --mode now`.

## 5. Risk Assessment

- Highest risk: the task text is stale relative to the current checkout. The plan therefore relies on deterministic tracked doc/regression deltas for Fixes 1-3 instead of optional or empty commits.
- Blocking risk: the Fix 4 hook update lives under `.git/hooks/`, which git does not version. The plan therefore depends on an explicit up-front decision about whether a local-only wrapper edit is acceptable or whether a tracked hook source-of-truth must be introduced.
- Medium risk: the `.git/hooks/pre-commit` change is easy to get wrong if implemented as pure regex over unified diff text. Correct behavior needs test-context awareness for both inline `#[cfg(test)]` blocks and test-only files such as `*/tests.rs`; tracking the logic in `scripts/pre_commit_secret_scan.sh` is intended to make that behavior reviewable.
- Medium risk: even with the tracked helper, the `.git/hooks/pre-commit` wrapper update itself remains outside git history. The plan must continue to document that limitation so implementation does not over-claim what the four commits capture.
- Medium risk: moving the real `sk-...` redaction cases out of `src/gate/mod.rs` must preserve coverage of the default secret catalog in the new test-only file.
- Low-to-medium risk: even small edits in `src/session_runtime/drain.rs` can accidentally perturb queue bookkeeping, first-denial capture, or child-completion enqueue behavior. Keep behavior changes minimal and test-driven.
- Low risk: server auth/http/queue fixture replacement is mechanical, but the replacement strings must be updated consistently across state setup, headers, token lookups, and assertions.
- Low risk: `docs/architecture/overview.md` is definitely stale about `src/plan/executor.rs`; the plan avoids depending on potentially historical spec files for Fix 1.
