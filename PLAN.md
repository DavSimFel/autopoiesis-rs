# Phase 3c Plan

## 1. Files read

Config and docs:
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/roadmap.md`
- `docs/specs/identity-v2.md`
- `docs/vision.md`
- `docs/architecture/overview.md`

Source files:
- `src/agent.rs`
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
- `src/principal.rs`
- `src/read_tool.rs`
- `src/server.rs`
- `src/session.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact changes per file

### `src/spawn.rs`
- Extend `SpawnRequest` with `tier: Option<String>`.
- Add a small internal serde struct for child-session metadata instead of hand-assembling raw JSON.
- Resolve spawn tier once during `spawn_child()`:
  - accept only `t1`, `t2`, `t3` when the field is present
  - treat `None` as “inherit the current active tier” for backward compatibility with existing constructors
- Persist the resolved tier in child metadata together with the existing spawn fields.
- Never recompute the fallback later; runtime should read the already-resolved concrete tier from metadata.
- Keep persisting both `resolved_model` and `resolved_provider_model`; the child runtime must use `resolved_provider_model` for `config.model`.
- Add `SpawnDrainResult` with the spawned child id, resolved model key, and `last_assistant_response: Option<String>`; do not synthesize fallback text in this API.
- Add helpers to parse child metadata, validate the persisted tier again on readback, and extract the concrete runtime inputs needed by `spawn_and_drain()`.

### `src/config.rs`
- Add one runtime override helper on `Config` for spawned children, something like `with_spawned_child_runtime(...) -> Result<Config>`.
- That helper will:
  - clone the parent config
  - mutate the active agent’s `tier`
  - recompute `identity_files`
  - re-append the currently selected domain pack files after rebuilding the tier-specific identity file list
  - recompute tier-derived `reasoning_effort`, `base_url`, `system_prompt`, and `session_name`
  - set `config.model` from the stored `resolved_provider_model`
  - let an explicit `reasoning_override` win over tier defaults
- Preserve the current loader rule that `t3` uses T2-style identity/no-personality config, because there is no `[agents.*.t3]` table today.

### `src/store.rs`
- Add a read helper for session metadata by session id so runtime code can reconstruct child tier/model from persisted session metadata instead of trusting only the original request object.
- Keep queue/session semantics unchanged.

### `src/agent.rs`
- Add the new synchronous child primitive, `spawn_and_drain(...)`.
- Concrete behavior:
  - load the parent session from disk and compute the live budget snapshot
  - call `spawn_child()`
  - fetch and parse the child metadata from `Store`
  - build the overridden child `Config`
  - build the child `Turn` with `build_turn_for_config(&child_config)`
  - open/load the child `Session`
  - drain the child queue to completion with the existing `drain_queue()`
  - return `SpawnDrainResult { last_assistant_response: latest_assistant_response(...) }`
- Keep `drain_queue()` and `run_agent_loop()` signatures unchanged.
- Add a small private generic helper behind the public API that takes the resolved child `Config` plus a config-aware fake-provider hook, so unit tests can inject a fake provider, avoid network/auth, and assert which model/reasoning inputs the child path actually used.
- Re-export `SpawnDrainResult` next to `SpawnRequest`/`SpawnResult` if that keeps the API surface consistent.

### `src/turn.rs`
- No new production branching should be needed if the child config override is correct.
- Add or extend tests that build a turn from a spawned-child override config and assert:
  - T1 child => `execute` plus T1 delegation config
  - T2 child => only `read_file`
  - T2 child => shell lookup fails closed
  - T3 child => `execute` plus shell guards

### `docs/architecture/overview.md`
- Update the current-state description so it reflects:
  - child session metadata now stores tier
  - child runtime config is reconstructed from metadata
  - `spawn_and_drain()` is the sync child-execution primitive

### Files intentionally unchanged in this phase
- `src/main.rs`
- `src/server.rs`
- `src/session.rs`
- `src/read_tool.rs`
- `src/tool.rs`

Reason: Phase 3c can be implemented below those layers if the new primitive owns child config reconstruction and then reuses the existing drain path unchanged.

## 3. Tests to write

### Spawn metadata and validation
- `spawn_child_persists_resolved_tier_in_child_metadata`
  - assert metadata contains the concrete tier string, not just the request payload
- `spawn_child_rejects_invalid_tier_before_child_creation`
  - assert no child session row is created on bad tier input
- `spawn_child_defaults_missing_tier_to_parent_active_tier_for_backward_compat`
  - assert `None` resolves once, is persisted as a concrete tier, and does not rely on later recomputation

### Config override correctness
- `spawned_child_config_t1_uses_t1_identity_files_and_selected_domains`
  - assert `agent.md` is present again for T1 children and domain packs are retained
- `spawned_child_config_t2_uses_t2_identity_files_and_reasoning_defaults`
  - assert no `agent.md`, only constitution/context, plus any domain packs
- `spawned_child_config_t3_uses_t2_identity_files_but_resolved_provider_model`
  - assert `config.model` is the provider model string from metadata, not the catalog key
- `spawned_child_config_reasoning_override_wins_over_tier_reasoning`

### Turn/tool invariants
- `build_turn_for_spawned_t1_child_contains_execute_and_delegation`
- `build_turn_for_spawned_t2_child_contains_only_read_file`
  - assert tool list is exactly `["read_file"]`
- `build_turn_for_spawned_t2_child_has_no_shell_tool`
  - assert executing `execute` returns `tool 'execute' not found`
- `build_turn_for_spawned_t3_child_contains_execute`

### End-to-end child drain behavior
- `spawn_and_drain_runs_child_queue_and_returns_last_assistant_response`
  - use a static/fake provider
  - assert child queue is emptied/processed
  - assert returned assistant text matches the final child assistant message
- `spawn_and_drain_returns_none_when_child_produces_no_assistant_response`
  - assert the new API returns `None` rather than a synthetic fallback string
- `spawn_and_drain_enqueues_parent_completion_after_child_finishes`
  - assert the parent queue receives the handoff message from `enqueue_child_completion()`
- `spawn_and_drain_uses_metadata_reconstructed_tier_not_parent_turn`
  - spawn as T2 from a T1 parent and assert the provider sees only the `read_file` tool definition
- `spawn_and_drain_uses_resolved_provider_model_not_parent_config_model`
  - assert the fake provider path observes the child model from metadata, not the parent config’s model
- `spawn_and_drain_errors_on_missing_child_metadata`
  - assert the runtime fails loudly if the child session row cannot be reloaded
- `spawn_and_drain_errors_on_malformed_child_metadata`
  - assert invalid JSON metadata is surfaced as an error instead of silently falling back
- `spawn_and_drain_errors_on_invalid_child_tier_in_metadata`
  - assert well-formed metadata with an unsupported persisted tier is rejected before turn construction

## 4. Order of operations

1. Refactor `src/spawn.rs` first: add tier resolution, typed child metadata, and update every `SpawnRequest` constructor/fixture in `src/spawn.rs` and `src/agent.rs` tests so the tree keeps compiling immediately.
2. Add `Store` metadata read access in `src/store.rs` with unit tests.
3. Add the config override helper in `src/config.rs` and lock down the precedence rules with unit tests.
4. Add/extend `src/turn.rs` tests to prove the overridden config produces the correct tool surface.
5. Add `spawn_and_drain()` in `src/agent.rs` using the already-tested metadata + config helper pieces.
6. Add the end-to-end fake-provider tests for synchronous child execution and parent handoff.
7. Update `docs/architecture/overview.md`.

This order keeps failures local, preserves the existing queue/turn signatures, and avoids debugging the full child drain path before tier resolution is correct.

## 5. Risk assessment

- Highest risk: confusing `resolved_model` with `resolved_provider_model`. `config.model` must be set to the provider model string, or the child will build the right turn but call the wrong LLM model.
- Highest risk: tier ambiguity. I will resolve `tier=None` once for backward compatibility, persist the concrete resolved tier in metadata, and never recompute that fallback later.
- Medium risk: persisted metadata can be syntactically valid but semantically invalid. The readback path must re-validate the stored tier instead of assuming spawn-time validation was enough.
- Medium risk: T3 has no dedicated config table. The safest behavior is to preserve the current loader rule: T3 reuses T2 identity/non-personality fields, then overrides only the model/reasoning inputs needed for execution.
- Medium risk: domain packs are easy to drop when rebuilding `identity_files`. The override helper must rebuild the base tier file list and then append `domains.selected` in the same order `Config::load()` uses today.
- Medium risk: provider construction for the new primitive. To keep tests fast and deterministic, the public helper should call a private generic helper so unit tests do not hit auth/network.
- Medium risk: known repo hazards still apply, especially `Session::append` non-atomicity (`docs/risks.md`). This phase should avoid changing queue-claiming, session persistence ordering, or `drain_queue()` semantics outside the new child wrapper.
