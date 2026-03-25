# Phase 2b Plan

## Implementation Decisions

- Route matching will follow the task text: when `task_kind` is present, scan `ModelRoute.requires` for that task kind.
- If more than one route matches the same `task_kind`, return an error fail-closed instead of depending on `HashMap` iteration order.
- Because the shipped config/docs currently present a different route story, the implementation slice will also update those examples so runtime behavior, config, and docs stay aligned.
- `spawn::spawn_child()` will take a `BudgetSnapshot` argument for preflight budget validation. `agent::spawn_child()` will compute that snapshot from the live parent `Session` and forward it.
- Pre-spawn budget validation will check session/day ceilings only. `max_tokens_per_turn` is intentionally excluded because a fresh child starts with zero turn tokens and this phase does not estimate future turn cost.

## 1. Files Read

### Docs and specs
- `docs/risks.md`
- `docs/roadmap.md`
- `docs/specs/identity-v2.md`
- `docs/vision.md`
- `docs/architecture/overview.md`

### Config
- `Cargo.toml`
- `agents.toml`

### Source
- `src/agent.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
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
- `src/principal.rs`
- `src/server.rs`
- `src/session.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### `src/model_selection.rs` (new)
- Add a small `ModelSelector` that borrows the model catalog, routes, and default catalog key.
- Implement fail-closed enabled checks with `enabled == Some(true)` as the only selectable state.
- Implement route selection by scanning `route.requires` for `task_kind`, then scan the matching route's `prefer` list in order and pick the first enabled catalog entry.
- If more than one route matches the same `task_kind`, error fail-closed instead of selecting an arbitrary route from a `HashMap`.
- If no route matches or `task_kind` is `None`, resolve the configured default catalog key and require it to exist and be enabled.
- If a route matches but none of its preferred catalog entries are enabled/present, return an error instead of silently falling back.
- Add unit tests in the module.

### `agents.toml`
- Update the shipped example route data so `requires` matches the Phase 2b task-kind routing semantics chosen above.
- Keep the example small but make sure the default model and the sample route remain internally consistent.

### `docs/specs/identity-v2.md`
- Update the T3 catalog/routing description and examples so they match the implemented Phase 2b route-matching semantics.

### `docs/vision.md`
- Update the model-repository section so it does not describe a different routing rule than the one being implemented.

### `src/spawn.rs`
- Extend `SpawnRequest` with `task_kind: Option<String>`.
- Extend `SpawnResult` with `resolved_model: String`; this will be the catalog key, not the provider model name.
- Add a pre-spawn budget helper that uses the existing `BudgetSnapshot` path from `Session::budget_snapshot()`.
- Change `spawn_child()` to receive `parent_budget: BudgetSnapshot`; budget computation stays with the caller, but the reject/allow logic stays in `spawn.rs`.
- Preflight budget behavior:
- Reject spawn when `max_tokens_per_session` is exhausted or exceeded for the parent session.
- Reject spawn when `max_tokens_per_day` is exhausted or exceeded for the parent session.
- Explicitly ignore `max_tokens_per_turn` in this preflight check and document that choice in code comments/tests.
- Do not add new token tracking or predictive spend estimation.
- Do not change provider/runtime model initialization.
- Model resolution behavior:
- If `model_override` is set, resolve that catalog key directly and require it to exist and be enabled.
- If both `model_override` and `task_kind` are present, `model_override` wins.
- If no override is set, use `task_kind` routing through `ModelSelector`.
- Persist `task_kind` and `resolved_model` into child session metadata.
- Keep existing metadata fields (`parent_session_id`, `task`, `model_override`, `reasoning_override`, `active_agent`, `default_model`) unless they become redundant.
- Return `SpawnResult { child_session_id, resolved_model }`.
- Update the local spawn test config helper so it includes a real `[models]` default plus catalog entries for every override/default used by the tests.
- Expand `spawn.rs` tests to cover routing, override failure, budget rejection, and metadata/result contents.

### `src/agent.rs`
- Update `agent::spawn_child()` to take the live parent `Session`, compute `session.budget_snapshot()`, and forward the resulting `BudgetSnapshot` into `spawn::spawn_child()`.
- Update every `SpawnRequest` construction in tests to include `task_kind`.
- Update spawn-related test configs so they populate `models.default` and `models.catalog` instead of relying on `ModelsConfig::default()`.
- Update spawn-related assertions to check `resolved_model` in the returned `SpawnResult`.
- Keep agent-loop runtime behavior unchanged; Phase 2b is config-time selection only.

### `src/lib.rs`
- Export the new `model_selection` module.

### `docs/architecture/overview.md`
- Update the current-state module map to include `model_selection.rs`.
- Note that child-session spawn now does fail-closed catalog selection and pre-spawn budget validation before queueing work.

### Intentionally unchanged in this slice
- `src/main.rs`
- `src/server.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/turn.rs`
- `src/gate/budget.rs`
- `src/config.rs`

Reason: Phase 2b is config-level model selection plus spawn preflight only. Runtime LLM wiring stays on `config.model` until Phase 3.

## 3. Tests To Write

### `src/model_selection.rs`
- `selects_first_enabled_preferred_model_for_matching_task_kind`
- `skips_disabled_or_missing_preferred_models_in_order`
- `multiple_routes_matching_same_task_kind_error_fail_closed`
- `matching_route_with_no_viable_preferred_model_errors_fail_closed`
- `unknown_task_kind_uses_enabled_default`
- `no_task_kind_uses_enabled_default`
- `missing_default_errors_fail_closed`
- `disabled_default_errors_fail_closed`
- `enabled_none_is_treated_as_disabled`

Assertions to pin:
- Route lookup is by scanning `route.requires`, not by route key.
- Multiple matching routes do not depend on `HashMap` iteration order.
- A matched route does not silently degrade to default when its `prefer` list is unusable.
- Only `Some(true)` is selectable.

### `src/spawn.rs`
- `spawn_child_uses_explicit_model_override_when_enabled`
- `spawn_child_override_wins_when_task_kind_is_also_present`
- `spawn_child_rejects_unknown_model_override`
- `spawn_child_rejects_disabled_model_override`
- `spawn_child_rejects_override_with_enabled_omitted`
- `spawn_child_uses_task_kind_route_when_no_override_is_present`
- `spawn_child_uses_default_when_task_kind_is_missing_or_unknown`
- `spawn_child_rejects_when_default_is_missing_or_disabled`
- `spawn_child_persists_task_kind_and_resolved_model_in_child_metadata`
- `spawn_child_returns_resolved_model_in_result`
- `spawn_child_rejects_when_parent_session_budget_is_exhausted`
- `spawn_child_rejects_when_parent_day_budget_is_exhausted`
- `spawn_child_does_not_reject_on_parent_turn_limit_only`
- `spawn_child_still_rejects_missing_parent`

Assertions to pin:
- Budget rejection happens before child session creation and before queue insertion.
- Budget rejection error names the exhausted ceiling and includes observed vs limit values.
- Successful spawn still creates the child session row and its initial queued task exactly once.
- `resolved_model` is the catalog key selected from `[models]`, not the provider model string.
- Explicit override precedence is preserved even when `task_kind` would match a different route.
- `enabled = None` is rejected on the explicit override branch, not just on route/default selection.

### `src/agent.rs`
- Update the existing spawn wrapper test to assert the new request shape and `resolved_model`.
- If the wrapper signature changes to accept `&Session` or `BudgetSnapshot`, add one smoke test that the wrapper passes through the resolved catalog key unchanged.

## 4. Order Of Operations

1. Add `src/model_selection.rs` with unit tests and export it from `src/lib.rs`.
2. Run targeted tests for the new selector module.
3. Update the shipped config/docs examples (`agents.toml`, `docs/specs/identity-v2.md`, `docs/vision.md`) so the routing semantics are aligned before runtime code lands.
4. Update the existing spawn/agent test config builders so they contain real model catalog/default entries before fail-closed selection is introduced.
5. In one pass, change `SpawnRequest`, `SpawnResult`, `spawn::spawn_child()`, `agent::spawn_child()`, the model-selection policy wiring, and every request/result constructor and assertion together so the tree never sits in a half-updated or behavior-divergent state.
6. Add the `BudgetSnapshot` preflight helper and the session/day reject plus turn-limit-ignored tests.
7. Update `docs/architecture/overview.md` to reflect the new current-state module and spawn behavior.
8. Run the full required validation set:
- `cargo fmt --check`
- `cargo test`
- `cargo clippy -- -D warnings`
- `cargo build --release`

## 5. Risk Assessment

- `ModelSelector` API shape still has one real tension: the spec asks for `select_model(...) -> &ModelDefinition`, but spawn also needs the selected catalog key for `resolved_model`. I will keep the public selector API simple and add an internal helper that preserves both key and definition, rather than re-looking up by provider model name.
- Matching `task_kind` through `route.requires` means route ambiguity is now possible. The selector needs a deterministic fail-closed rule for multiple matches or tests will be flaky and behavior will depend on `HashMap` iteration.
- Pre-spawn budget validation can only reject already-exhausted parent budgets with current infrastructure. It will not estimate future token spend and it will not aggregate spend across child sessions, which matches the stated constraint of “no new budget tracking.”
- Passing a `BudgetSnapshot` into `spawn::spawn_child()` changes the spawn API surface and requires updating the wrapper/tests in the same patch. Splitting that work would leave the tree uncompilable.
- `max_tokens_per_turn` is intentionally not part of the pre-spawn gate. That choice is defensible for a fresh child session, but it must be documented and tested so it does not get “fixed” later into a contradictory behavior.
- Treating `enabled = None` as disabled is intentionally fail-closed, but it can break legacy catalog entries that omitted `enabled`. The selector tests should make that behavior explicit so it does not drift later.
