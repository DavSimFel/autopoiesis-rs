# PLAN

No code changes performed in this step. This file is the implementation plan only.

## 1. Files Read

- `Cargo.toml`
- `agents.toml`
- `CRITICAL_REVIEW.md`
- `docs/current/risks.md`
- `docs/current/architecture.md`
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
- `src/store.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### `src/gate/mod.rs`

- Extend `guard_message_output()` so it also matches `MessageContent::ToolCall { call }`.
- Redact `call.arguments` with the same outbound secret-redaction path already used for text blocks.
- Keep the change scoped to the persisted assistant message only. Do not mutate the separate `turn_reply.tool_calls` vector that `run_agent_loop()` executes, otherwise the agent would execute redacted JSON instead of the original tool call.
- Add an explicit fallback for the "text-delta guard denied the arguments and returned an empty string" case:
  - keep the tool-call block so assistant/tool round-trips stay structurally intact
  - persist a valid empty-object arguments payload (`"{}"`) instead of an invalid blank string
- Keep the existing empty-text pruning behavior limited to `Text` blocks.
- Add unit coverage for:
  - a secret inside `ToolCall.arguments` becoming `[REDACTED]`
  - a mixed assistant message retaining tool-call and tool-result blocks while only the secret-bearing fields are changed
  - a deny-to-empty case on tool-call arguments falling back to `"{}"` instead of persisting invalid JSON

### `src/agent.rs`

- Replace the persisted approval/denial audit entries with a non-system replay role.
- Use assistant-role text messages with `Principal::System` for audit notes, constructed explicitly with `ChatMessage::with_role_with_principal(...)` plus a single `MessageContent::Text` block.
- Refactor the current helpers:
  - `append_approval_denied(...)`
  - `append_hard_deny(...)`
  - the inline inbound-denial `session.append(ChatMessage::system_with_principal(...))` block inside `run_agent_loop()`
- Change those persisted audit notes so they do not include raw model/user-controlled command text.
- Strip raw `command` text from persisted audit notes entirely.
- Strip raw `reason` text from persisted audit notes when it can carry model-controlled command text. Persist only a generic summary plus the guard id, for example "execution request rejected after approval by shell-policy" or "execution request hard-denied by shell-policy".
- Keep detailed denial information in the returned `TurnVerdict` and existing CLI/WS terminal output paths so operators still see the real reason immediately; only the replayed session history is sanitized/lowered out of system-prompt space.
- Update call sites so approval-denial persistence receives `gate_id` directly instead of reconstructing the message from `reason` and `command`.
- Change tool-result principal assignment at the tool execution site from conditional operator/system selection to unconditional `Principal::System`.
- Extract the current "process one already-claimed queue row and apply QueueOutcome semantics" logic into a shared helper in `agent.rs`.
- Refactor existing `drain_queue()` to call that shared helper, so CLI keeps the same behavior while server concurrency work can reuse the exact processed/failed/denial handling instead of duplicating the full drain loop.
- Add/extend tests for:
  - approval denial persistence is no longer `ChatRole::System`
  - persisted audit entries do not contain the raw command text
  - a hard deny also persists as non-system audit text
  - tool results produced from a clean operator turn are still persisted with `Principal::System`
  - a follow-up turn becomes tainted after replaying a prior tool result

### `src/llm/openai.rs`

- Add a regression test for the replay boundary that motivated P0-2.
- Build a message history containing:
  - the primary system prompt
  - a normal user message
  - the new assistant-role/system-principal audit note
- Assert `OpenAIProvider::build_input()` keeps the first system prompt in `instructions` and does not replay the audit note as a later system-role input.

### `src/server.rs`

- Remove the global `worker_lock` field from `ServerState` and all code paths/tests that construct it.
- Add per-session locking, keyed by `session_id`, inside `ServerState`, using a lazily populated map of session id to `Arc<tokio::sync::Mutex<()>>`.
- Add a helper that returns the per-session mutex for a given `session_id`.
- Factor the current "load session + drain queue" logic into a server-local async helper used by both websocket and HTTP workers.
- Make that helper explicitly testable by parameterizing the provider factory, token sink, and approval handler instead of hard-wiring `auth::get_valid_token()` and production WS approval plumbing inside the helper body.
- In that helper:
  - acquire the per-session lock once for the session turn
  - load/reload `Session`
  - claim one queued message at a time by locking `state.store` only around `dequeue_next_message()`
  - drop the store mutex before calling the new shared agent-side "process claimed message" helper
  - reacquire the store mutex only long enough to call `mark_processed()` or `mark_failed()`
  - continue looping until the session queue is empty or a terminal denial/error is reached
- Replace current websocket and HTTP worker calls to `agent::drain_queue(...)` with the new server-local claim loop so the store mutex is never held across provider calls, tool execution, or websocket approval waits.
- Keep CLI on `agent::drain_queue()`, but make both paths share the same per-message processing helper so only queue-claim strategy differs.
- Keep short store-lock sections for:
  - `create_session()`
  - `list_sessions()`
  - enqueue operations
  - queue claim / processed / failed transitions
- Update `test_state()` and any helper setup to initialize the new per-session lock map.
- Add concurrency-focused tests in this file for:
  - different sessions can make progress concurrently
  - same-session workers still serialize
  - store operations are not blocked while another session is inside a long-running agent turn

## 3. What Tests To Write

### P0-1: Tool-call argument redaction

- `guard_message_output_redacts_tool_call_arguments`
  - Build an assistant message with a `ToolCall.arguments` JSON string containing a known secret.
  - Run `guard_message_output()`.
  - Assert the secret is absent and `[REDACTED]` is present in `call.arguments`.
  - Assert the tool-call block is still present.

- `guard_message_output_denied_tool_call_arguments_fallback_to_empty_object`
  - Use a deny-on-text test guard.
  - Run `guard_message_output()` on an assistant tool call.
  - Assert the block is retained and `call.arguments == "{}"`.
  - Assert no blank arguments string is persisted.

- `run_agent_loop_persists_redacted_tool_call_arguments`
  - Use a provider that emits a tool call containing a secret in its arguments.
  - Run one tool-call turn plus a final stop turn.
  - Assert the session JSONL never contains the original secret.
  - Assert the stored assistant tool-call entry contains the redacted arguments.

### P0-2: Audit-message privilege reduction and sanitization

- `approval_denial_audit_is_not_system_role`
  - Trigger a guarded tool call, deny approval, reload session history.
  - Assert the persisted audit note is assistant-role, not system-role.

- `approval_denial_audit_omits_raw_command_and_reason_payload`
  - Use a command containing unmistakable marker text.
  - Deny approval.
  - Assert the marker text is absent from JSONL and replayed history.
  - Assert the stored note still includes the guard id or generic event label.

- `hard_deny_audit_is_non_system_and_sanitized`
  - Trigger a hard deny from a guard.
  - Assert the persisted note is not system-role and does not copy the raw command text.

- `audit_note_does_not_reenter_openai_system_replay_path`
  - Construct/reload history with the new persisted audit note.
  - Feed it through `OpenAIProvider::build_input()`.
  - Assert the note is not placed into `instructions` or a later `{"role":"system"}` replay item.

### P0-3: Tool outputs always taint future turns

- `tool_result_principal_is_system_even_for_operator_turns`
  - Execute a tool from an untainted operator session.
  - Assert the persisted tool-result message principal is `System`.

- `replayed_tool_result_marks_followup_turn_tainted`
  - Seed/reload a session containing a tool-result message with `Principal::System`.
  - Run inbound checks on a follow-up prompt.
  - Assert `turn.is_tainted()` becomes `true`.

### P0-4: Server concurrency

- `different_sessions_do_not_block_each_other`
  - Invoke the new injected/testable server session-drain helper with fake providers.
  - Start one session turn with a provider/tool that waits on a barrier.
  - Start a second session turn with a fast provider.
  - Assert the second session completes before releasing the first barrier.

- `same_session_processing_is_serialized`
  - Invoke the same helper twice for one session.
  - Start two workers for the same session.
  - Hold the first inside the turn.
  - Assert the second does not begin provider execution until the first releases the per-session lock.

- `store_mutex_is_not_held_across_agent_turn`
  - Use the injected helper so no live auth is involved.
  - Hold one session inside a long-running turn.
  - While it is blocked, acquire `state.store` in another task and perform `create_session()` or `list_sessions()`.
  - Assert that operation completes immediately instead of waiting for the first turn to finish.

## 4. Order Of Operations

1. Fix P0-1 in `src/gate/mod.rs` first.
   - Smallest isolated change.
   - Adds the missing redaction coverage without affecting execution flow.

2. Fix P0-2 and P0-3 together in `src/agent.rs`.
   - Add the sanitized assistant-role audit helpers.
   - Flip tool-result principals to unconditional `System`.
   - Extract the shared per-message queue-processing helper that both CLI and server will use.

3. Add the P0-2 replay regression test in `src/llm/openai.rs`.
   - This locks the actual upstream replay invariant before the server refactor.

4. Refactor server draining for P0-4 in `src/server.rs`.
   - Introduce the per-session lock map.
   - Add the injected/testable server session-drain helper.
   - Replace the long-held global/store lock pattern with short claim/mark lock sections around the shared agent helper.
   - Add concurrency tests once the helper shape is stable.

5. Run the full required checks from `AGENTS.md`.
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy -- -D warnings`
   - `cargo build --release`

6. If any concurrency test is timing-sensitive, stabilize it with barriers/channels before merging.

## 5. Risk Assessment

- P0-1 risk:
  - Persisted assistant tool-call arguments will intentionally diverge from the originally executed arguments.
  - This is correct for secrecy, but only if live execution still uses the original streamed `tool_calls` vector.
  - The implementation must not accidentally execute the redacted persisted copy.
  - The deny-to-empty fallback must preserve valid JSON; otherwise replay corruption would replace the leak with a parse failure.

- P0-2 risk:
  - Lowering audit notes from system-role to assistant-role changes replay semantics.
  - That is intentional and desirable for security, but it may slightly change future model behavior because denial notes no longer carry system priority.
  - Keeping the operator-facing denial detail in `TurnVerdict` output avoids losing observability.

- P0-3 risk:
  - Marking all tool outputs as `Principal::System` will taint more follow-up turns.
  - Expected behavior: more approvals may be required and standing approvals may stop applying after any shell read.
  - This is the intended security tightening, but it is a behavior change and should be called out in review.

- P0-4 risk:
  - A per-session lock map introduces lock-lifecycle bookkeeping. If locks are never removed, the map can grow with session count.
  - That is acceptable for the first fix if kept simple, but it should be noted as a bounded in-process memory tradeoff.
  - The injected/testable server helper should be the only new entry point for session-drain logic. If production code and tests use different helpers, the concurrency tests will not prove the real path.
  - This change does not fix the separate P1 queue-claim race across multiple processes in `src/store.rs`; it only removes same-process global turn serialization and long-held store mutexes.
