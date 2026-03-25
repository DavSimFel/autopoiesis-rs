# Architecture — How the Code Works Today

> **Current state only.** For future design, see [../vision.md](../vision.md).
> For known hazards, see [../risks.md](../risks.md).

## Overview

29 source files, ~17.8K lines, 288 tests (287 run, 1 ignored).

## Module map

```
main.rs (420L)              CLI entrypoint, clap args, REPL loop, server launch, sub add/remove/list
agent.rs (2041L)            Agent loop, turn orchestration, output cap, approval flow
                            TokenSink + ApprovalHandler traits for I/O decoupling
                            Denied tool calls persist text-only (no broken tool_call replay)
server.rs (1559L)           axum HTTP + WS, Principal-based auth, queue-driven agent exec
session.rs (1296L)          JSONL persistence, token tracking, context trimming, budget snapshots
subscription.rs (592L)      Subscription data layer: filters, path normalization, content loading, token utilization
llm/openai.rs (1016L)       OpenAI Responses API, SSE streaming, incremental parsing
llm/mod.rs (215L)           LlmProvider trait, message types, tool call structs
model_selection.rs          Fail-closed model catalog/routing selection
gate/mod.rs (324L)          Guard trait, Verdict/Severity enums, guard_text_output/guard_message_output
gate/shell_safety.rs (718L) Policy-driven shell allow/deny, standing approvals (taint-gated),
                            compound command detection, protected credential path denial
gate/secret_redactor.rs (278L)  Regex-based secret redaction in message content
gate/streaming_redact.rs (346L) Byte-by-byte secret redaction during SSE streaming
gate/exfil_detector.rs (279L)   Cross-call read+send pattern detection
gate/budget.rs (199L)       Per-turn/session/day token ceiling enforcement
gate/output_cap.rs (215L)   Shell output cap + file-backed result storage
gate/secret_patterns.rs (778L)  Shared secret pattern catalog + protected path detection + env wrapper stripping
context.rs (383L)           ContextSource trait — Identity (prompt files), History (replay, not wired)
turn.rs (723L)              Turn composition: ContextSource + Tool + Guard builder, taint via is_taint_source()
tool.rs (665L)              Shell tool: async exec, RLIMIT, process-group kill, bounded output drain
store.rs (715L)             SQLite session registry + message queue + subscriptions table
auth.rs (401L)              OAuth device flow, token storage/refresh
config.rs                    agents.toml loading, ShellPolicy, BudgetConfig, resolved identity file lists
spawn.rs                     Child-session creation, model resolution, budget preflight, completion enqueueing
identity.rs                  Loads explicit identity file lists and template helpers
principal.rs (92L)          Principal enum (Operator/User/System/Agent), trust/taint source mapping
cli.rs (116L)               CLI display helpers, denial formatting
template.rs (86L)           {{var}} placeholder resolution
util.rs (96L)               utc_timestamp(), helpers
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
**Note:** these are heuristics, not a security boundary. See [../risks.md](../risks.md).

### Model routing
Model selection is config-only in this phase. `models.catalog` stores the enabled provider/model definitions, `models.routes` maps task kinds to preferred catalog keys, and `models.default` is the fail-closed fallback. Unknown task kinds do not invent a model; if routing does not produce an enabled catalog entry, spawn fails. `spawn_child()` resolves the final catalog key before persistence and stores the resolved name in child metadata for later runtime wiring.

Pre-spawn budget validation happens before the child session is created. It checks the live `BudgetSnapshot` against session/day ceilings only; the per-turn ceiling remains part of the agent loop guard pipeline, not child spawn preflight.

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
Queue claims are atomic across SQLite processes via `UPDATE ... RETURNING`. Startup recovery only requeues `processing` rows whose `claimed_at` is older than the configured stale threshold.

### Two execution paths, one Turn
CLI and server both use `build_default_turn()` (takes `Config` with shell policy). The Turn composes Identity (context source), Shell (tool), and the guard pipeline. Differences are only in TokenSink (stdout vs WS) and ApprovalHandler (stdin vs WS/reject).

### Subscriptions
Agent subscribes to files via CLI (`sub add <path>`). SQLite `subscriptions` table with unique (topic, path) index. Filters: Full (default), Lines, Regex, Head, Tail, Jq. Content loaded from disk, filtered, and token-estimated via tiktoken. `sub add/remove` print total token utilization. Effective timestamp = `max(activated_at, file_mtime)`. Not yet wired into context assembly (roadmap 2b).

### Auth and principals
Server authenticates via API key header. Two keys: operator key (full role control) and user key (always enqueues as `user`). `Principal` enum in `principal.rs`: Operator (trusted), User, System, Agent (all tainted). Taint propagates through `GuardContext` — standing approvals are skipped when tainted.

### Identity system (v2 — current)
Runtime identity is assembled from explicit file lists:
1. `identity-templates/constitution.md` — global laws of thought
2. `identity-templates/agents/silas/agent.md` — T1 voice, worldview, defaults, edges
3. `identity-templates/context.md` — model/cwd/tools template vars, workspace layout

T1 loads constitution + agent + context. T2/T3 load constitution + context only. Selected domain packs append to the identity file list when configured. Template variables resolved at runtime: `{{model}}`, `{{cwd}}`, `{{tools}}`.

See [../specs/identity-v2.md](../specs/identity-v2.md) for the current stack and config shape.
