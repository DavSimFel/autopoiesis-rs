# Session 9 Plan

## 1. Files Read

Required standards/config/docs:

```text
CODE_STANDARD.md
Cargo.toml
agents.toml
AGENTS.md
README.md
docs/risks.md
docs/architecture/overview.md
docs/specs/restructure-plan.md
xtask/lint.sh
tests/MANUAL.md
tests/integration.rs
tests/plan_engine.rs
tests/shipped_shell_policy.rs
tests/tier_integration.rs
tests/xtask_lint_paths.sh
```

All Rust source under `src/`:

```text
src/agent/audit.rs
src/agent/loop_impl.rs
src/agent/loop_impl/tests.rs
src/agent/mod.rs
src/agent/queue.rs
src/agent/queue/tests.rs
src/agent/shell_execute.rs
src/agent/spawn.rs
src/agent/spawn/tests.rs
src/agent/tests.rs
src/agent/tests/common.rs
src/agent/tests/regression_tests.rs
src/agent/usage.rs
src/app/args.rs
src/app/mod.rs
src/app/plan_commands.rs
src/app/session_run.rs
src/app/subscription_commands.rs
src/app/tracing.rs
src/auth.rs
src/config/agents.rs
src/config/domains.rs
src/config/file_schema.rs
src/config/load.rs
src/config/mod.rs
src/config/models.rs
src/config/policy.rs
src/config/runtime.rs
src/config/spawn_runtime.rs
src/config/tests.rs
src/context/history.rs
src/context/identity_prompt.rs
src/context/mod.rs
src/context/skill_instructions.rs
src/context/skill_summaries.rs
src/context/subscriptions.rs
src/context/tests.rs
src/delegation.rs
src/gate/budget.rs
src/gate/command_path_analysis.rs
src/gate/exfil_detector.rs
src/gate/mod.rs
src/gate/output_cap.rs
src/gate/protected_paths.rs
src/gate/secret_catalog.rs
src/gate/secret_redactor.rs
src/gate/shell_safety.rs
src/gate/streaming_redact.rs
src/identity.rs
src/lib.rs
src/llm/history_groups.rs
src/llm/mod.rs
src/llm/openai.rs
src/logging.rs
src/main.rs
src/model_selection.rs
src/plan.rs
src/plan/executor.rs
src/plan/notify.rs
src/plan/patch.rs
src/plan/recovery.rs
src/plan/runner.rs
src/principal.rs
src/read_tool.rs
src/server/auth.rs
src/server/http.rs
src/server/mod.rs
src/server/queue.rs
src/server/queue_worker.rs
src/server/session_lock.rs
src/server/state.rs
src/server/ws.rs
src/session/budget.rs
src/session/delegation_hint.rs
src/session/jsonl.rs
src/session/mod.rs
src/session/tests.rs
src/session/trimming.rs
src/session_runtime/drain.rs
src/session_runtime/factory.rs
src/session_runtime/mod.rs
src/skills.rs
src/spawn.rs
src/store/message_queue.rs
src/store/migrations.rs
src/store/mod.rs
src/store/plan_runs.rs
src/store/sessions.rs
src/store/step_attempts.rs
src/store/subscriptions.rs
src/subscription.rs
src/template.rs
src/terminal_ui.rs
src/time.rs
src/tool.rs
src/turn/builders.rs
src/turn/mod.rs
src/turn/tests.rs
src/turn/tiers.rs
src/turn/verdicts.rs
src/util.rs
```

## 2. Exact Changes Per File

Delete list:

- `src/spawn.rs`
- `src/agent/spawn.rs`
- `src/agent/spawn/tests.rs`
- `src/llm/openai.rs`
- `src/util.rs`

Planned file changes:

- `src/child_session/mod.rs` (new)
  Public child-session facade. Reexport only the requested public surface: `SpawnRequest`, `SpawnResult`, `SpawnDrainResult`, `spawn_child()`, and `enqueue_child_completion()`. Keep `should_enqueue_child_completion()`, assistant-response helpers, and child-metadata parsing `pub(crate)` so the cleanup does not accidentally widen the public API.

- `src/child_session/create.rs` (new, moved out of `src/spawn.rs`)
  Move `SpawnRequest`, `SpawnResult`, `ChildSessionMetadata`, `parse_child_session_metadata()`, `validate_spawn_budget()`, `resolve_model()`, `validate_tier()`, `resolve_spawn_tier()`, `validate_requested_skills()`, `generate_child_session_id()`, and `spawn_child()`. Keep the existing spawn validation/model/skill tests here.

- `src/child_session/completion.rs` (new, moved out of `src/spawn.rs`)
  Move `enqueue_child_completion()`, `should_enqueue_child_completion()`, `build_completion_message()`, and assistant-response extraction. Keep only `enqueue_child_completion()` public; the other helpers stay crate-private. Add the bug fix here: treat empty or whitespace-only assistant text as absent before composing the parent completion payload. Use the latest non-empty agent assistant text from session history before falling back to `No assistant response was produced.`. Move the completion-focused unit tests here and tighten them around non-blank payloads.

- `src/agent/child_drain.rs` (rename from `src/agent/spawn.rs`)
  Rename the module/file only, update imports from `crate::spawn::*` to `crate::child_session::*`, and keep `SpawnDrainContext`, `finish_spawned_child_drain()`, `spawn_and_drain_with_provider()`, `spawn_and_drain()`, and T2 plan handoff intact. No stream/runtime behavior change beyond the module rename.

- `src/agent/child_drain/tests.rs` (rename from `src/agent/spawn/tests.rs`)
  Update the module import from `crate::agent::spawn` to `crate::agent::child_drain`. Keep the existing runtime-config, plan-handoff, stale-history, and approval tests. Add or update the regression coverage so a tool-call-only child turn cannot yield a blank parent completion.

- `src/agent/mod.rs`
  Change `mod spawn;` to `mod child_drain;`. Reexport child-session DTOs from `crate::child_session` instead of `crate::spawn`. Reexport `spawn_and_drain`, `SpawnDrainContext`, and `finish_spawned_child_drain` from `child_drain`. Update the wrapper `spawn_child()` to call `crate::child_session::spawn_child()`. Final state: no public `spawn` module path remains.

- `src/session_runtime/drain.rs`
  Replace `use crate::spawn;` and every `crate::spawn::*` call with `crate::child_session::*` or direct helper imports. Keep queue-claim and completion-enqueue control flow unchanged.

- `src/plan/runner.rs`
  Replace `crate::spawn::SpawnRequest`, `crate::spawn::SpawnResult`, and `crate::spawn::spawn_child()` with `crate::child_session::*`. Keep the `SpawnDrainContext` / `finish_spawned_child_drain()` import aligned with the renamed `agent::child_drain` reexports. Update `utc_timestamp` import at the same time.

- `src/llm/openai/mod.rs` (new)
  Keep `OpenAIProvider`, `new()`, `with_client()`, the HTTP transport loop, chunk buffering, trailing-buffer parse, terminal-event enforcement, and `impl LlmProvider`. Keep the public path `crate::llm::openai::OpenAIProvider` stable so root integration tests do not need an OpenAI import path rewrite.

- `src/llm/openai/request.rs` (new)
  Move `build_input()` and `build_tools()` here, along with the request-shape tests that assert first-system-message instructions extraction, later system replay, audit-note replay, tool-call mapping, tool-result mapping, and flat `tools` payload shape.

- `src/llm/openai/sse.rs` (new)
  Move `SseEvent`, `parse_sse_line()`, `upsert_tool_call()`, `finalize_function_call()`, `finalize_output_item()`, `SseStreamState`, `apply_sse_event()`, `require_terminal_sse_event()`, and `note_terminal_sse_event()` here, with the parser/state-machine tests. Preserve the exact stream behavior: function-call ordering, interleaved deltas, trailing buffer parsing, multiple events per chunk, missing-id/name drop behavior, and terminal-event failure.

- `src/llm/mod.rs`
  Keep `pub mod openai;` but point it at the new directory module layout. No public API rename.

- `src/store/mod.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;` everywhere in this file, including test code. Leave `format_system_time()` in place so this cleanup does not silently change the public `store` surface or broaden scope beyond the requested `util.rs` removal.

- `src/agent/loop_impl.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/plan/notify.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/plan/patch.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/session/jsonl.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/store/message_queue.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/store/plan_runs.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/store/sessions.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/store/step_attempts.rs`
  Replace `use crate::util::utc_timestamp;` with `use crate::time::utc_timestamp;`.

- `src/util.rs`
  Delete it after every call site moves to `crate::time`. No compatibility alias in the final tree.

- `src/lib.rs`
  Add `pub mod child_session;`. Remove `pub mod spawn;` and `pub mod util;`. Keep `pub mod time;`. Update any root reexports so downstream code reaches child-session types via `child_session`, not `spawn`.

- `src/server/queue.rs`
  Update the regression test `drain_queue_enqueues_completion_when_persisted_history_exists_but_new_assistant_response_is_empty` to assert the parent completion is non-blank and uses the newest non-empty agent assistant text (or the fallback string if none exists), instead of tolerating an empty payload body.

- `tests/tier_integration.rs`
  Replace `autopoiesis::spawn::{SpawnRequest, SpawnResult}` and every `autopoiesis::spawn::*` call with `autopoiesis::child_session::*`. Keep the tier/runtime/skill/completion assertions the same apart from the renamed path.

- `tests/MANUAL.md`
  Add an architecture spot-check item so the manual checklist reflects the new paths: `src/child_session/`, `src/agent/child_drain.rs`, `src/llm/openai/{mod,request,sse}.rs`, and the removal of `src/util.rs`.

- `README.md`
  Update the architecture tree from `spawn.rs`, `agent/spawn.rs`, `llm/openai.rs`, and `util.rs` to the new module tree. Also refresh the stale current-state file/line counts if README is intended to describe the checked-in repo.

- `docs/architecture/overview.md`
  Update current-state path references to `src/child_session/*`, `src/agent/child_drain.rs`, and `src/llm/openai/{mod,request,sse}.rs`. Replace the `src/util.rs` note with `src/time.rs`. Refresh the stale file/line/test counts.

- `AGENTS.md`
  Update the note that currently says `src/llm/openai.rs` has the SSE parser to point at `src/llm/openai/sse.rs`. If the on-disk AGENTS summary is kept current, also refresh the module tree wording for child-session helpers.

Files read but not expected to need path edits:

- `tests/integration.rs`
  The import path `autopoiesis::llm::openai::OpenAIProvider` should remain stable after the split, so this file should only change if the module split accidentally breaks visibility.

- `tests/plan_engine.rs`
  No path change expected. Read for plan-run/spawn-step invariants only.

- `tests/shipped_shell_policy.rs`
  No path change expected. Read for shipped-config coverage only.

- `tests/constitution/results/1st_person/07_irreversible_delete.txt`
  Explicitly treat this as a historical text fixture unless the constitution-result corpus is being regenerated in the same session. If it is left unchanged, exclude `tests/constitution/results/` from the stale-path verification sweep instead of leaving those references as accidental stragglers.

- `docs/specs/restructure-plan.md`
  Explicitly treat this as a historical planning/spec document unless the restructure spec itself is being refreshed in the same session. If it is left unchanged, exclude it from the stale-path verification sweep so intentional historical path references do not create false failures.

## 3. What Tests To Write

- Child completion regression in `src/child_session/completion.rs`
  Assert that `build_completion_message()` ignores `Some("")` and `Some("   ")`.
  Assert that an empty fresh assistant response falls back to the newest non-empty agent assistant text in session history.
  Assert that if there is no non-empty agent assistant text at all, the payload uses `No assistant response was produced.`.

- Queue/drain regression in `src/server/queue.rs`
  Tighten `drain_queue_enqueues_completion_when_persisted_history_exists_but_new_assistant_response_is_empty` so it rejects blank payloads, not just stale-history leakage.

- Child-drain stale-history invariant in `src/agent/child_drain/tests.rs`
  Keep the existing assertion that non-agent/inbound assistant history does not become `last_assistant_response`.
  If the completion fix is scoped only to payload building, also assert that `finish_spawned_child_drain()` return semantics stay unchanged.

- OpenAI request-builder tests
  Move the current `build_input*` and `build_tools*` tests into `request.rs` unchanged. The invariant is request-shape stability, not new behavior.

- OpenAI SSE/parser tests
  Move the current parser/state tests into `sse.rs` unchanged. The invariant is byte-for-byte stream behavior preservation: chunk splits, trailing buffer handling, terminal-event enforcement, tool-call ordering, and missing-id/name rejection.

- OpenAI transport test
  Keep the HTTP-level `stream_completion_requires_terminal_sse_event_through_http` test in `openai/mod.rs` so the split still proves end-to-end behavior did not change.

- Public-path compilation coverage
  `tests/tier_integration.rs` becomes the root-level proof that the public import path is now `autopoiesis::child_session::*` and not `autopoiesis::spawn::*`.

## 4. Order Of Operations

1. Move the time helpers first.
   Do only the requested `util.rs` cleanup: flip every `crate::util::utc_timestamp` call site to `crate::time::utc_timestamp`, including `src/store/mod.rs`, run the focused store/session/plan tests, then delete `src/util.rs`.

2. Introduce `src/child_session/`.
   Move `src/spawn.rs` into `src/child_session/create.rs` and `src/child_session/completion.rs`, add `src/child_session/mod.rs`, port the existing spawn/completion tests into the new files, and update `src/lib.rs`, `src/agent/mod.rs`, `src/session_runtime/drain.rs`, `src/plan/runner.rs`, and `tests/tier_integration.rs` in the same compile boundary before deleting `src/spawn.rs`.

3. Rename the agent drain module.
   Rename `src/agent/spawn.rs` to `src/agent/child_drain.rs` and `src/agent/spawn/tests.rs` to `src/agent/child_drain/tests.rs`, update `agent/mod.rs` and any internal imports, then run the focused child-drain, plan-runner, and tier tests.

4. Land the blank-completion fix while the completion helpers are isolated.
   Implement the blank-as-absent normalization inside `child_session/completion.rs`, update the server queue regression and the completion unit tests, and re-run the focused completion/drain tests before touching OpenAI.

5. Split `src/llm/openai.rs` in one compile-boundary patch.
   Because Rust cannot keep both `src/llm/openai.rs` and `src/llm/openai/mod.rs` at once, add `src/llm/openai/{mod.rs,request.rs,sse.rs}`, move the code in one step, update tests in the same patch, then delete `src/llm/openai.rs`.

6. Update docs and the manual checklist last.
   Patch `tests/MANUAL.md`, `README.md`, `docs/architecture/overview.md`, and `AGENTS.md` after the final source paths exist so the docs reflect the final tree, not an intermediate one. Treat `tests/constitution/results/` and `docs/specs/restructure-plan.md` as historical unless they are explicitly regenerated or refreshed in this session.

7. Run verification in this order.
   Focused tests after each step:
   `cargo test child_completion`
   `cargo test finish_spawned_child_drain`
   `cargo test drain_queue_enqueues_completion_when_persisted_history_exists_but_new_assistant_response_is_empty`
   `cargo test openai`
   `cargo test --test tier_integration`

   Tree-sweep verification before the full suite:
   Run a targeted grep for stale code/doc paths and old module imports, excluding `tests/constitution/results/` and `docs/specs/restructure-plan.md` if those references are intentionally left historical.
   Check for `crate::spawn`, `autopoiesis::spawn`, `crate::util`, `src/spawn.rs`, `src/agent/spawn.rs`, `src/llm/openai.rs`, and `src/util.rs` in `src/`, `tests/`, `README.md`, `docs/`, and `AGENTS.md`.

   Final required checks:
   `cargo build --release`
   `cargo fmt --check`
   `cargo clippy -- -D warnings`
   `cargo test`
   `xtask/lint.sh`

   Conditional live check if auth exists:
   `cargo test --features integration --test integration`

## 5. Risk Assessment

- `src/llm/openai.rs` is the highest-risk split.
  The parser is protocol-sensitive. A visibility-only refactor can still change stream behavior if the chunk loop, trailing-buffer parse, or stop-reason state machine drifts. Mitigation: move the existing logic with minimal edits and keep the current tests, not rewritten approximations.

- The blank-completion fix can easily widen behavior if applied in the wrong place.
  If blank filtering is pushed into the generic `last_assistant_response` path, it may change `SpawnDrainResult.last_assistant_response` and plan-step summaries, not just parent completion payloads. Mitigation: scope the normalization to completion payload construction unless a caller explicitly wants normalized text.

- `spawn.rs` to `child_session/` is a public API rename.
  Internal callers are straightforward, but root tests and any downstream code using `autopoiesis::spawn::*` will fail. Mitigation: switch every internal caller first, then remove the old module, and grep the tree for `crate::spawn`, `autopoiesis::spawn`, and `agent::spawn`.

- `util.rs` removal is mechanically simple but touches persistence code everywhere.
  Timestamp helpers are used by store/session/plan code, so a missed import causes broad compile failures. Mitigation: do the time move first and finish it fully before the larger rename/split work.

- Moving `format_system_time()` out of `src/store/mod.rs` must preserve exact formatting.
  Subscription ordering and update tests rely on the current microsecond UTC string shape. Mitigation: copy the implementation exactly and add direct format tests in `src/time.rs`.

- The docs are already stale on current counts and paths.
  `README.md` and `docs/architecture/overview.md` still describe the old tree and old file counts. Mitigation: update them only after the source moves are complete, then refresh the counts once from the final tree.
