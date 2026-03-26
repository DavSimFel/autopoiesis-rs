# PLAN

## 1. Files Read

- `CODE_STANDARD.md`
- `Cargo.toml`
- `agents.toml`
- `src/**/*.rs` (full source-tree sweep completed across every Rust file under `src/`)
- Refactor-critical files reviewed in detail:
  - `src/context.rs`
  - `src/turn.rs`
  - `src/agent/loop_impl.rs`
  - `src/cli.rs`
  - `src/session_runtime/drain.rs`
  - `src/llm/history_groups.rs`
  - `src/session/jsonl.rs`
  - `src/session/trimming.rs`
  - `src/agent/mod.rs`
  - `src/agent/shell_execute.rs`
  - `src/agent/loop_impl/tests.rs`
  - `src/agent/tests/regression_tests.rs`
  - `src/main.rs`
  - `src/session_runtime/factory.rs`
  - `src/plan/executor.rs`
  - `src/plan/runner.rs`
  - `src/server/queue.rs`
  - `src/server/ws.rs`
  - `src/subscription.rs`
- Supporting docs reviewed for constraints and current architecture:
  - `docs/risks.md`
  - `docs/architecture/overview.md`

## 2. Exact Changes Per File

- `src/context/mod.rs`
  - Replace the current monolithic `src/context.rs` module root.
  - Declare submodules: `identity_prompt`, `skill_summaries`, `skill_instructions`, `subscriptions`, `history`, `tests`.
  - Preserve the existing external `crate::context::*` API with `pub use` only where callers need stable paths.

- `src/context/identity_prompt.rs`
  - Move identity prompt loading/rendering helpers here.
  - Keep prompt assembly logic isolated from history and subscription logic.

- `src/context/skill_summaries.rs`
  - Move skill-summary loading/formatting helpers here.
  - Keep catalog summarization separate from instruction rendering.

- `src/context/skill_instructions.rs`
  - Move skill instruction expansion/formatting helpers here.
  - Keep skill text assembly separate from prompt identity and history trimming.

- `src/context/subscriptions.rs`
  - Move subscription-context assembly here.
  - Keep subscription rendering isolated from prompt/history logic.

- `src/context/history.rs`
  - Move history selection and formatting helpers here.
  - Remove any local reverse-walk/grouping implementation and consume one shared helper from `src/llm/history_groups.rs`.
  - Add an invariant comment stating that assistant/tool round-trips are grouped atomically and must never be split when selecting history.

- `src/context/tests.rs`
  - Move cross-module `context` tests here.
  - Do not move private-helper assertions blindly: for tests that currently rely on sibling-private helpers, either rewrite them against the public `assemble` surface or expose only the minimal helper(s) as `pub(super)` for test access.

- `src/llm/history_groups.rs`
  - Keep this as the single source of truth for grouping history into replay-safe units.
  - Add the concrete shared helper that collects the newest whole groups within a token/message budget.
  - Add an invariant comment describing assistant/tool round-trip grouping and why partial groups are forbidden.

- `src/turn/mod.rs`
  - Replace the current monolithic `src/turn.rs` module root.
  - Declare submodules: `verdicts`, `tiers`, `builders`, `tests`.
  - Re-export the same public turn-building API so callers continue using `crate::turn::*`.

- `src/turn/verdicts.rs`
  - Move guard verdict types and precedence logic here.
  - Add an invariant comment documenting the precedence rule: `deny > approve > allow`.

- `src/turn/tiers.rs`
  - Move tier-specific tool-surface and guard composition helpers here.
  - Keep tier policy selection separate from verdict types and turn builders.

- `src/turn/builders.rs`
  - Move turn-construction helpers here, including the shared `build_turn_for_config()` path used by CLI/server flows.
  - Keep `build_turn_for_config()` as the only shared constructor path; do not leave behind alternate builder entrypoints in caller code after the split.
  - Keep orchestration/build logic separate from verdict type definitions.

- `src/turn/tests.rs`
  - Move `turn` tests here.
  - Apply the same visibility rule as `context`: if existing tests touch sibling-private helpers, rewrite them against public builder/verdict surfaces or expose only the minimal helper(s) as `pub(super)`.

- `src/agent/mod.rs`
  - Declare new submodules `audit` and `usage`.
  - Own the shared agent-facing enums/types that both `loop_impl.rs` and `audit.rs` need, specifically `TurnVerdict` and `QueueOutcome`.
  - Re-export the one shared `format_denial_message` function for CLI/session-runtime callers.

- `src/agent/audit.rs`
  - Move audit-note appenders, denied-assistant persistence helpers, `make_denial_verdict`, and the one shared `format_denial_message` here.
  - Keep denial text generation in exactly one location.

- `src/agent/usage.rs`
  - Move token/budget accounting helpers out of `loop_impl.rs`.
  - Keep usage/budget logic isolated from queue draining and audit persistence.

- `src/agent/loop_impl.rs`
  - Keep only the core turn runner/orchestration path here.
  - Import `TurnVerdict` and `QueueOutcome` from `src/agent/mod.rs`.
  - Call into `audit` and `usage` helpers instead of owning that logic inline.

- `src/cli.rs`
  - Delete the duplicate denial formatter/wrapper.
  - Keep the CLI-side direct turn-construction path on `build_turn_for_config()`.
  - Call the shared `crate::agent::format_denial_message` export.

- `src/session_runtime/drain.rs`
  - Delete any duplicate denial-formatting logic here.
  - Route denial text generation through the same shared formatter used by CLI/agent code paths.

- `src/main.rs`
  - Treat this as the process entrypoint only.
  - Update imports only if module re-exports move during the split; do not use `src/main.rs` as the architectural anchor for the shared turn-constructor invariant.

- `src/session_runtime/factory.rs`
  - Keep this direct runtime-side turn-construction path on `build_turn_for_config()`.
  - Update imports only as required by the `src/turn/*` split, without introducing a second builder entrypoint.

- `src/server/queue.rs`
  - Keep this direct queue/server turn-construction path on `build_turn_for_config()`.
  - Update imports only as required by the `src/turn/*` split, without introducing a second builder entrypoint.

- `src/server/ws.rs`
  - Keep this direct websocket/server turn-construction path on `build_turn_for_config()`.
  - Update imports only as required by the `src/turn/*` split, without introducing a second builder entrypoint.

- `src/agent/loop_impl/tests.rs`
  - Update imports and helper references after the `audit`/`usage` extraction.
  - Keep existing loop regression coverage attached to the focused runner.

- `src/agent/tests/regression_tests.rs`
  - Add or adjust regression coverage for guard precedence, denial verdict creation, shared denial formatting, and the shared turn-builder invariant if those assertions already live in agent-level tests.

- `docs/architecture/overview.md`
  - Update any architecture text that names `src/context.rs`, `src/turn.rs`, or a monolithic `src/agent/loop_impl.rs`.
  - Reflect the new split boundaries only if those file paths or responsibilities are documented there now.

- `AGENTS.md`
  - Update the “Key Files” and any other path references that still point at removed single-file module paths such as `src/context.rs` or `src/turn.rs`.
  - Keep the repository guidance aligned with the new module layout.

## 3. What Tests To Write

- Guard precedence regression:
  - Explicit assertions that combined guard outcomes still resolve as `deny > approve > allow`.
  - This must prove precedence did not change during the `turn` split.

- History grouping regression:
  - Add/adjust tests for the shared helper in `src/llm/history_groups.rs`.
  - Assert that assistant/tool round-trips are preserved as whole groups and are either fully included or fully excluded under budget pressure.
  - Assert that `src/context/history.rs` now uses the shared grouping behavior instead of a second implementation.

- Context split regression:
  - Preserve existing `context` behavior tests after the file split.
  - If private-helper tests are rewritten, ensure they still prove the same observable behavior through the public assembly surface.

- Turn split regression:
  - Preserve existing `turn` behavior tests after the file split.
  - If private-helper tests are rewritten, assert through public builder/verdict entry points rather than broadening visibility unnecessarily.

- Shared turn-constructor invariant:
  - Add a structural or regression check that the direct caller set still routes through the single shared `build_turn_for_config()` constructor after the split.
  - The direct caller set to verify explicitly is:
    - `src/cli.rs`
    - `src/session_runtime/factory.rs`
    - `src/server/queue.rs`
    - `src/server/ws.rs`
  - Make the structural check exclusive as well as inclusive: it should fail if any other `src/` file grows a new direct `build_turn_for_config()` call.

- Agent extraction regression:
  - Add/adjust tests showing `audit` extraction preserves denial verdict creation and denial-note persistence behavior.
  - Add/adjust tests showing `usage` extraction preserves token/budget calculations.

- Shared denial formatting:
  - Behavior test proving the CLI path and session-runtime path produce the same denial text through the shared formatter.
  - Structural uniqueness check (grep/lint assertion) that `fn format_denial_message` exists in exactly one place: `src/agent/audit.rs`.

- Final verification guard:
  - Confirm the final passing test total reported by `cargo test` remains at least `555`, so moved tests were not silently dropped during the refactor.

## 4. Order Of Operations

1. Add the shared helper and invariant comment in `src/llm/history_groups.rs`, then switch `src/context/history.rs` to that helper.
   - Run focused grouping/history tests immediately so the grouping change is proven before larger file moves.

2. Add `src/agent/audit.rs` and `src/agent/usage.rs`, move `TurnVerdict` and `QueueOutcome` ownership into `src/agent/mod.rs`, and shrink `src/agent/loop_impl.rs` to the core runner.
   - Update agent tests in the same step so the extraction compiles cleanly and preserves behavior.

3. Move the one shared denial formatter into `src/agent/audit.rs`, then replace duplicate logic in `src/cli.rs` and `src/session_runtime/drain.rs`.
   - Run focused denial-formatting checks and the structural uniqueness grep immediately after this step.

4. Split `src/context.rs` into `src/context/*`.
   - Move or rewrite `context` tests in the same step, using public-surface assertions or minimal `pub(super)` exposure where necessary, so the tree stays compiling and tests stay green.

5. Split `src/turn.rs` into `src/turn/*`.
   - Move or rewrite `turn` tests in the same step, using the same visibility strategy as `context`, so the tree stays compiling and tests stay green.
   - In the same step, verify that the only direct `build_turn_for_config()` callers in `src/` are `src/cli.rs`, `src/session_runtime/factory.rs`, `src/server/queue.rs`, and `src/server/ws.rs`.

6. Update docs in the same change set:
   - `docs/architecture/overview.md`
   - `AGENTS.md`
   - Any other document that still names removed single-file paths after the split

7. Run the full verification stack:
   - `cargo fmt --check`
   - `cargo build --release`
   - `cargo clippy -- -D warnings`
   - `cargo test`
   - Confirm the reported passing test total is still `555+`
   - `xtask/lint.sh`
   - `cargo test --features integration` if auth/config is available for live tests

8. Only after all checks pass, run:
   - `openclaw system event --text 'Session 6 done: turn/context/loop split' --mode now`

## 5. Risk Assessment

- Highest risk: semantic drift in history grouping.
  - Mitigation: land the `history_groups` helper first, add the round-trip invariant comment there, and prove the behavior with focused tests before the `context` split.

- High risk: module privacy breaks parent-level test modules.
  - Mitigation: for both `src/context/tests.rs` and `src/turn/tests.rs`, prefer rewriting tests against public behavior; use only minimal `pub(super)` exposure when public-surface assertions cannot cover the same invariant.

- High risk: dependency tangles in the agent split.
  - Mitigation: keep shared enums/types in `src/agent/mod.rs`, keep helper modules one-way (helpers do not call back into `loop_impl.rs`), and keep `loop_impl.rs` focused on orchestration only.

- Medium risk: duplicate denial formatting survives under a different helper name.
  - Mitigation: use both behavior tests across callers and a structural uniqueness grep for the shared formatter definition.

- Medium risk: dropped tests during file moves.
  - Mitigation: preserve existing test cases during the split and explicitly verify that the final passing test total remains at least `555`.

- Medium risk: architecture drift creates more than one turn-construction path.
  - Mitigation: keep `build_turn_for_config()` as the single shared constructor and explicitly verify that the only direct caller files in `src/` are `src/cli.rs`, `src/session_runtime/factory.rs`, `src/server/queue.rs`, and `src/server/ws.rs`.

- Medium risk: doc staleness after `src/context.rs` and `src/turn.rs` disappear.
  - Mitigation: update `docs/architecture/overview.md`, `AGENTS.md`, and any path references in the same change set rather than as a follow-up.
