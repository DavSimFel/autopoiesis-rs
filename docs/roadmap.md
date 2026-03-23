# Roadmap — Sequencing and Priorities

> Updated: 2026-03-22

## Build order

### 1. Security stack (blocking everything else)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| ~~1a~~ | ~~P0 fixes (role injection, denial termination, shell default-approve)~~ | — | ~~3 PRs~~ ✓ |
| ~~1b~~ | ~~Standing approvals (`standing_approvals = [...]` under `[shell]` in agents.toml)~~ | ~~1a~~ | ~~PR #10~~ ✓ |
| ~~1c~~ | ~~Taint tracking (Principal enum, GuardContext.tainted, standing approvals gated)~~ | ~~1b~~ | ~~PR #11~~ ✓ |
| ~~1d~~ | ~~Budget enforcement (per-turn/session/day token ceilings via BudgetGuard)~~ | ~~1a~~ | ~~direct to main~~ ✓ |
| ~~1e~~ | ~~P0 fixes round 2 (shell metachar bypass, auth.json exposure, taint over-fire)~~ | ~~1a-1d~~ | ~~direct to main~~ ✓ |

**Note:** Security stack complete. All P0s resolved. Remaining P1s tracked in [risks.md](current/risks.md).

### 2. Context management (after security)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| ~~2a~~ | ~~Subscription system (SQLite table, `sub add/remove/list` CLI, filters)~~ | ~~1a~~ | ~~direct to main~~ ✓ |
| 2b | Context assembly rework (materialize subs in history by timestamp) | 2a | 1 PR |
| 2c | Topics (optional grouping on subscriptions, triggers, relations) | 2a | 1-2 PRs |

### 3. Identity + infrastructure (parallel track)

| # | Item | Depends on | Scope |
|---|------|-----------|-------|
| 3a | Identity v2 (operator.md, persona dimensions, guard rules) | 1c | 1-2 PRs |
| 3b | Trigger evaluation (cron + webhook → enqueue) | 2c | 1 PR |
| 3c | Provider abstraction (Anthropic, local models) | — | 1 PR |
| 3d | Permissions/sandboxing (seccomp/landlock/uid-drop) | 1a | large |
| 3e | PTY shell (persistent interactive sessions, SSH, REPLs) | 3a | large |
| 3f | Constitution evals in CI (automated scorecard from tests/constitution/) | — | 1 PR |

## Done

- Agent loop (async, real SSE streaming with incremental parsing)
- Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- Guard pipeline (SecretRedactor, ShellSafety, ExfilDetector)
- Turn architecture (ContextSource + Tool + Guard trait composition)
- Approval system with severity levels + REPL prompt flow
- Session persistence (daily JSONL, tool_call round-trip — P1-8 fixed: denied calls persist text-only)
- Identity system v1 (constitution + identity + context, template vars)
- Constitution v1 (4 laws, 1st person, research-backed)
- OAuth device flow auth
- Token estimation (tiktoken-rs) + context trimming
- SQLite message queue + session store (source-agnostic inbox)
- Queue draining for CLI (`agent::drain_queue()`) and server (`server::drain_session_queue()`), both via `process_queued_message()`
- axum HTTP server + WebSocket
- API key auth middleware (header + WS query param)
- Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler)
- Kill child process on shell timeout (process-group aware)
- CI pipeline (GitHub Actions: fmt + clippy + test)
- Shell output cap + file-backed result storage (4KB threshold)
- Persistent named sessions (`--session <name>`)
- Server path sanitization (session_id validation)
- Stale message recovery on startup (server path only, not CLI)
- **P0-1:** Shell default-approve with configurable policy — `[shell]` in agents.toml (#8)
- **P0-2:** HTTP role enforcement via Principal enum (#6)
- **P0-3:** Approval denial terminates turn — first denial breaks the loop; MAX_DENIALS_PER_TURN (2) affects summary text (#7)
- **1b:** Standing approvals — `standing_approvals` list under `[shell]` in agents.toml (#10)
- **1c:** Taint tracking — Principal propagation, GuardContext.tainted, standing approvals skipped when tainted (#11)
- **1d:** Budget enforcement — BudgetGuard with per-turn/session/day ceilings, session-total accounting
- Gate split refactor — guard.rs → 7 gate/ submodules + cli.rs
- Per-session server locking (replaced global worker_lock)
- **P0-4:** Shell metacharacter bypass — compound commands force approval
- **P0-5:** Credential file exposure — protected paths always denied
- **P1-7:** Taint over-fire — only User/System taint, Agent doesn't
- **P1-8:** Denied tool-call replay — text-only persistence on denial
- **2a:** Subscription system — SQLite table, `sub add/remove/list` CLI, filters (Full/Lines/Regex/Head/Tail/Jq), content loading, token utilization
