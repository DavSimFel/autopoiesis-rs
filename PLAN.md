# PE-2 Plan

## 1. Files Read

### Spec and architecture/docs
- `docs/specs/plan-engine.md`
- `docs/risks.md`
- `docs/architecture/overview.md`

### Config
- `Cargo.toml`
- `agents.toml`

### Source
- `src/lib.rs`
- `src/main.rs`
- `src/cli.rs`
- `src/auth.rs`
- `src/config.rs`
- `src/context.rs`
- `src/delegation.rs`
- `src/identity.rs`
- `src/model_selection.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/session.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/gate/mod.rs`
- `src/gate/budget.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/output_cap.rs`
- `src/gate/secret_patterns.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/server/mod.rs`
- `src/server/auth.rs`
- `src/server/http.rs`
- `src/server/queue.rs`
- `src/server/ws.rs`
- `src/agent/mod.rs`
- `src/agent/loop_impl.rs`
- `src/agent/queue.rs`
- `src/agent/spawn.rs`
- `src/agent/tests.rs`
- `src/agent/loop_impl/tests.rs`
- `src/agent/queue/tests.rs`
- `src/agent/spawn/tests.rs`
- `src/agent/tests/common.rs`
- `src/agent/tests/regression_tests.rs`

### Existing tests relevant to PE-2
- `tests/plan_engine.rs`

## 2. Exact Changes Per File

### `src/plan.rs` (new)
- Add a new pure data/parsing module with no execution logic.
- Define `PlanAction` with fields:
  - `kind: PlanActionKind`
  - `plan_run_id: Option<String>`
  - `replace_from_step: Option<usize>`
  - `note: Option<String>`
  - `steps: Vec<PlanStepSpec>`
- Define `PlanActionKind` enum with variants `Plan`, `Done`, `Escalate`.
- Define `PlanStepSpec` as a tagged serde enum with `rename_all = "snake_case"`:
  - `Spawn { id, spawn: SpawnStepSpec, checks: Vec<ShellCheckSpec>, max_attempts: u32 }`
  - `Shell { id, command, timeout_ms: Option<u64>, checks: Vec<ShellCheckSpec>, max_attempts: u32 }`
- Define `SpawnStepSpec` with:
  - `task`
  - `task_kind: Option<String>`
  - `tier`
  - `model_override: Option<String>`
  - `reasoning_override: Option<String>`
  - `skills: Vec<String>`
  - `skill_token_budget: Option<u64>`
- Define `ShellCheckSpec` with:
  - `id`
  - `command`
  - `expect: ShellExpectation`
- Define `ShellExpectation` with:
  - `exit_code: Option<i32>`
  - `stdout_contains: Option<String>`
  - `stderr_contains: Option<String>`
  - `stdout_equals: Option<String>`
- Derive `Serialize` and `Deserialize` on all new types. Also derive `Debug`, `Clone`, `PartialEq`, and `Eq` for testability and parity with other plain data types in the repo.
- Implement `validate_plan_action(action: &PlanAction) -> anyhow::Result<()>`.
  - Reject empty or whitespace-only step IDs.
  - Reject duplicate step IDs across `action.steps`.
  - Reject any step with `max_attempts < 1`.
  - Reject empty or whitespace-only `command` on `PlanStepSpec::Shell`.
  - Reject empty or whitespace-only `command` on every `ShellCheckSpec` attached to either `Spawn` or `Shell` steps.
  - Reject spawn steps whose `tier` is not exactly one of `t1`, `t2`, `t3`.
  - Require non-empty `steps` for `PlanActionKind::Plan`.
  - Require empty `steps` for `PlanActionKind::Done` and `PlanActionKind::Escalate`.
- Implement `extract_plan_action(assistant_text: &str) -> anyhow::Result<Option<PlanAction>>`.
  - Scan assistant text top-to-bottom for fenced code blocks.
  - Match only fences whose info string is exactly `plan-json`.
  - Treat the first matching `plan-json` block as the contract and ignore any later `plan-json` blocks.
  - If no `plan-json` fence exists, return `Ok(None)`.
  - If a `plan-json` opening fence is found but no closing fence exists, return `Err(...)`.
  - If a complete `plan-json` fence is found but the JSON body is malformed, return `Err(...)`.
  - After deserialization, call `validate_plan_action`; semantically invalid but well-formed JSON also returns `Err(...)`.
  - Do not continue scanning for later `plan-json` blocks after the first matching opening fence; if that first block is malformed or semantically invalid, return `Err(...)`.
- Add unit tests at the bottom of the file in `#[cfg(test)] mod tests`.

### `src/lib.rs`
- Export the new module with `pub mod plan;`.
- Make this edit in the same implementation step as the initial `src/plan.rs` addition so the new module and its unit tests compile immediately.

### `tests/plan_engine.rs`
- Unignore the three parser-oriented stubs and replace each `todo!()` with real assertions:
  - `extracts_plan_json_block_from_assistant_text`
  - `validates_plan_action_with_serde`
  - `rejects_malformed_plan_blocks`
- Rename the stale schema-oriented stub `creates_plan_engine_sqlite_schema` to `parses_done_action_with_empty_steps`.
- Replace that renamed test body with a PE-2 parser/validation assertion that a `Done` action with `steps: []` parses successfully.
- Add a fifth integration test named `parses_escalate_action_with_empty_steps`.
- Use that new test to assert that an `Escalate` action with `steps: []` also parses successfully.

### Docs/workflow follow-up
- No manual spec text change is planned for PE-2 because this work is implementing the existing `docs/specs/plan-engine.md` contract.
- During the implementation pass, if the repo’s normal hooks regenerate architecture stats under `docs/architecture/overview.md` because `src/` changed, keep and review that generated diff instead of discarding it.

## 3. Tests To Write

### Unit tests in `src/plan.rs`
- `validate_plan_action_accepts_valid_plan_with_spawn_and_shell_steps`
  - Build a `PlanActionKind::Plan` with one `spawn` step and one `shell` step.
  - Assert `validate_plan_action` succeeds.
- `extract_plan_action_returns_none_when_no_plan_json_block_exists`
  - Input contains assistant prose with no matching fenced block.
  - Assert `Ok(None)`.
- `extract_plan_action_ignores_non_plan_json_fences`
  - Input contains fenced blocks like `json` or `rust`, but no `plan-json`.
  - Assert `Ok(None)`.
- `extract_plan_action_parses_shell_and_spawn_steps_from_surrounding_text`
  - Input contains prose before and after a `plan-json` block.
  - Assert the parsed action matches the expected `PlanAction`.
- `extract_plan_action_rejects_unclosed_plan_json_fence`
  - Input contains a `plan-json` opening fence with no closing fence.
  - Assert `Err`.
- `extract_plan_action_rejects_malformed_json_in_plan_block`
  - Input contains a complete `plan-json` fence with invalid JSON syntax.
  - Assert `Err`.
- `extract_plan_action_uses_first_plan_json_block_when_multiple_exist`
  - Input contains two `plan-json` fences.
  - Assert the first block is parsed and later blocks are ignored.
- `extract_plan_action_rejects_malformed_json_first_block_even_if_later_block_is_valid`
  - Input contains two complete `plan-json` fences where the first has invalid JSON syntax and the second is valid.
  - Assert `Err` so the extractor cannot skip a malformed first matching block and recover by parsing a later one.
- `extract_plan_action_rejects_semantically_invalid_first_block_even_if_later_block_is_valid`
  - Input contains two `plan-json` fences where the first is well-formed JSON but fails validation and the second is valid.
  - Assert `Err` so the extractor cannot skip the first matching block or bypass `validate_plan_action`.
- `extract_plan_action_rejects_semantically_invalid_json_after_deserialization`
  - Input contains one complete `plan-json` fence with valid JSON that violates semantic rules such as `kind = "plan"` with `steps = []`.
  - Assert `Err` to prove `extract_plan_action` calls `validate_plan_action`.
- `validate_plan_action_rejects_duplicate_step_ids`
  - Two steps share the same `id`.
  - Assert `Err`.
- `validate_plan_action_rejects_empty_step_id`
  - A step uses `""` or whitespace-only `id`.
  - Assert `Err`.
- `validate_plan_action_rejects_zero_max_attempts`
  - A step sets `max_attempts` to `0`.
  - Assert `Err`.
- `validate_plan_action_rejects_empty_shell_step_command`
  - A shell step uses `""` or whitespace-only `command`.
  - Assert `Err`.
- `validate_plan_action_rejects_empty_shell_check_command`
  - A check attached to either step variant uses `""` or whitespace-only `command`.
  - Assert `Err`.
- `validate_plan_action_rejects_invalid_spawn_tier`
  - A spawn step uses a tier outside `t1`, `t2`, `t3`.
  - Assert `Err`.
- `validate_plan_action_requires_steps_for_plan_kind`
  - `kind = Plan` and `steps = []`.
  - Assert `Err`.
- `validate_plan_action_requires_empty_steps_for_done_kind`
  - `kind = Done` and `steps` is non-empty.
  - Assert `Err`.
- `validate_plan_action_requires_empty_steps_for_escalate_kind`
  - `kind = Escalate` and `steps` is non-empty.
  - Assert `Err`.

### Tests in `tests/plan_engine.rs`
- `extracts_plan_json_block_from_assistant_text`
  - Use realistic assistant markdown containing a `plan-json` fence.
  - Assert `extract_plan_action` returns the expected `PlanAction`.
- `validates_plan_action_with_serde`
  - Feed a valid JSON block covering both `spawn` and `shell` variants plus non-empty `checks`.
  - Assert serde parsing and validation succeed together.
- `rejects_malformed_plan_blocks`
  - Cover malformed JSON, malformed-first-with-later-valid JSON, and an unclosed `plan-json` fence.
  - Assert `extract_plan_action` returns `Err`, not `None`.
- `parses_done_action_with_empty_steps`
  - Feed a `Done` action with `steps: []`.
  - Assert parsing succeeds and validation does not require steps for non-`Plan` kinds.
- `parses_escalate_action_with_empty_steps`
  - Feed an `Escalate` action with `steps: []`.
  - Assert parsing succeeds and validation treats `Escalate` the same way as `Done`.

## 4. Order Of Operations

1. Add `src/plan.rs` with the type definitions and add `pub mod plan;` to `src/lib.rs` in the same edit so the new module compiles immediately.
2. Add `validate_plan_action` plus unit tests for the semantic invariants, including empty shell-step commands, empty shell-check commands, invalid tiers, duplicate IDs, and kind/steps rules.
3. Add `extract_plan_action` plus parser tests for no block, non-`plan-json` fences, malformed JSON, unclosed fences, and the explicit first-match-wins multiple-block contract.
4. Unignore and rewrite `tests/plan_engine.rs`, including renaming `creates_plan_engine_sqlite_schema` to `parses_done_action_with_empty_steps`.
5. Run `cargo fmt`.
6. Run `cargo test`.
7. Run `cargo clippy -- -D warnings`.
8. Review any hook-generated doc/stat updates under `docs/architecture/overview.md` and keep them if produced.

This order keeps the work incremental and actually testable:
- The module is wired into `lib.rs` before any unit tests are added, so `src/plan.rs` tests compile as soon as they exist.
- Validation lands before parser extraction, so parser tests can assert the full parse-and-validate contract instead of duplicating rule checks.
- Integration tests come last, after the module API and parser behavior are stable.

## 5. Risk Assessment

- Fence parsing behavior must be an explicit contract
  - The parser will scan top-to-bottom and use the first `plan-json` block it finds.
  - A later `plan-json` block will be ignored, and this behavior must be locked in with a test.
- Malformed block handling has two distinct failure modes
  - An unclosed `plan-json` fence is a parser error.
  - A closed fence with invalid JSON is a serde error.
  - Tests need both so `Ok(None)` remains reserved for “no block present.”
- Validation must cover both executable shell text locations
  - `PlanStepSpec::Shell.command` and `ShellCheckSpec.command` both need non-empty validation.
  - Missing the check-command path would accept unusable plan checks.
- The stale schema stub is a scope trap
  - `src/store.rs` already carries plan-run schema support.
  - PE-2 is parse/validate only, so the integration test file must be realigned to parser behavior instead of implying new schema work.
- Docs sync is a workflow risk, not a feature risk
  - No manual spec edit is expected for PE-2.
  - The implementation pass still needs to preserve any hook-generated architecture-stat diff tied to `src/` changes.
