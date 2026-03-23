# Architecture — How the Code Works Today

> **Current state only.** For future design, see [docs/vision.md](../vision.md).
> For known hazards, see [risks.md](risks.md).

## Overview

27 source files, ~13.4K lines, 209 tests (208 run, 1 ignored). One binary: CLI (REPL or one-shot) + HTTP/WS server. One tool: shell. CLI also exposes `sub add/remove/list` for subscription management. Run `cargo test` for current count.

## Module map

```
main.rs (213L)              CLI entrypoint, clap args, REPL loop, server launch
agent.rs (1959L)            Agent loop, turn orchestration, output cap, approval flow
                            TokenSink + ApprovalHandler traits for I/O decoupling
                            Denied tool calls persist text-only (no broken tool_call replay)
server.rs (1558L)           axum HTTP + WS, Principal-based auth, queue-driven agent exec
session.rs (1221L)          JSONL persistence, token tracking, context trimming, budget snapshots
subscription.rs (592L)      Subscription data layer: filters, path normalization, content loading, token utilization
llm/openai.rs (1016L)       OpenAI Responses API, SSE streaming, incremental parsing
llm/mod.rs (215L)           LlmProvider trait, message types, tool call structs
gate/mod.rs (324L)          Guard trait, Verdict/Severity enums, guard_text_output/guard_message_output
gate/shell_safety.rs (716L) Policy-driven shell allow/deny, standing approvals (taint-gated),
                            compound command detection, protected credential path denial
gate/secret_redactor.rs (278L)  Regex-based secret redaction in message content
gate/streaming_redact.rs (346L) Byte-by-byte secret redaction during SSE streaming
gate/exfil_detector.rs (279L)   Cross-call read+send pattern detection
gate/budget.rs (199L)       Per-turn/session/day token ceiling enforcement
gate/output_cap.rs (175L)   Shell output cap + file-backed result storage
gate/secret_patterns.rs (778L)  Shared secret pattern catalog + protected path detection + env wrapper stripping
context.rs (383L)           ContextSource trait — Identity (prompt files), History (replay, not wired)
turn.rs (719L)              Turn composition: ContextSource + Tool + Guard builder, taint via is_taint_source()
tool.rs (322L)              Shell tool: async exec, RLIMIT, process-group kill
store.rs (509L)             SQLite session registry + message queue + subscriptions table
auth.rs (401L)              OAuth device flow, token storage/refresh
config.rs (420L)            agents.toml loading, ShellPolicy, BudgetConfig, protected path config
identity.rs (165L)          Loads identity/*.md, concatenates in order
principal.rs (92L)          Principal enum (Operator/User/System/Agent), trust/taint source mapping
cli.rs (116L)               CLI display helpers, denial formatting
main.rs (411L)              CLI entrypoint, clap args, REPL loop, server launch, sub add/remove/list
template.rs (86L)           {{var}} placeholder resolution
util.rs (95L)               utc_timestamp(), helpers
lib.rs (19L)                Module re-exports
```

## Execution flow

```
sources (CLI/HTTP/WS) ──→ SQLite queue ──→ drain_queue() ──→ agent loop ──→ responses

agent loop (run_agent_loop):
  1. ensure_context_within_limit() — trim if over budget
  2. clone session history + append current user message
  3. check_inbound() — guard pipeline on inbound messages
  4. stream_completion() — call LLM, stream tokens via TokenSink
  5. if tool_calls:
     a. check_tool_call() — guard pipeline on each call
     b. if Approve verdict → request_approval() via ApprovalHandler
     c. if denied → break after MAX_DENIALS_PER_TURN (2)
     d. execute_tool() → shell execution
     e. guard_text_output() — redact secrets in output
     f. cap_tool_output() — save to file, cap inline at 4KB
     g. persist tool result → loop back to step 1
  6. if Stop → persist assistant message, return
```

## Key design points

### One tool
Shell (`sh -lc`) is the only tool. All capabilities come from the prompt teaching the agent what shell commands to run. The tool surface never grows.

### Guard pipeline (gate/)
Guards in order: BudgetGuard → SecretRedactor → ShellSafety → ExfilDetector.
BudgetGuard is only wired when `[budget]` config exists.
Verdict precedence: Deny > Approve > Allow. `resolve_verdict()` in turn.rs.
Guards check inbound messages, tool calls, and outbound text.
ShellSafety uses a configurable policy (`[shell]` in agents.toml) with allow/deny patterns, standing approvals (skipped when tainted), and a default action.
`GuardContext` carries `tainted: bool` + `BudgetSnapshot`. Taint is set when any message in history has a `User` or `System` principal (via `Principal::is_taint_source()`). Agent-authored messages do not taint.
**Note:** these are heuristics, not a security boundary. See [risks.md](risks.md).

### Shell output cap
Every shell result is saved to `sessions/{id}/results/call_<sanitized_call_id>.txt` (call_id is sanitized for filesystem safety).
Below 4KB: also returned inline in history.
Above 4KB: only metadata pointer in history. Agent must read the file explicitly.

### Session persistence
Daily JSONL files in `sessions/{name}/`. Each line is a `SessionEntry` with role, content, optional tool_calls, optional metadata.
Session replay loads all day-files in chronological order.
Token tracking via tiktoken-rs (cl100k_base). Trimming drops oldest non-system messages.

### SQLite queue
`sessions/queue.sqlite` — session registry + message queue.
All sources (CLI, HTTP, WS) enqueue to the same queue. CLI uses `agent::drain_queue()`, server uses `server::drain_session_queue()` — both ultimately call `agent::process_queued_message()`.
**Note:** queue claiming is not atomic across processes. See [risks.md](risks.md#p1-2).

### Two execution paths, one Turn
CLI and server both use `build_default_turn()` (takes `Config` with shell policy). The Turn composes Identity (context source), Shell (tool), and the guard pipeline. Differences are only in TokenSink (stdout vs WS) and ApprovalHandler (stdin vs WS/reject).

### Subscriptions
Agent subscribes to files via CLI (`sub add <path>`). SQLite `subscriptions` table with unique (topic, path) index. Filters: Full (default), Lines, Regex, Head, Tail, Jq. Content loaded from disk, filtered, and token-estimated via tiktoken. `sub add/remove` print total token utilization. Effective timestamp = `max(activated_at, file_mtime)`. Not yet wired into context assembly (roadmap 2b).

### Auth and principals
Server authenticates via API key header. Two keys: operator key (full role control) and user key (always enqueues as `user`). `Principal` enum in `principal.rs`: Operator (trusted), User, System, Agent (all tainted). Taint propagates through `GuardContext` — standing approvals are skipped when tainted.

### Identity system (v1)
Three files concatenated into the system prompt:
1. `constitution.md` — laws of thought (intended immutable, not yet enforced)
2. `identity.md` — name, voice, working style, coding conventions
3. `context.md` — model/cwd/tools template vars, workspace layout

Template variables resolved at runtime: `{{model}}`, `{{cwd}}`, `{{tools}}`.
