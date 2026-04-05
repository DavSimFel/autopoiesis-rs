# Architecture - How the Code Works Today

> Current state only. For future direction, see [../vision.md](../vision.md). For known hazards, see [../risks.md](../risks.md).

## Snapshot

- Rust source files in `src/`: `104`
- Lines of Rust in `src/`: `38,142`
- Lines of Rust in `tests/`: `1,901`
- Commits on `HEAD`: `178`

## Module Map

- `src/main.rs` - CLI entrypoint, server launch, tracing setup.
- `src/app/enqueue_command.rs` - CLI queue-only entrypoint for registry-backed sessions.
- `src/terminal_ui.rs` - CLI presentation and denial formatting.
- `src/tui/{mod,event,state,bridge,render,input}.rs` - optional ratatui TUI (`--features tui`).
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
- `src/server/ws.rs` - WebSocket streaming, approval prompts, and terminal protocol-error shutdown.
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
- `src/plan/notify.rs` - notifications back to T2.
- `src/plan/patch.rs` - plan patching and revision handling.
- `src/plan/recovery.rs` - crash recovery for stalled plan runs.
- `src/config/mod.rs` - config facade; `src/config/{runtime,load,spawn_runtime,agents,models,domains,policy,file_schema}.rs` own runtime state, validation, and schema parsing.
- `src/context/mod.rs` - context facade and public reexports.
- `src/context/{identity_prompt,skill_summaries,skill_instructions,session_manifest,subscriptions,history}.rs` - focused context sources and prompt assembly helpers.
- `src/session/mod.rs` - JSONL history and per-session metadata.
- `src/session_registry.rs` - registry materialization for startup and CLI/server routing.
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
Config -> SessionRegistry
  -> startup bootstrap of declared session rows
  -> queue-owned always-on workers paused by active websocket sessions
  -> CLI enqueue / HTTP / WS
  -> SQLite queue
  -> atomic claim
  -> build_turn_for_config(..., optional session manifest)
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
- `store/mod.rs` owns the sessions table, subscriptions, and plan runs.
- Declared registry-backed sessions are bootstrapped idempotently before server workers start.

### Session Registry and Routing

- `session_registry.rs` derives session ids and per-session runtime configs from loaded `agents.toml`.
- Always-on registry-backed sessions are queue-owned and each gets one persistent worker.
- Direct CLI mode does not auto-attach to queue-owned registry sessions; it only defaults to request-owned sessions when one exists.
- HTTP enqueues registry-backed always-on sessions to the queue worker path.
- WebSocket sessions mark the session active, pause the always-on worker with a count gate, drain inline, then clear the active mark on disconnect.
- Registry-backed non-always-on sessions reuse request-owned execution with the registry manifest.
- Non-registry sessions keep the legacy ad hoc request-owned path.
- `autopoiesis enqueue --session <id> "task"` is the explicit CLI path for queue-owned registry sessions.

### Tiered Turns

- T1 uses shell plus its identity stack and skill summaries.
- T2 uses `read_file` only, plus its identity stack and skill summaries.
- T3 uses shell and can receive full skill instructions when spawned.
- `build_turn_for_config()` remains the shared facade; registry-backed sessions thread the optional `## Available Sessions` manifest through the same builder path.

### Guard Pipeline

- Verdict order is deny, then approve, then allow.
- Guards cover inbound messages, tool calls, and outbound text.
- Shell safety is policy-driven and taint-aware.
- The shell guard stack is risk reduction, not sandboxing.

### Identity and Domains

- T1 loads constitution, agent, and context files.
- T2 and T3 load constitution and context only.
- Selected domains append `context_extend` files to the identity assembly.
- `src/shipped/identity-templates/` is the shipped prompt source of truth.

> **Legacy vs v2:** The `src/shipped/identity-templates/` layout above is the current live implementation. The design in `docs/specs/identity-v2.md` describes the intended layered model (operator.md, persona dimensions, guard rules) which is specified but not yet built. When in doubt, the code in `src/identity.rs` and `src/config/agents.rs` is the authority.

### Skills

- `skills.rs` discovers local TOML skill definitions.
- T1 and T2 receive summaries.
- Spawned T3 workers receive full skill instructions from the spawn path.

### Plan Engine

- T2 emits structured `plan-json` blocks.
- `plan.rs` and `src/plan/*` persist and execute plan runs.
- Shell steps and postcondition checks reuse `src/agent/shell_execute.rs`.
- Crash recovery marks stale running attempts crashed, moves the run to `waiting_t2`, and notifies the owner T2 session.

### TUI (optional, `--features tui`)

- Feature-gated ratatui-based terminal UI for direct interactive CLI sessions.
- Split architecture: a dedicated OS thread owns terminal rendering and input; an async worker loop on a tokio task owns all mutable session state.
- `src/tui/mod.rs` is the entry point (`run_tui()`); wires channels, spawns render thread, runs worker loop.
- `src/tui/event.rs` defines the `TuiEvent`/`TuiCommand` channel protocol.
- `src/tui/bridge.rs` implements `TokenSink`, `ApprovalHandler`, and `Observer` backed by the TUI event channel.
- `src/tui/state.rs` is pure state logic (unit-testable, no terminal deps).
- `src/tui/render.rs` and `src/tui/input.rs` handle ratatui drawing and crossterm key events.
- Observer injection is local: the TUI calls `drain_queue_with_store_observed()` with a `MultiObserver` containing both the runtime observer and a `TuiObserver`. The shared observer factory is unchanged.
- Tracing is redirected through TUI channel-backed writers via `init_tracing_for_tui()` to prevent alternate-screen corruption.
- `--tui` is rejected when combined with any subcommand. Without the `tui` feature, it returns a clear build error.

### Subscriptions

- Subscriptions are stored in SQLite with topics, filters, and token estimates.
- Topic is part of subscription identity; session-scoped rows only override globals within the same topic.
- They are durable and queryable today.
- They are wired into turn-context assembly alongside the identity and session manifest blocks.
