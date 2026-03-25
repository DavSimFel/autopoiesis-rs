# Architecture - How the Code Works Today

> **Current state only.** For future design, see [../vision.md](../vision.md).
> For known hazards, see [../risks.md](../risks.md).

## Overview

Source tree and test coverage are current. The plan-engine placeholder tests remain ignored.

## Module Map

main.rs                     CLI entrypoint, clap args, REPL loop, server launch, tracing setup
agent.rs                    Agent loop, turn orchestration, output cap, approval flow
                            TokenSink + ApprovalHandler traits for I/O decoupling
                            Denied tool calls persist text-only (no broken tool_call replay)
server/                     axum HTTP + WS, Principal-based auth, queue-driven agent exec
session.rs                  JSONL persistence, token tracking, replay warnings, context trimming,
                            budget snapshots
subscription.rs             Subscription data layer: filters, path normalization, content loading,
                            token utilization
llm/openai.rs               OpenAI Responses API, SSE streaming, incremental parsing,
                            trailing-buffer event handling
llm/mod.rs                  LlmProvider trait, message types, tool call structs
model_selection.rs          Fail-closed model catalog/routing selection
gate/mod.rs                 Guard trait, Verdict/Severity enums, guard_text_output/guard_message_output
gate/shell_safety.rs        Policy-driven shell allow/deny, standing approvals (taint-gated),
                            compound command detection, protected credential path denial
gate/secret_redactor.rs     Regex-based secret redaction in message content
gate/streaming_redact.rs    Byte-by-byte secret redaction during SSE streaming
gate/exfil_detector.rs      Cross-call read+send pattern detection
gate/budget.rs              Per-turn/session/day token ceiling enforcement
gate/output_cap.rs          Shell output cap + file-backed result storage
gate/secret_patterns.rs     Shared secret pattern catalog + protected path detection + env wrapper stripping
context.rs                  ContextSource trait - Identity (prompt files), Skills (discovery),
                            History (replay, not wired)
turn.rs                     Turn composition: tier-aware ContextSource + Tool + Guard builder, taint via is_taint_source()
tool.rs                     Shell tool: async exec, RLIMIT, process-group kill, bounded output drain
store.rs                    SQLite session registry + message queue + subscriptions table
auth.rs                     OAuth device flow, token storage/refresh
config.rs                   agents.toml loading, ShellPolicy, BudgetConfig, skill catalog loading,
                            resolved identity file lists
skills.rs                   Local TOML skill catalog, discovery summaries, lookup by name
spawn.rs                    Child-session creation, tier/model resolution, budget preflight, completion enqueueing
identity.rs                 Loads explicit identity file lists and template helpers
principal.rs                Principal enum (Operator/User/System/Agent), trust/taint source mapping
cli.rs                      CLI display helpers, denial formatting
template.rs                 `{{var}}` placeholder resolution
util.rs                     `utc_timestamp()`, helpers
lib.rs                      Module re-exports

## Execution Flow

```
sources (CLI/HTTP/WS) -> SQLite queue -> drain_queue() -> agent loop -> responses

agent loop (run_agent_loop):
  1. ensure_context_within_limit() - trim if over budget
  2. clone session history + append current user message
  3. check_inbound() - guard pipeline on inbound messages
  4. stream_completion() - call LLM, stream tokens via TokenSink
  5. if tool_calls:
     a. check_tool_call() - guard pipeline on each call
     b. if Approve verdict -> request_approval() via ApprovalHandler
     c. if denied -> break after MAX_DENIALS_PER_TURN (2)
     d. execute_tool() -> shell execution
     e. guard_text_output() - redact secrets in output
     f. cap_tool_output() - save to file, cap inline at 4KB
     g. persist tool result -> loop back to step 1
  6. if Stop -> persist assistant message, return
```

## Key Design Points

### Tiered Tools

Shell (`sh -lc`) remains the execution tool for T1 and T3. T2 uses the structured `read_file`
API only. Tool selection happens in `turn.rs`, not in the agent loop or provider layers.

### Guard Pipeline

Guards in order for shell-backed turns: BudgetGuard -> SecretRedactor -> ShellSafety ->
ExfilDetector. T2 skips ShellSafety and ExfilDetector because it uses `read_file` only.
BudgetGuard is only wired when `[budget]` config exists.
Verdict precedence: Deny > Approve > Allow in `resolve_verdict()` (`turn.rs`).
Guards check inbound messages, tool calls, and outbound text.
ShellSafety uses a configurable policy (`[shell]` in `agents.toml`) with allow/deny patterns,
standing approvals (skipped when tainted), and a default action.
`GuardContext` carries `tainted: bool` + `BudgetSnapshot`. Taint is set when any message in
history has a `User` or `System` principal via `Principal::is_taint_source()`. Agent-authored
messages do not taint.
These are heuristics, not a security boundary. See [../risks.md](../risks.md).

### Model Routing

Model selection is config-only in this phase. `models.catalog` stores the enabled provider/model
definitions, `models.routes` maps task kinds to preferred catalog keys, and `models.default` is
the fail-closed fallback. Unknown task kinds do not invent a model; if routing does not produce
an enabled catalog entry, spawn fails. `spawn_child()` resolves the final catalog key before
persistence and stores the resolved name, resolved provider model, and concrete tier in child
metadata. `spawn_and_drain()` reads that metadata back, rebuilds the child `Config`, and then
drains the child queue with the tier-specific `Turn`.

Pre-spawn budget validation happens before the child session is created. It checks the live
`BudgetSnapshot` against session/day ceilings only; the per-turn ceiling remains part of the
agent loop guard pipeline, not child spawn preflight.

### Shell Output Cap

Every shell result is saved to `sessions/{id}/results/call_<sanitized_call_id>.txt`
(call_id is sanitized for filesystem safety).
Below 4KB: also returned inline in history.
Above 4KB: only metadata pointer in history. Agent must read the file explicitly.

## Session Persistence

Daily JSONL files in `sessions/{name}/`. Each line is a `SessionEntry` with role, content,
optional tool_calls, and optional metadata.
Session replay loads all day-files in chronological order.
Token tracking via `tiktoken-rs` (`cl100k_base`). Trimming drops oldest non-system messages.
Replay warns and drops unknown roles or malformed tool entries, and trimming keeps assistant/tool
round-trips intact.

### SQLite Queue

`sessions/queue.sqlite` stores the session registry and message queue.
All sources (CLI, HTTP, WS) enqueue to the same queue. CLI uses `agent::drain_queue()`, server
uses `server::drain_session_queue()` - both ultimately call `agent::process_queued_message()`.
Queue claims are atomic across SQLite processes via `UPDATE ... RETURNING`. Startup recovery only
requeues `processing` rows whose `claimed_at` is older than the configured stale threshold.

### Two Execution Paths, One Turn

CLI and server both use `build_turn_for_config()` (which falls back to `build_default_turn()`
for T1/backward compat). The `Turn` composes Identity, the tier-selected tool surface, and the
guard pipeline. Differences are only in `TokenSink` (stdout vs WS) and `ApprovalHandler`
(stdin vs WS/reject).

### Subscriptions

Agent subscribes to files via CLI (`sub add <path>`). SQLite `subscriptions` table uses a unique
`(topic, path)` index. Filters: Full (default), Lines, Regex, Head, Tail, Jq. Content is loaded
from disk, filtered, and token-estimated via tiktoken. `sub add/remove` print total token
utilization. Effective timestamp = `max(activated_at, file_mtime)`. Not yet wired into context
assembly (roadmap 2b).

### Auth and Principals

Server authenticates via API key header. Two keys: operator key (full role control) and user
key (always enqueues as `user`). `Principal` in `principal.rs`: Operator (trusted), User,
System, Agent (all tainted). Taint propagates through `GuardContext` - standing approvals are
skipped when tainted.

### Identity System

Runtime identity is assembled from explicit file lists:
1. `identity-templates/constitution.md` - global laws of thought
2. `identity-templates/agents/silas/agent.md` - T1 voice, worldview, defaults, edges
3. `identity-templates/context.md` - model/cwd/tools template vars, workspace layout

T1 loads constitution + agent + context. T2/T3 load constitution + context only. Selected domain
packs append to the identity file list when configured. Template variables resolved at runtime:
`{{model}}`, `{{cwd}}`, `{{tools}}`.

See [../specs/identity-v2.md](../specs/identity-v2.md) for the current stack and config shape.
T2/T3 continue to load constitution + context only; T1 also loads `agent.md`.
