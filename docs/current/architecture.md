# Architecture — How the Code Works Today

> **Current state only.** For future design, see [docs/vision.md](../vision.md).
> For known hazards, see [risks.md](risks.md).

## Overview

17 source files, ~7.9K lines. One binary: CLI (REPL or one-shot) + HTTP/WS server. One tool: shell. Run `cargo test` for current count.

## Module map

```
main.rs (213L)        CLI entrypoint, clap args, REPL loop, server launch
agent.rs (1400L)      Agent loop, turn orchestration, output cap, approval flow
                      TokenSink + ApprovalHandler traits for I/O decoupling
server.rs (1110L)     axum HTTP + WS, Principal-based auth, queue-driven agent exec
session.rs (956L)     JSONL persistence, token tracking, context trimming
llm/openai.rs (948L)  OpenAI Responses API, SSE streaming, incremental parsing
llm/mod.rs (185L)     LlmProvider trait, message types, tool call structs
guard.rs (675L)       SecretRedactor, ShellSafety (policy-driven), ExfilDetector
context.rs (383L)     ContextSource trait — Identity (prompt files), History (replay)
turn.rs (351L)        Turn composition: ContextSource + Tool + Guard builder
tool.rs (322L)        Shell tool: async exec, RLIMIT, process-group kill
store.rs (297L)       SQLite session registry + message queue
auth.rs (401L)        OAuth device flow, token storage/refresh
config.rs (264L)      agents.toml loading, ShellPolicy config
identity.rs (165L)    Loads identity/*.md, concatenates in order
template.rs (86L)     {{var}} placeholder resolution
util.rs (95L)         utc_timestamp(), helpers
lib.rs (14L)          Module re-exports
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

### Guard pipeline
Three guards in order: SecretRedactor → ShellSafety → ExfilDetector.
Verdict precedence: Deny > Approve > Allow. `resolve_verdict()` in turn.rs.
Guards check inbound messages, tool calls, and outbound text.
ShellSafety uses a configurable policy (`[shell]` in agents.toml) with allow/deny patterns and a default action (approve or allow).
**Note:** these are heuristics, not a security boundary. See [risks.md](risks.md).

### Shell output cap
Every shell result is saved to `sessions/{id}/results/{call_id}.txt`.
Below 4KB: also returned inline in history.
Above 4KB: only metadata pointer in history. Agent must read the file explicitly.

### Session persistence
Daily JSONL files in `sessions/{name}/`. Each line is a `SessionEntry` with role, content, optional tool_calls, optional metadata.
Session replay loads all day-files in chronological order.
Token tracking via tiktoken-rs (cl100k_base). Trimming drops oldest non-system messages.

### SQLite queue
`sessions/queue.sqlite` — session registry + message queue.
All sources (CLI, HTTP, WS) enqueue to the same queue. `drain_queue()` is the unified consumer for both CLI and server.
**Note:** queue claiming is not atomic across processes. See [risks.md](risks.md#p1-2).

### Two execution paths, one Turn
CLI and server both use `build_default_turn()` (takes `Config` with shell policy). The Turn composes Identity (context source), Shell (tool), and the guard pipeline. Differences are only in TokenSink (stdout vs WS) and ApprovalHandler (stdin vs WS/reject).

### Auth and principals
Server authenticates via API key header. Two keys supported: an operator key (full role control) and a user key (always enqueues as `user`). See `Principal` enum in server.rs.

### Identity system (v1)
Three files concatenated into the system prompt:
1. `constitution.md` — laws of thought (intended immutable, not yet enforced)
2. `identity.md` — name, voice, working style, coding conventions
3. `context.md` — model/cwd/tools template vars, workspace layout

Template variables resolved at runtime: `{{model}}`, `{{cwd}}`, `{{tools}}`.
