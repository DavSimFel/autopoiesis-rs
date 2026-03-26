# Session 3 Plan: Validation Boundaries - Config Split

## 1. Files Read

### Repo guidance, standards, and validation flow
- `AGENTS.md`
- `CODE_STANDARD.md`
- `README.md`
- `docs/risks.md`
- `docs/architecture/overview.md`
- `docs/specs/restructure-plan.md` (config split section)
- `xtask/lint.sh`

### Config
- `Cargo.toml`
- `agents.toml`

### `src/` root
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/delegation.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/main.rs`
- `src/model_selection.rs`
- `src/plan.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/time.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

### `src/agent/`
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

### `src/gate/`
- `src/gate/budget.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/secret_patterns.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`

### `src/llm/`
- `src/llm/history_groups.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`

### `src/plan/`
- `src/plan/executor.rs`
- `src/plan/notify.rs`
- `src/plan/patch.rs`
- `src/plan/recovery.rs`
- `src/plan/runner.rs`

### `src/server/`
- `src/server/auth.rs`
- `src/server/http.rs`
- `src/server/mod.rs`
- `src/server/queue.rs`
- `src/server/ws.rs`

### `src/session/`
- `src/session/budget.rs`
- `src/session/delegation_hint.rs`
- `src/session/jsonl.rs`
- `src/session/mod.rs`
- `src/session/tests.rs`
- `src/session/trimming.rs`

### Audit note
- All `src/` files were inspected before writing this plan.
- `src/config.rs` and `src/subscription.rs` were then re-read line-by-line because they are the target validation boundaries for this session.

## 2. Exact Changes Per File

### `src/config.rs`
- Delete the monolith after its contents move into `src/config/`.
- Delete list:
- remove `src/config.rs`
- remove the inline `#[cfg(test)]` blocks from production config code once tests live in `src/config/tests.rs`

### `src/config/mod.rs`
- Create the facade module and keep it thin.
- Reexport the current public surface so existing `crate::config::*` call sites keep compiling:
- `Config`
- `ConfigError`
- `BudgetConfig`
- `QueueConfig`
- `ReadToolConfig`
- `SubscriptionsConfig`
- `ShellPolicy`
- `ShellDefaultAction`
- `ShellDefaultSeverity`
- `AgentsConfig`
- `AgentDefinition`
- `AgentTierConfig`
- `ModelsConfig`
- `ModelDefinition`
- `ModelRoute`
- `DomainsConfig`
- `DomainConfig`
- any new typed enums added for bounded config values
- the existing `pub(crate)` constants currently consumed as `crate::config::DEFAULT_*`
- `mod.rs` should only declare submodules, reexport symbols, and host `#[cfg(test)] mod tests;`.

### `src/config/runtime.rs`
- Move `Config`, `ConfigError`, and lightweight accessors here.
- Keep `active_agent_definition()` and `active_t1_config()` here because they are read-only runtime accessors, not parsing logic.
- Keep the default shell/queue constants here if other modules/tests continue importing them from `crate::config::*`.

### `src/config/load.rs`
- Move `Config::load()`, `Config::load_typed()`, `Config::from_file()`, and `Config::from_file_typed()` here.
- Keep the default runtime assembly, skills-dir resolution, skill catalog load, and env override application here.
- Extract small helpers so load-time precedence is explicit instead of being spread across one long function.
- Add `Policy:` comments on runtime precedence:
- tier-specific value wins over agent-level value
- agent-level value wins over hard-coded runtime defaults
- `AUTOPOIESIS_OPERATOR_KEY` wins over file config because env is the last override

### `src/config/spawn_runtime.rs`
- Move `with_spawned_child_runtime()` and `with_spawned_child_runtime_typed()` here.
- Keep spawned-child retargeting separate from file parsing.
- Reuse the same typed tier representation as the load path instead of reparsing raw strings.
- Add a `Policy:` comment that child-runtime retargeting only rewrites tier-sensitive runtime fields and intentionally preserves already validated shared state like shell/read/subscription/queue policy and the loaded skill catalog.

### `src/config/agents.rs`
- Move `AgentsConfig`, `AgentDefinition`, `AgentTierConfig`, `select_active_agent()`, and `validate_agent_identity()` here.
- Introduce a typed enum for agent tier because `t1 | t2 | t3` is a closed set and is still stringly typed in `src/config.rs`.
- Replace config-side `Option<String>` tier storage and `validate_spawn_tier(&str)`-style checks with typed parse/validation at the serde boundary where possible.
- Keep `model`, `base_url`, `system_prompt`, and `session_name` as strings because they are open-ended values.
- Keep `reasoning` / `reasoning_effort` stringly unless a shared repo-wide enum already exists; the current provider path forwards these values verbatim, so forcing a local enum without a settled vocabulary would be speculative.
- Add `Invariant:` comments:
- active-agent selection currently supports exactly one non-`default` entry
- agent identity must be a single lexical path segment because it is appended under `identity-templates/agents/`

### `src/config/models.rs`
- Move `ModelsConfig`, `ModelDefinition`, and `ModelRoute` here.
- Introduce a typed enum for `provider`; current repo usage is a closed config choice and is still represented as a raw string.
- Audit `cost_tier` before typing it:
- repo examples are inconsistent today: `cheap` appears in `README.md`/`agents.toml`, while test fixtures also use `low`, `medium`, and `high`
- if the vocabulary is normalized in this session, type it as an enum and fail closed on unknown values
- if the vocabulary is not normalized, leave it as an intentional string and document why it is not yet a safe enum candidate
- Keep `caps`, `requires`, and `prefer` as strings because they are catalog keys / capability labels, not a fixed closed set.

### `src/config/domains.rs`
- Move `DomainsConfig`, `DomainConfig`, and `validate_domain_context_extend()` here.
- Keep domain prompt extension validation isolated from agent selection and runtime assembly.
- Add an `Invariant:` comment that `context_extend` is a lexical safety check: it must remain under `identity-templates/` and contain only `Normal` path components.

### `src/config/policy.rs`
- Move `BudgetConfig`, `QueueConfig`, `ReadToolConfig`, `SubscriptionsConfig`, `ShellPolicy`, shell enums, defaults, and validation helpers here.
- Keep shell policy parsing fail-closed through serde + `TryFrom<String>`.
- Keep read/subscription validation here so policy validation remains separate from file schema and runtime assembly.
- Add a short `Policy:` comment near validation helpers explaining that zero/empty policy values are rejected here so later runtime code does not need to silently repair config.

### `src/config/file_schema.rs`
- Move `RuntimeFileConfig` and `AuthFileSection` here.
- Keep this module raw and schema-shaped: TOML decode only, no runtime mutation or precedence logic.

### `src/config/tests.rs`
- Move the current config tests here and regroup them by boundary:
- shell/read/subscription/queue policy defaults and validation
- load wrapper vs typed wrapper behavior
- active-agent and path-safety validation
- spawned-child runtime retargeting
- typed enum parsing and structured error variants
- Reuse shared helpers like `temp_toml_path()` and `assert_default_*()` instead of duplicating them across production modules.

### `src/subscription.rs`
- Re-verify it against the repository standard and keep it as the canonical validation shape.
- Expected outcome: no behavior change.
- If any edit is needed at all, keep it comment-only and limited to clarifying that `SubscriptionFilter::from_flags()` / `from_storage()` are fail-closed validation boundaries.
- Do not restructure this file during Session 3 unless the audit finds an actual validation drift.

### Required doc sync if implementation lands in the same merge
- `AGENTS.md`
- `README.md`
- `docs/architecture/overview.md`
- These files currently reference `src/config.rs`; deleting that path without updating the docs would violate the repo rule that docs stay synced with `src/` changes.

## 3. What Tests To Write

### `src/config/tests.rs`
- Add a module-split parity test: `Config::load*()` and `Config::with_spawned_child_runtime*()` still behave exactly as before after the file split.
- Add focused precedence tests:
- selected tier overrides agent-level values
- agent-level values override hard-coded defaults
- `AUTOPOIESIS_OPERATOR_KEY` overrides the file value
- Add typed enum parse tests for every bounded value converted in this session.
- Assertions:
- valid TOML values deserialize successfully
- invalid values fail closed with a structured `ConfigError` or serde parse failure
- Add tier-validation tests using the new config-side tier enum.
- Assertions:
- `t1`, `t2`, and `t3` are accepted
- unknown tier values are rejected before child-runtime assembly proceeds
- Add provider-validation tests if `provider` becomes typed.
- Assertions:
- `openai` is accepted
- unknown providers are rejected
- For `cost_tier`, choose one path and test it explicitly:
- if normalized and typed, test the accepted vocabulary and hard failure on unknown values
- if left as a string, add a regression that heterogeneous values round-trip unchanged and document that this field is intentionally untyped until vocabulary is settled
- Keep the existing path-safety regressions and make them more boundary-explicit:
- `identity='../tmp/prompt'` is rejected
- `context_extend='../prompt.md'` is rejected
- selected domains must exist and must provide `context_extend`
- Add a reexport smoke test that constructs representative types through `crate::config::*` paths so the facade proves the public module surface stayed intact.

### `src/subscription.rs`
- No new functional tests are required if the audit leaves the file unchanged.
- If a clarifying comment move accidentally changes code shape, rerun the existing subscription tests and keep:
- filter parse/render round-trips
- path normalization / readability checks
- store round-trip and timestamp refresh behavior

### End-to-end verification
- Run `cargo test`.
- Run `xtask/lint.sh`, which currently executes:
- `./xtask/lint_checks.sh`
- `cargo build --release`
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`
- `cargo test --features integration --test integration` when auth is present

## 4. Order Of Operations

1. Mechanically move `src/config.rs` to `src/config/mod.rs` first, with no semantic change, and delete `src/config.rs` in the same step so the old shape is actually removed.
2. Extract pure type modules next: `runtime.rs`, `policy.rs`, `agents.rs`, `models.rs`, `domains.rs`, and `file_schema.rs`. Keep `mod.rs` reexporting the existing surface after each extraction.
3. Extract `load.rs` and keep `Config::load*()` behavior byte-for-byte equivalent before touching any enum conversions.
4. Extract `spawn_runtime.rs` and keep spawned-child behavior equivalent before touching any enum conversions.
5. Move the existing config test coverage into `src/config/tests.rs` so production files stop carrying giant inline test blocks.
6. Convert remaining clearly bounded config values to enums one at a time, starting with tier, then provider.
7. Handle `cost_tier` only after auditing and normalizing the repo vocabulary. Do not silently invent an enum that breaks existing fixtures.
8. Add the requested `Policy:` and `Invariant:` comments on precedence and path-safety boundaries after the logic split is stable.
9. Audit `src/subscription.rs` last. If it still matches the standard, leave the implementation alone.
10. Update `AGENTS.md`, `README.md`, and `docs/architecture/overview.md` if the implementation merge actually deletes `src/config.rs`.
11. Finish with `cargo test` and `xtask/lint.sh`.

## 5. Risk Assessment

- Highest risk: accidentally breaking the public `crate::config::*` surface while moving code into `src/config/`.
- Mitigation: keep `mod.rs` as an exhaustive reexport facade and add a small reexport smoke test.

- High risk: changing precedence semantics while extracting `load.rs` and `spawn_runtime.rs`.
- Mitigation: keep the split mechanical first, then add focused precedence tests before any cleanup beyond file moves.

- High risk: typing the wrong fields.
- Mitigation: only convert values that are actually closed sets in config today; tier is clearly bounded, provider is likely bounded, but `reasoning_effort` and possibly `cost_tier` need explicit vocabulary decisions before typing.

- Medium risk: `cost_tier` vocabulary is inconsistent across the repo right now.
- Mitigation: normalize the accepted values first or leave the field intentionally stringly with a documented reason; do not half-convert it.

- Medium risk: path-safety checks could become weaker if `validate_agent_identity()` or `validate_domain_context_extend()` are rewritten during extraction.
- Mitigation: move those functions mechanically, add `Invariant:` comments, and preserve the existing traversal regressions.

- Medium risk: the file split lands cleanly but docs still point to `src/config.rs`.
- Mitigation: treat doc sync as part of the implementation merge, not as optional cleanup.

- Medium risk: Session 3 only types config-side tier/provider values while `SpawnRequest` / plan JSON still use raw strings elsewhere.
- Mitigation: keep the scope explicit: this session fixes the config validation boundary, not every tier string in the runtime. Call out any remaining non-config raw strings as a follow-up, not as hidden debt.

- Low risk: `src/subscription.rs` gets churned even though it already matches the desired validator shape.
- Mitigation: keep the subscription audit read-only unless a real mismatch appears.
