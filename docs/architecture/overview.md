# Architecture - How the Code Works Today

> Current state only. For future direction, see [../vision.md](../vision.md). For known hazards, see [../risks.md](../risks.md).

## Snapshot

- Rust source files in `src/`: `104`
- Lines of Rust in `src/`: `38,213`
- Lines of Rust in `tests/`: `1,572`
- Commits on `HEAD`: `175`

## Module Map

- `src/main.rs` - CLI entrypoint, server launch, tracing setup.
- `src/terminal_ui.rs` - CLI presentation and denial formatting.
- `src/lib.rs` - crate re-exports.
- `src/agent/loop_impl.rs` - core agent loop and turn runner.
- `src/agent/audit.rs` - denial/audit persistence helpers and shared denial text formatting.
- `src/agent/usage.rs` - token charging and post-turn budget helpers.
- `src/agent/queue.rs` - queue draining for CLI/session work.
- `src/agent/shell_execute.rs` - guarded shell execution shared by the agent loop and plan engine.
- `src/agent/child_drain.rs` - child-session drain orchestration.
- `src/child_session/{mod,create,completion}.rs` - child-session creation, metadata, and parent completion propagation.
- `src/server/mod.rs` - server wiring.
- `src/server/http.rs` - HTTP control plane.
- `src/server/ws.rs` - WebSocket streaming and approval prompts.
- `src/server/auth.rs` - server-side auth handling.
- `src/server/queue.rs` - server queue draining.
- `src/gate/budget.rs` - token budget guard.
- `src/gate/shell_safety.rs` - shell policy, standing approvals, protected paths.
- `src/gate/secret_redactor.rs` - message-content redaction.
- `src/gate/exfil_detector.rs` - cross-call read/send detection.
- `src/gate/output_cap.rs` - shell output cap and artifact storage.
- `src/gate/streaming_redact.rs` - streaming redaction during SSE output.
- `src/gate/secret_catalog.rs` - secret prefixes, regexes, and streaming-redaction metadata.
- `src/gate/protected_paths.rs` - protected-path catalogs and normalized path checks.
- `src/gate/command_path_analysis.rs` - shell command heuristics for reads/writes to protected or target paths.
- `src/plan/runner.rs` - plan scheduler and execution loop.
- `src/plan/executor.rs` - step execution helpers.
- `src/plan/notify.rs` - notifications back to T2.
- `src/plan/patch.rs` - plan patching and revision handling.
- `src/plan/recovery.rs` - crash recovery for stalled plan runs.
- `src/config/mod.rs` - config facade; `src/config/{runtime,load,spawn_runtime,agents,models,domains,policy,file_schema}.rs` own runtime state, validation, and schema parsing.
- `src/context/mod.rs` - context facade and public reexports.
- `src/context/{identity_prompt,skill_summaries,skill_instructions,subscriptions,history}.rs` - focused context sources and prompt assembly helpers.
- `src/session/mod.rs` - JSONL history and per-session metadata.
- `src/store/mod.rs` - SQLite sessions, queue, subscriptions, and plan tables.
- `src/turn/mod.rs` - turn facade and public reexports.
- `src/turn/{verdicts,tiers,builders}.rs` - guard verdicts, tier resolution, and turn construction.
- `src/tool.rs` - shell tool execution primitives.
- `src/child_session/{mod,create,completion}.rs` - child session spawn helpers and completion propagation.
- `src/skills.rs` - skill catalog loading and summaries.
- `src/subscription.rs` - subscription records, filters, token accounting.
- `src/delegation.rs` - delegation configuration and thresholds.
- `src/model_selection.rs` - fail-closed model routing.
- `src/read_tool.rs` - structured read API for T2.
- `src/principal.rs` - caller principal and taint source mapping.
- `src/identity.rs` - identity file loading and prompt assembly.
- `src/template.rs` - template-variable resolution.
- `src/auth.rs` - OAuth/device auth.
- `src/llm/mod.rs` - provider trait and shared LLM types.
- `src/llm/openai/{mod,request,sse}.rs` - OpenAI backend, request shaping, and SSE parsing.
- `src/plan.rs` - plan orchestration and CLI state transitions.
- `src/logging.rs` - tracing formatter and user-output targets; `src/time.rs` now holds timestamp helpers.

## Execution Flow

```text
CLI / HTTP / WS
  -> SQLite queue
  -> atomic claim
  -> build_turn_for_config()
  -> identity + context + tier tool surface + guards
  -> LLM stream
  -> tool calls / approvals / guard checks
  -> JSONL session append + SQLite metadata
  -> token stream or persisted result
```

### Agent Loop

1. Claim a queued message.
2. Trim or reject if the budget guard requires it.
3. Build a tiered turn with identity, skills, context, and tool surface.
4. Stream the LLM response through the token sink.
5. For each tool call, run the guarded shell executor or the read tool.
6. Persist the assistant/tool round-trip back to session storage.

### Queue and Store

- CLI, HTTP, and WS all enqueue into the same SQLite-backed message queue.
- Queue claims are atomic.
- Startup recovery only requeues stale `processing` rows.
- `session/mod.rs` keeps JSONL history and tool-call metadata.
- `store/mod.rs` owns the registry tables for sessions, subscriptions, and plan runs.

### Tiered Turns

- T1 uses shell plus its identity stack and skill summaries.
- T2 uses `read_file` only, plus its identity stack and skill summaries.
- T3 uses shell and can receive full skill instructions when spawned.
- `build_turn_for_config()` is shared by CLI and server paths through the turn builder module.

### Guard Pipeline

- Verdict order is deny, then approve, then allow.
- Guards cover inbound messages, tool calls, and outbound text.
- Shell safety is policy-driven and taint-aware.
- The shell guard stack is risk reduction, not sandboxing.

### Identity and Domains

- T1 loads constitution, agent, and context files.
- T2 and T3 load constitution and context only.
- Selected domains append `context_extend` files to the identity assembly.
- `identity-templates/` is the runtime prompt source of truth.

### Skills

- `skills.rs` discovers local TOML skill definitions.
- T1 and T2 receive summaries.
- Spawned T3 workers receive full skill instructions from the spawn path.

### Plan Engine

- T2 emits structured `plan-json` blocks.
- `plan.rs` and `src/plan/*` persist and execute plan runs.
- Shell steps and postcondition checks reuse `src/agent/shell_execute.rs`.
- Crash recovery marks stale running attempts crashed, moves the run to `waiting_t2`, and notifies the owner T2 session.

### Subscriptions

- Subscriptions are stored in SQLite with filters and token estimates.
- They are durable and queryable today.
- They are not yet wired into turn-context assembly.
