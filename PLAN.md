# PLAN

## 1. Files read

From the prior planning pass, the files already read for this task were:

- `docs/roadmap.md`
- `docs/specs/identity-v2.md`
- `docs/vision.md`
- `agents.toml`
- `Cargo.toml`
- every Rust source file under `src/`

That includes the modules directly involved here:

- `src/agent.rs`
- `src/config.rs`
- `src/context.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/turn.rs`
- supporting model/config/session code under `src/` that these modules depend on

## 2. Exact changes per file

### `src/skills.rs`

- Add `Serialize`/`Deserialize` derives to `SkillDefinition` so resolved skills can be snapshotted into child metadata at spawn time.
- Add a fail-closed helper that resolves an ordered list of requested skill names into full `SkillDefinition` values from `SkillCatalog`.
- The helper will:
  - reject unknown names with an error
  - reject duplicate names with an error instead of silently deduping or loading twice
  - preserve caller order for valid unique names
- Add a helper to sum `token_estimate` across the resolved skills.
- Keep `SkillCatalog::browse()` behavior unchanged for T1/T2 summary injection.

### `src/context.rs`

- Add `SkillLoader` as a new `ContextSource`.
- `SkillLoader` will accept fully resolved `SkillDefinition` values and implement `ContextSource::assemble()` by producing a deterministic system-message fragment for the requested skills in request order.
- Exact rendered block per skill:

```text
Skill: {name}
{instructions}
```

- `SkillLoader` will stay within the repo’s context-source composition model, but it will **not** own the merge into the first system message. Its responsibility is to produce the fragment via the `ContextSource` abstraction.
- `SkillContext` remains unchanged and continues to provide browse summaries for T1/T2.

### `src/spawn.rs`

- Extend `SpawnRequest` with:
  - `skills: Vec<String>`
  - `skill_token_budget: Option<u64>`
- Make `SpawnRequest.skills` deserialize backward-compatibly with `#[serde(default)]` so existing request payloads that omit the new field resolve to `[]` instead of failing.
- Enforce these validations before child spawn:
  - non-empty `skills` on any non-`t3` spawn is an error
  - duplicate skill names are an error
  - unknown skill names are an error
  - total `token_estimate` across requested skills must be `<= resolved_budget`
  - required skill capabilities must be satisfied by the resolved model
- Resolve budget as:
  - `SpawnRequest.skill_token_budget.unwrap_or(4096)`
- Resolve skills from the catalog during spawn, then persist the resolved full `Vec<SkillDefinition>` into child metadata.
- Do not persist only names. Persisting full definitions is what makes skill loading a spawn-time snapshot instead of a runtime catalog lookup.
- Replace any duplicated child metadata type/parser split between spawn/agent with one shared metadata struct in this module or in a small shared module that both sides can depend on without a module cycle.
- Make the persisted metadata backward-compatible by defaulting the new `skills` field to empty on deserialize so old queued child rows still load.

### `src/turn.rs`

- Keep `build_turn_for_config()` unchanged as the public ordinary-turn entrypoint.
- Extract a small shared lower-level turn-assembly helper that both ordinary turn construction and spawned-child turn construction can call. This avoids high-level builder duplication without requiring the spawned-child path to “build normally and then strip” skill summaries.
- Ordinary path:
  - continues to assemble the current context sources, including `SkillContext` where it already applies
- Spawned-child path:
  - uses the shared lower-level assembly helper with explicit parameters so spawned `t3` children do **not** attach `SkillContext` browse summaries
  - for spawned `t3`, attaches `SkillLoader` as a context source, collects its rendered fragment, and merges that fragment into the **first** system message before the turn is finalized
  - does not rely on a separate later system message, because the current LLM request builder only treats the first system message as `instructions`
- Ordinary non-spawned `t3` turns remain on the existing `build_turn_for_config()` path only.

### `src/agent.rs`

- Stop using any duplicated ad hoc child-metadata parsing. Use the shared metadata struct introduced for spawn/drain coordination.
- Parse the persisted resolved skills from child metadata when draining a spawned child.
- Call the new spawned-child turn helper in `turn.rs` for spawned children.
- For spawned `t3` children, pass the persisted resolved skills into that helper.
- Do not re-open the catalog to resolve skills again at drain time.
- Existing T1/T2 behavior stays unchanged.

### `src/config.rs`

- No functional changes planned unless a small helper is needed to read model capabilities cleanly from existing config/model definitions.
- Do not add a new config surface for budget because this phase chose the `SpawnRequest` budget option, with defaulting at spawn validation time.

### `agents.toml`

- No changes planned.

### `Cargo.toml`

- No dependency changes planned.

## 3. What tests to write

### `src/skills.rs`

- `resolve_requested_skills_preserves_order`
  - resolving `["a", "b"]` returns skills in exactly that order
- `resolve_requested_skills_rejects_unknown_name`
- `resolve_requested_skills_rejects_duplicate_name`
- `sum_skill_token_estimates_matches_requested_set`

### `src/context.rs`

- `skill_loader_implements_context_source_and_assembles_single_skill_fragment`
- `skill_loader_assembles_multiple_skills_in_request_order`
- `skill_loader_assembled_fragment_matches_expected_format_exactly`

### `src/spawn.rs`

- `spawn_request_deserializes_missing_skills_as_empty`
- `spawn_rejects_skills_for_non_t3_child`
- `spawn_rejects_unknown_skill_name`
- `spawn_rejects_duplicate_skill_name`
- `spawn_rejects_when_skill_tokens_exceed_explicit_budget`
- `spawn_rejects_when_skill_tokens_exceed_default_budget_4096`
- `spawn_rejects_when_resolved_model_lacks_required_caps`
- `spawn_persists_resolved_skill_definitions_in_child_metadata`
- `spawned_child_metadata_deserializes_old_rows_without_skills_field`
  - construct old-format metadata JSON and prove it deserializes with `skills == []`

### `src/turn.rs`

- `spawned_t3_turn_merges_skill_loader_fragment_into_first_system_message`
- `spawned_t3_turn_does_not_emit_skill_loader_as_later_system_message`
- `spawned_t3_turn_does_not_include_available_skills_summary_block`
- `spawned_t3_turn_keeps_t3_toolset_and_guard_behavior`
- `ordinary_non_spawned_t3_turn_is_unchanged`

### `src/agent.rs`

- `drain_spawned_t3_uses_persisted_skill_snapshot_not_catalog_lookup`
  - spawn a child with a resolved skill snapshot
  - then mutate or remove the underlying on-disk/catalog source before drain
  - assert the child still receives the originally persisted instructions
- `drain_spawned_t3_loads_full_skill_instructions_not_browse_summary`
- `drain_spawned_t2_toolset_remains_read_file_only`
- `drain_old_spawned_child_without_skills_metadata_still_runs`
  - cover the real queued-child compatibility path, not just raw metadata deserialize

### Cross-module invariants

- `SkillLoader` stays inside the `ContextSource` abstraction while spawned-child turn assembly still guarantees that full skill text ends up inside the first system message.
- Requested skill order is preserved end-to-end from `SpawnRequest.skills` through persisted metadata through injected prompt text.
- Missing `skills` on an incoming `SpawnRequest` deserializes to `[]` for backward compatibility.
- Duplicate skill names fail closed before spawn.
- Skills are snapshotted at spawn time; draining a queued child must not depend on the current catalog contents.
- Ordinary non-spawned `t3` turns remain on the current path and do not pick up spawned-child-only behavior.

## 4. Order of operations

1. Update `src/skills.rs` first:
   - add serde derives
   - add ordered resolution, duplicate rejection, and token-sum helpers
   - add unit tests for order, unknown names, duplicates, and sums
   - this is isolated and keeps the tree green

2. Add `SkillLoader` in `src/context.rs` as a `ContextSource` that assembles deterministic skill fragments.
   - do not wire it into turn construction yet
   - `SkillContext` remains intact, so existing callers keep working

3. Extend `SpawnRequest` and introduce the shared child metadata struct in `src/spawn.rs`.
   - in the same step, add `#[serde(default)]` for `SpawnRequest.skills`
   - in the same step, update every `SpawnRequest` constructor/call site that must now populate `skills`
   - also add `#[serde(default)]` on persisted child-metadata `skills`
   - this avoids an intermediate compile break from changing the request type without updating its callers

4. Add spawn-time validation in `src/spawn.rs`.
   - reject non-`t3` skills
   - reject duplicates/unknown names
   - apply default budget `4096`
   - validate required caps against the resolved model
   - persist resolved full skills into child metadata
   - add spawn-layer tests, including missing-request-field compatibility and old-metadata compatibility coverage

5. Refactor `src/turn.rs` to introduce the shared lower-level assembly helper.
   - keep `build_turn_for_config()` as the ordinary wrapper
   - add the spawned-child helper that reuses shared assembly without calling the high-level ordinary builder
   - make that helper merge the `SkillLoader` fragment into the first system message and omit `SkillContext` summaries for spawned `t3`
   - add focused turn-builder tests for first-system-message placement, no later-system-message fallback, no browse summary, and unchanged ordinary non-spawned `t3`

6. Update `src/agent.rs` to consume the shared child metadata struct and call the new spawned-child helper.
   - parse persisted resolved skills from metadata
   - pass them only on spawned `t3` drains
   - keep T1/T2 behavior unchanged

7. Add drain-path tests in `src/agent.rs`.
   - snapshot semantics: queued child still uses persisted skills after catalog mutation/removal
   - compatibility semantics: old queued child rows without `skills` still drain
   - behavioral semantics: spawned `t3` gets full skills, spawned `t2` remains `read_file` only

8. Run verification:
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `cargo test`
   - `cargo build --release`
- If any docs are touched by implementation, sync them last after code/tests are green.

This order keeps the code compiling after each step and avoids introducing a request-shape or metadata-shape change before all direct consumers are updated in the same increment.

## 5. Risk assessment

- The highest correctness risk is prompt placement. Full skill text must be merged into the first system message by the spawned-child turn builder; emitting a separate later system message would silently fail the intended behavior with the current LLM request builder.
- The next rollout risks are backward compatibility for both incoming spawn requests and old queued child metadata. Missing `#[serde(default)]` on either path could break existing inputs during deployment.
- The biggest architectural risk is accidentally re-resolving skills from the catalog during drain. That would violate the spawn-time snapshot requirement and make queued child behavior depend on later file edits.
- Persisting full `SkillDefinition` values increases metadata size in the queue/store path; the implementation should keep the stored shape minimal and only include fields needed at drain time.
- `token_estimate` enforcement is heuristic. The plan treats it as a guardrail, not an exact tokenizer guarantee.
- Duplicate skill names are treated as invalid input to keep budget accounting, capability checks, and prompt assembly deterministic.
