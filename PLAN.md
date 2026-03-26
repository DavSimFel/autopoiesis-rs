# PLAN.md

No code has been changed yet. This file is the implementation plan for the requested Session 1 cleanup.

## 1. Files Read

- Guidance and plan: `AGENTS.md` from the prompt, `docs/risks.md`, `docs/architecture/overview.md`, `/tmp/aprs-state-debate/EXECUTION_PLAN.md`
- Root/config files: `Cargo.toml`, `agents.toml`, `.github/workflows/ci.yml`
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

## 2. Exact Changes Per File

Important scope note: the requested Session 1 artifacts are small, but `xtask/lint.sh` must pass on the current tree. The exact lint checks from the execution plan would currently fail in additional production files, so Session 1 implementation must include those fixes now.

Important task-constraint note: the user requires `CODE_STANDARD.md` to match the execution plan verbatim, and separately requires `xtask/lint.sh` to run `cargo test --features integration --test integration`. That means the copied standard will immediately disagree with the script on this one command. The implementation plan must preserve the exact `CODE_STANDARD.md` text anyway and treat that mismatch as a task-level exception, not as license to edit the standard text.

- `CODE_STANDARD.md`
  - Create the file at repo root.
  - Copy the `CODE_STANDARD.md` block from `/tmp/aprs-state-debate/EXECUTION_PLAN.md` verbatim, with no edits.
  - Do not “fix” the integration-command sentence in this file; the exact-text requirement wins.

- `src/lib.rs`
  - Prepend the exact `#![cfg_attr(not(test), deny(...))]` block from the execution plan.
  - Do not widen the deny list beyond the exact seven clippy lints in the plan.

- `xtask/lint.sh`
  - Create `xtask/` and add `lint.sh`.
  - Start from the execution plan script exactly.
  - Change only two requested details:
  - Make the `#[allow(` grep exclude test files by path: files under `tests/`, files named `tests.rs`, and files under any `tests/` subdirectory.
  - Change the auth-gated integration command to `cargo test --features integration --test integration`.
  - Mark the script executable.

- `tests/xtask_lint_paths.sh`
  - Create a shell smoke test that exercises the shipped `./xtask/lint.sh`, not a copied grep command.
  - The script should create disposable probe files inside the real repo tree, for example under `src/__lint_path_probe__/` and `tests/__lint_path_probe__/`, so the real lint script sees them when it greps `src/`.
  - The script should use `trap` cleanup to remove every probe file and directory before exit, even on failure.
  - The script should assert that a production probe file containing `#[allow(...)]` fails lint, while probe files under ignored test paths do not trigger the `#[allow]` gate.
  - This script is a post-cleanup validation step, not an early bootstrap step, because it exercises the full shipped lint script.

- `.github/workflows/ci.yml`
  - Keep checkout, toolchain install, and cargo cache steps.
  - Replace the separate `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` steps with a single `run: ./xtask/lint.sh` step.
  - Add a distinct `cargo build --release` step, because repo policy requires it and `xtask/lint.sh` will not cover it.
  - Add a step to run `bash tests/xtask_lint_paths.sh`.

- `src/plan/runner.rs`
  - Remove both `#[allow(clippy::too_many_arguments)]` attributes.
  - Refactor `handle_spawn_missing_metadata(...)` so its inputs are grouped into a context struct or smaller helper parameters.
  - Refactor `build_step_summary(...)` the same way, while preserving the current JSON payload shape and existing behavior.

- `src/plan/executor.rs`
  - Remove `#[allow(dead_code)]`.
  - The wrapper is already used by `src/plan/runner.rs`, so this should be a straight deletion of the stale allow unless a deeper compiler warning appears.

- `src/agent/shell_execute.rs`
  - Delete the unused `guarded_shell_execute(...)` convenience wrapper.
  - Remove `#[allow(dead_code)]`.
  - Rewrite the local tests to build a `ToolCall` and exercise `guarded_shell_execute_call(...)` directly.
  - Replace the manual JSON error string `format!(r#"{{"error": "{err}"}}"#)` with typed serialization through `serde_json`.

- `src/auth.rs`
  - Rename `AuthorizationResponse.code_challenge` to `_code_challenge`.
  - Add `#[serde(rename = "code_challenge", default)]` so deserialization stays wire-compatible.
  - Remove `#[allow(dead_code)]`.

- `src/plan/patch.rs`
  - Remove the production `.expect("checked is_some above")`.
  - Fold the `is_some()` check and the extraction into a single `if let Some(plan_run_id) = ...` branch so the code stays equivalent without tripping `clippy::expect_used`.

- `src/gate/output_cap.rs`
  - Remove the production `.expect("writing to String cannot fail")`.
  - Replace the `write!` call with infallible string building for the escaped hex suffix.

- `src/agent/loop_impl.rs`
  - Replace the manual JSON error string used for non-shell tool execution with typed JSON serialization.

- `src/config.rs`
  - Replace `ShellPolicy.default: String` with a config-owned typed enum.
  - Replace `ShellPolicy.default_severity: String` with a config-owned typed enum.
  - Do not import gate-owned enums into `config`; that would create a `config` <-> `gate` cycle because `src/gate/shell_safety.rs` already depends on `config::ShellPolicy`.
  - Update load/deserialize/default logic so invalid values fail config load instead of surviving to runtime.
  - Update any builders/default constructors and in-file tests that still construct `ShellPolicy` with raw strings.

- `src/gate/shell_safety.rs`
  - Remove the fail-open `_ => ShellDefaultAction::Approve` branch.
  - Remove the fail-open `_ => Severity::Medium` branch.
  - Stop using `shell_words::split(...).ok()` in production logic; parse failures must produce an explicit fail-closed outcome.
  - Consume the new config-owned shell-policy enums and map them into runtime gate behavior inside the gate layer, keeping dependencies one-way.

- `src/gate/secret_redactor.rs`
  - Remove `Regex::new(...).ok()` fail-open behavior.
  - Promote invalid regex construction to an error path so the boundary fails closed.

- `src/gate/exfil_detector.rs`
  - Remove `shell_words::split(...).ok()` fail-open behavior.
  - Make malformed shell syntax an explicit suspicious/error path instead of silently disabling the detection branch.

- `src/llm/mod.rs`
  - Remove `#![allow(async_fn_in_trait)]`.
  - Replace the async trait method with a boxed-future trait signature owned in `llm`, for example a local future type alias plus `fn stream_completion(...) -> LlmFuture<'_, StreamedTurn>`.
  - Do this without adding a new dependency such as `async-trait`.

- `src/llm/openai.rs`
  - Update `OpenAIProvider` to implement the boxed-future `LlmProvider` signature introduced in `src/llm/mod.rs`.

- `LlmProvider` implementation sites in tests and helpers
  - Update `src/agent/tests/common.rs`
  - Update `src/agent/tests/regression_tests.rs`
  - Update `src/agent/loop_impl/tests.rs`
  - Update `src/agent/spawn/tests.rs`
  - Update `src/server/queue.rs`
  - Update `src/turn.rs`
  - Each implementation should return `Box::pin(async move { ... })` so the trait refactor compiles in one batch.

- `src/turn.rs`
  - Update turn construction to handle the fail-closed `SecretRedactor` and config-owned typed `ShellPolicy` APIs.
  - Preserve the current `build_turn_for_config*` surface while plumbing through the stricter constructors.

- Test helpers and test modules that construct `ShellPolicy` or `SecretRedactor`
  - Update helper constructors and call sites after the typed-config and fail-closed API changes.
  - Expected touch points include `src/agent/tests/common.rs`, `src/agent/loop_impl/tests.rs`, `src/gate/shell_safety.rs` tests, `src/server/auth.rs` tests, `src/server/http.rs` tests, `src/server/queue.rs` tests, `src/config.rs` tests, and `src/turn.rs` tests.

## 3. What Tests To Write

- `tests/xtask_lint_paths.sh`
  - Exercise the shipped `./xtask/lint.sh` against the real repo tree by creating and cleaning disposable probe files inside the repo before invoking the real script.
  - Only treat this as a pass/fail gate after the rest of the repo has been cleaned enough for `./xtask/lint.sh` to pass aside from the path-probe behavior being tested.
  - Required assertions: a production probe file with `#[allow(...)]` fails, `tests/integration.rs` style probe paths are ignored, `src/foo/tests.rs` style probe paths are ignored, and `src/foo/tests/bar.rs` style probe paths are ignored.
  - Fail the script on any mismatch so CI can treat it as a real gate.

- `src/config.rs`
  - Add regressions that `shell.default = "typo"` fails load.
  - Add regressions that `shell.default_severity = "typo"` fails load.
  - Assert valid defaults still deserialize into the new config-owned typed fields.

- `src/gate/secret_redactor.rs`
  - Add a unit test that an invalid regex returns an error instead of being silently dropped.

- `src/gate/shell_safety.rs`
  - Add a unit test that malformed shell syntax does not fall open through `shell_words::split`.
  - Add a unit test that config-owned default action/severity values are required before the guard is constructed.

- `src/gate/exfil_detector.rs`
  - Add a unit test that malformed shell syntax does not silently disable structured-read detection.

- `src/agent/shell_execute.rs` and `src/agent/loop_impl.rs`
  - Add or update tests so the new JSON error payload helper produces valid JSON for errors containing quotes and newlines.
  - Assert behavior stays the same from the caller’s perspective.

- `src/auth.rs`
  - Add a deserialization regression proving that a payload containing `code_challenge` still loads after the `_code_challenge` rename.

- `src/llm/mod.rs` and provider impl sites
  - Rely on the full build/test matrix to cover the boxed-future trait refactor.
  - Keep all existing provider-using tests green; the point of the refactor is API-shape cleanup, not behavior change.

- `src/plan/runner.rs`
  - Keep the existing runner regressions green.
  - Add a focused regression if payload construction changes during the refactor, so `StepSummaryPayload` and `waiting_t2` behavior stay stable.

- End-to-end validation
  - Run `bash tests/xtask_lint_paths.sh`
  - Run `cargo build --release`
  - Run `cargo fmt --check`
  - Run `cargo clippy -- -D warnings`
  - Run `cargo test`
  - Run `./xtask/lint.sh`
  - If `~/.autopoiesis/auth.json` exists, assert the script runs `cargo test --features integration --test integration`

## 4. Order Of Operations

1. Add `CODE_STANDARD.md` exactly as specified. This is isolated and does not affect the build.
2. Create `xtask/lint.sh` from the plan, with only the two requested changes, and make it executable. Do not wire CI yet.
3. Create `tests/xtask_lint_paths.sh` so it writes disposable probe files inside the real repo tree, invokes the shipped `./xtask/lint.sh`, and cleans everything up with `trap`.
4. Run the new script once to enumerate current failures and fix them in the smallest dependency order.
5. Remove stale production `#[allow(...)]` attributes in `src/plan/executor.rs`, `src/agent/shell_execute.rs`, `src/auth.rs`, and `src/plan/runner.rs`.
6. Remove production `expect` usage in `src/plan/patch.rs` and `src/gate/output_cap.rs`.
7. Replace manual JSON error string building in `src/agent/shell_execute.rs` and `src/agent/loop_impl.rs`.
8. Convert `src/config.rs` shell-policy defaults to config-owned typed values, then update `src/gate/shell_safety.rs` to consume them and fail closed without introducing a config/gate cycle.
9. Convert `src/gate/secret_redactor.rs` and `src/gate/exfil_detector.rs` away from `.ok()` fail-open parsing, then update `src/turn.rs` and test helpers for the stricter constructors.
10. Refactor `src/llm/mod.rs` to a boxed-future `LlmProvider` trait and update every implementation site in one compileable batch, including `src/llm/openai.rs`, `src/agent/tests/common.rs`, `src/agent/tests/regression_tests.rs`, `src/agent/loop_impl/tests.rs`, `src/agent/spawn/tests.rs`, `src/server/queue.rs`, and `src/turn.rs`.
11. Add the exact clippy deny block to `src/lib.rs`.
12. Once the repo is otherwise clean enough for `./xtask/lint.sh` to pass, run `bash tests/xtask_lint_paths.sh` and get it green.
13. Update `.github/workflows/ci.yml` to call `./xtask/lint.sh`, add `cargo build --release`, and run `bash tests/xtask_lint_paths.sh`.
14. Run `bash tests/xtask_lint_paths.sh`, `cargo build --release`, `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `./xtask/lint.sh`.
15. If auth is present, confirm `cargo test --features integration --test integration` passes through the script.
16. Only after all checks are green, run `openclaw system event --text 'Session 1 done: CODE_STANDARD.md + gates installed' --mode now`.

## 5. Risk Assessment

- The execution plan labels typed config and fail-closed security changes as later sessions, but the Session 1 lint script would fail immediately on the current codebase. Session 1 therefore pulls in extra production refactors now.
- The exact-copy `CODE_STANDARD.md` requirement conflicts with the user-mandated narrower integration command in `xtask/lint.sh`. This mismatch is known in advance and cannot be resolved inside `CODE_STANDARD.md` without violating the exact-text requirement.
- The requested `#[allow(` grep fix is path-based only. It will ignore `tests/`, `*/tests.rs`, and `*/tests/*.rs`, but it will still flag any future `#[allow(...)]` placed inside inline test modules within production files.
- Tightening `ShellPolicy` from raw strings to config-owned typed values can ripple through config defaults, test helpers, and direct struct literals. This is the highest compile-churn area in the session.
- Making `SecretRedactor` fail closed likely changes constructor signatures or turn-building paths. That will force coordinated updates in `src/turn.rs` and many tests.
- Removing `#![allow(async_fn_in_trait)]` requires a trait-signature change that touches every `LlmProvider` impl. That is manageable, but it must be done in one compileable batch so the tree never sits half-migrated.
- The grep-path smoke test must clean up probe files on every exit path. If cleanup is wrong, it can pollute the worktree and create false positives in later lint runs.
- Replacing manual JSON error strings must preserve the current payload contract expected by existing tests and persistence code.
- CI should switch to `./xtask/lint.sh` only after the script is green locally; otherwise the branch will fail immediately for reasons already known.
