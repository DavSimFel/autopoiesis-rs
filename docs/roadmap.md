# Roadmap — Sequencing and Priorities

> Updated: 2026-03-22

## Build order

### 1. Security stack (blocking everything else)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| ~~1a~~ | ~~P0 fixes (role injection, denial termination, shell default-approve)~~ | — | ~~3 PRs~~ ✓ |
| 1b | Standing approvals (`[shell.standing_approvals]` in agents.toml) | 1a | 1 PR |
| 1c | Taint tracking (`<meta principal="..."/>` on messages, guard integration) | 1b | 1 PR |
| 1d | Budget enforcement (per-turn/session/day token ceilings) | 1a | 1 PR |

**Why this order:** Standing approvals without taint = exploitable. Taint without standing approvals = unusably slow. Both together = practical + safe autonomy.

### 2. Context management (after security)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| 2a | Subscription system (SQLite table, `sub add/remove/list` CLI, filters) | 1a | 1-2 PRs |
| 2b | Context assembly rework (materialize subs in history by timestamp) | 2a | 1 PR |
| 2c | Topics (optional grouping on subscriptions, triggers, relations) | 2a | 1-2 PRs |

### 3. Identity + infrastructure (parallel track)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| 3a | Identity v2 (operator.md, persona dimensions, guard rules) | 1c | 1-2 PRs |
| 3b | Trigger evaluation (cron + webhook → enqueue) | 2c | 1 PR |
| 3c | Provider abstraction (Anthropic, local models) | — | 1 PR |
| 3d | Permissions/sandboxing (seccomp/landlock/uid-drop) | 1a | large |

## Done

- Agent loop (async, real SSE streaming with incremental parsing)
- Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- Guard pipeline (SecretRedactor, ShellSafety, ExfilDetector)
- Turn architecture (ContextSource + Tool + Guard trait composition)
- Approval system with severity levels + REPL prompt flow
- Session persistence (daily JSONL, tool_call round-trip, replay-safe)
- Identity system v1 (constitution + identity + context, template vars)
- Constitution v1 (4 laws, 1st person, research-backed)
- OAuth device flow auth
- Token estimation (tiktoken-rs) + context trimming
- SQLite message queue + session store (source-agnostic inbox)
- Unified drain_queue() for CLI and server
- axum HTTP server + WebSocket
- API key auth middleware (header + WS query param)
- Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler)
- Kill child process on shell timeout (process-group aware)
- CI pipeline (GitHub Actions: fmt + clippy + test)
- Shell output cap + file-backed result storage (4KB threshold)
- Persistent named sessions (`--session <name>`)
- Server path sanitization (session_id validation)
- Stale message recovery on startup
- **P0-1:** Shell default-approve with configurable policy — `[shell]` in agents.toml (#8)
- **P0-2:** HTTP role enforcement via Principal enum (#6)
- **P0-3:** Approval denial terminates turn — MAX_DENIALS_PER_TURN + break (#7)
