# Session 3 Plan: Validation Boundaries / Config Split

## 1. Files Read

- `AGENTS.md`
- `CODE_STANDARD.md`
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`
- `xtask/lint.sh`
- every file under `src/`, with focused review of:
  - `src/config.rs`
  - `src/subscription.rs`
  - every in-tree `crate::config::*` consumer found during the full read

## 2. Exact Changes Per File

### `src/config.rs`

- Keep this file as the module root only during extraction.
- Replace inline definitions with `mod ...` declarations, reexports, and thin facade glue as code moves into `src/config/*.rs`.
- Remove moved definitions in the same patch that introduces their new owning submodule. Do not leave duplicate definitions behind.
- While `src/config.rs` is still the active root, create `src/config/tests.rs`, move the existing facade-level / cross-module tests out of `src/config.rs` into that file, and add `#[cfg(test)] mod tests;` in the same patch.
- Final state: once only module declarations, reexports, `#[cfg(test)] mod tests;`, and facade glue remain, move that glue verbatim to `src/config/mod.rs` and delete `src/config.rs` in the same patch.
- Never leave both `src/config.rs` and `src/config/mod.rs` present as active roots at the same time.

### `src/config/mod.rs`

- Final module root for the split config module.
- Declare:
  - `mod runtime;`
  - `mod load;`
  - `mod spawn_runtime;`
  - `mod agents;`
  - `mod models;`
  - `mod domains;`
  - `mod policy;`
  - `mod file_schema;`
  - `#[cfg(test)] mod tests;`
- Keep this file as a thin facade only. No moved business logic should remain here after the split.
- Preserve this explicit facade inventory exactly, at the same public or crate-visible paths it has today:
  - `Config`
  - `ConfigError`
  - `BudgetConfig`
  - `QueueConfig`
  - `ReadToolConfig`
  - `SubscriptionsConfig`
  - `AgentsConfig`
  - `AgentDefinition`
  - `AgentTierConfig`
  - `ModelsConfig`
  - `ModelDefinition`
  - `ModelRoute`
  - `DomainsConfig`
  - `DomainConfig`
  - `ShellDefaultAction`
  - `ShellDefaultSeverity`
  - `ShellPolicy`
  - `load`
  - `load_typed`
  - `from_file`
  - `from_file_typed`
  - `with_spawned_child_runtime`
  - `with_spawned_child_runtime_typed`
  - `DEFAULT_SHELL_MAX_OUTPUT_BYTES`
  - `DEFAULT_SHELL_MAX_TIMEOUT_MS`
  - `DEFAULT_STALE_PROCESSING_TIMEOUT_SECS`
- Preserve current visibilities exactly:
  - public items stay public through `pub use`
  - crate-visible items stay crate-visible through `pub(crate) use`
  - specifically keep the three `DEFAULT_*` constants at `pub(crate)`, not `pub`
- Add top-level comments:
  - `Invariant:` summary of precedence ownership across `load.rs` and `spawn_runtime.rs`
  - `Policy:` summary of path-safety ownership across `agents.rs`, `domains.rs`, and `load.rs`

### `src/config/runtime.rs`

- Move runtime-facing data structures and simple accessors here:
  - `Config`
  - queue/read/subscription/runtime structs
  - crate-visible `DEFAULT_*` constants
  - other runtime-only helpers that do not own load/validation policy
- Keep inherent impls here when they are just accessors or resolved-runtime behavior.
- Prefer `pub(super)` or private helpers over widening visibility for convenience.

### `src/config/load.rs`

- Move file loading, env application, merge/default resolution, and final validation orchestration here.
- Make this the owning module for `ConfigError`, since load/merge/validation failures originate here and the type belongs with the load/validation boundary.
- Keep precedence logic co-located here instead of scattering it across schema modules.
- Add `Invariant:` comments on:
  - env-over-file precedence where it exists today
  - file-over-default precedence
  - validation occurring after the fully merged config is assembled
- Add `Policy:` comment on `skills_dir` path handling:
  - relative values resolve against the config file directory
  - absolute values remain absolute
  - path handling must not silently normalize traversal into a different trusted root

### `src/config/spawn_runtime.rs`

- Move spawned-child runtime overlay/build logic and `validate_spawn_tier()` here.
- Keep child-runtime precedence logic here.
- Add `Invariant:` comments documenting the current order:
  - explicit override
  - child tier
  - agent defaults
  - parent runtime fallback
- Keep helper-private tests here when they depend on private builders or validators.

### `src/config/agents.rs`

- Move agent/tier schema and selection helpers here.
- Keep `validate_agent_identity()` here.
- Add `Policy:` comment that agent identity is a logical path segment and must reject empty, dot, dot-dot, or separator-containing values.

### `src/config/models.rs`

- Move provider/model schema and related helpers here.
- Keep this module focused on model metadata, not loading or policy.

### `src/config/domains.rs`

- Move domain schema and `validate_domain_context_extend()` here.
- Add `Policy:` comment that `context_extend` must stay under `identity-templates/` and only use normal path components after that root.

### `src/config/policy.rs`

- Move shell/read/subscription validation and policy enums/structs here.
- Keep fail-closed validation boundaries here instead of mixing them into file I/O.
- Add `Policy:` comments on:
  - deny/approve/allow expectations inherited from repo rules
  - validation rejecting unsafe or ambiguous config instead of normalizing it
- Keep private validation-helper tests here when they need direct access.

### `src/config/file_schema.rs`

- Move serde/TOML input-only structs and parsing-schema helpers here.
- Keep this module free of runtime merge policy and free of spawned-child overlay logic.

### `src/config/tests.rs`

- Create this file while `src/config.rs` is still the active root.
- Move the existing facade-level / cross-module tests here before the `src/config.rs` -> `src/config/mod.rs` cutover.
- Keep only facade-level and cross-module tests here.
- This file must compile using the `crate::config` facade surface and intentionally exposed items only.
- Do not move helper-private assertions here if doing so would require widening production visibility.
- If a current top-level test mixes facade behavior and private-helper behavior, split it:
  - keep the facade assertion here
  - move the helper-private assertion into the owning submodule

### `src/subscription.rs`

- Verification-only by default. No planned behavior change.
- Audit against this pass/fail checklist derived from `CODE_STANDARD.md` and repo rules:
  - no `.unwrap()` in non-test code
  - error handling still uses the repo’s expected style with context where needed
  - logging still follows repo severity guidance
  - policy/state mutation/I/O boundaries remain readable and not newly mixed together
  - public types and async entrypoints still have the expected docs/comments
  - non-obvious invariants or policy boundaries still have comments where needed
- If every checklist item passes, leave `src/subscription.rs` untouched and explicitly record that the audit passed with no code change.
- Only edit `src/subscription.rs` if this audit finds a real standard mismatch.

### Downstream Consumer Sweep

- Verify that the facade split does not require consumer-path churn. Expected no code changes outside `src/config/**` unless an import truly breaks.
- Explicit verification targets include:
  - `src/lib.rs`
  - `src/main.rs`
  - `src/cli.rs`
  - `src/model_selection.rs`
  - `src/turn.rs`
  - `src/context.rs`
  - `src/skills.rs`
  - `src/server/auth.rs`
  - `src/server/http.rs`
  - `src/server/mod.rs`
  - `src/server/queue.rs`
  - `src/server/ws.rs`
  - any tests importing `crate::config::*`
- Target state is no consumer-path churn. If a consumer needs a direct submodule import, treat that as a facade regression.

### Docs / Metadata

- Review docs for both:
  - literal `src/config.rs` references
  - higher-level prose that describes config loading/validation as a single-file responsibility
- This doc sweep is not limited to grep hits for `src/config.rs`; it also includes architecture/risk/overview prose that would become stale if config ownership, validation boundaries, or public API descriptions still imply a single-file module.
- Update any stale architecture/risk/overview references in the same change set.
- Keep the completion step in the implementation plan:
  - `openclaw system event --text 'Session 3 done: config split' --mode now`

## 3. What Tests To Write

### Preserve Existing Coverage

- Move or split existing tests; do not silently drop assertions.
- Keep helper-private tests in the module that owns the helper.

### Facade / API Coverage

- In `src/config/tests.rs`, add or preserve tests that exercise the explicit `crate::config` facade inventory from `src/config/mod.rs`.
- Verify the split keeps the same facade names reachable through the same public paths.
- Keep wrapper-equivalence tests for any top-level config helpers that remain part of the facade.

### Visibility Coverage

- Add in-crate tests that `crate::config::DEFAULT_*` constants remain reachable where `pub(crate)` access is expected.
- Do not claim this proves they are not `pub`; treat “not widened beyond `pub(crate)`” as a code-review invariant checked against the explicit reexport inventory in `src/config/mod.rs`.

### Precedence Coverage

- Preserve or add tests for load precedence:
  - env-over-file behavior where it exists today
  - file-over-default behavior
- Preserve or add tests for spawned-child precedence:
  - reasoning override wins over child tier / agent / parent
  - child `provider_model` replaces the parent model
  - child tier `base_url`, `system_prompt`, and `session_name` keep the current fallback order
  - identity retargeting behavior for child tiers stays unchanged

### Path-Safety Coverage

- In `src/config/agents.rs`, keep or add helper-private tests that `validate_agent_identity()` rejects:
  - empty string
  - `.`
  - `..`
  - separator-containing values
- In `src/config/domains.rs`, keep or add helper-private tests that `validate_domain_context_extend()` rejects:
  - absolute paths
  - traversal paths
  - roots outside `identity-templates/`
  - any non-`Normal` component after the allowed root
- In `src/config/load.rs`, keep or add tests that:
  - relative `skills_dir` resolves against the config file directory
  - absolute `skills_dir` stays absolute

### Validation Helper Coverage

- Keep or add private-helper tests in:
  - `src/config/policy.rs` for read/subscription validation helpers
  - `src/config/spawn_runtime.rs` for `validate_spawn_tier()`
  - any other submodule-local validator that should remain private

### `subscription.rs`

- No new `subscription.rs` tests unless the audit reveals an actual mismatch that requires a regression test.

## 4. Order Of Operations

1. Create `src/config/` submodules and move the most self-contained code first:
   - `file_schema`
   - `models`
   - `domains`
   - `agents`
   - `policy`
   - `runtime`
   Keep `src/config.rs` as the root during this phase.
2. As each responsibility moves, remove the old inline definition in the same patch and keep the facade wired through `src/config.rs`. Do not leave duplicate definitions in both places.
3. While `src/config.rs` is still the active root, create `src/config/tests.rs`, move every existing facade-level / cross-module test into it, and add `#[cfg(test)] mod tests;` in that same patch so no top-level facade assertion is lost or duplicated during the later cutover.
4. Move helper-private tests with their owning modules as those moves happen. Do not widen production visibility just to preserve old test locations.
5. Move `load` and `spawn_runtime` last, because they depend on the extracted schema/runtime/policy pieces and carry the key precedence comments.
6. Once `src/config.rs` contains only module declarations, reexports, `#[cfg(test)] mod tests;`, and facade glue, move that file verbatim to `src/config/mod.rs` and delete `src/config.rs` in the same patch.
7. Run the downstream consumer sweep and the explicit `src/subscription.rs` standards audit.
8. Run the broader docs sync pass:
   - literal filename/path references
   - conceptual ownership and architecture prose
9. Run the full required verification suite:
   - `cargo build --release`
   - `cargo test`
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `xtask/lint.sh`
   - `cargo test --features integration` if credentials are configured
10. Only after the full verification suite passes, run:
   - `openclaw system event --text 'Session 3 done: config split' --mode now`

## 5. Risk Assessment

- Highest risk: precedence drift while moving load/spawn code.
  - Mitigation: keep precedence logic co-located, document it with `Invariant:` comments, and preserve/add explicit precedence tests.
- High risk: root-module cutover from `src/config.rs` to `src/config/mod.rs`.
  - Mitigation: keep `src/config.rs` as the root until it is only facade glue, move facade tests out before the cutover, then do a same-patch move/delete with no dual-root state.
- High risk: facade visibility drift.
  - Mitigation: use the explicit symbol inventory in `src/config/mod.rs`, preserve `pub(crate)` constant visibility, and verify against that inventory during implementation and review.
- Medium risk: helper-private tests forcing visibility widening.
  - Mitigation: move those tests into the owning submodule instead of changing production visibility.
- Medium risk: docs drift even when no literal filename reference changes.
  - Mitigation: review architecture/risk prose for responsibility-boundary drift, not just string matches.
- Low risk: unnecessary churn in `src/subscription.rs`.
  - Mitigation: use the explicit audit checklist and leave the file untouched unless a concrete standard mismatch is found.
