# Roadmap

> Updated: 2026-03-27
> Status: Phases 1-5, Plan Engine, Observability, and Code Quality shipped.

## Completed Phases

### Phase 1 — Foundation
- `tracing` instrumentation, span-aware turns
- Tier config from `agents.toml`
- Identity stack: constitution.md + agent.md (T1 only) + context.md
- Template-driven prompt assembly with `{{model}}`, `{{cwd}}`, `{{tools}}`
- Model catalog, shell policy, read policy, queue config loading

### Phase 2 — Model Routing and Delegation
- Fail-closed model selection from catalog
- Delegation thresholds (token + tool depth)
- Spawn-time budget checks for child sessions
- Resolved model/tier metadata storage for children

### Phase 3 — T2 Capability Layer
- Structured `read_file` tool for T2 (no shell)
- Domain context extension loading
- T2-to-T3 spawning and handoff through queue

### Phase 4 — Skills
- Local TOML skill discovery
- Skill summaries for T1 and T2
- Full skill instructions for spawned T3 workers
- Skill budget checks, duplicate/unknown validation

### Phase 5 — Hardening
- Module split: `agent/`, `server/`, `gate/`, `plan/`, `turn/`, `config/`, `context/`, `session/`, `store/`
- Queue claim recovery and session locking
- Shell policy hardening: protected paths, metacharacters, standing approvals, taint-aware
- Disk-backed shell output capping
- Durable session, subscription, and plan storage
- HTTP + WebSocket server paths

### Plan Engine
- Structured `plan-json` parsing from T2
- Durable plan runs and step attempts in SQLite
- Guarded shell execution reuse for plan steps and checks
- Crash recovery and T2 failure notifications
- CLI commands: `plan status`, `plan resume`, `plan cancel`, `plan list`

### Observability (2026-03-27)
- `src/observe/` module: 21 structured event types (`TraceEvent` enum)
- `Observer` trait with `NoopObserver`, `SqliteObserver`, `OtelObserver`, `MultiObserver`
- SQLite trace store (`traces.sqlite`, indexed by eval_run, session, plan_run)
- OTel exporter to OpenObserve (configurable via `ZO_OTEL_ENDPOINT`)
- `TracedVerdict` + `GuardTraceOutcome` for per-guard attribution
- Wired into: agent loop, shell execute, child drain, plan runner/patch/notify/recovery, server startup
- Zero overhead when disabled (`NoopObserver` default)

### Code Quality (2026-03-27)
- 9 cleanup sessions: all oversized files split
- Mock secrets replaced with non-secret fixtures
- Tracked `scripts/pre_commit_secret_scan.sh` with test-context awareness
- Test fixture deduplication into `src/test_support.rs`
- Queue/drain dedup, turn constructor unified
- Pre-commit hooks: fmt + clippy + test + secret scan

## What Remains

### P0 — Always-On Tier Architecture (NEXT)
Design is complete (see below). Implementation is next.

**The model:**
- T1 and T2 are always-on persistent sessions, created at startup from `agents.toml`
- Session IDs are well-known, derived from config (e.g. `silas-t1`, `silas-t2`)
- T3 is spawned by T2 (plan engine), ephemeral initially, reuse designed later
- Domain T2s configured explicitly in `agents.toml` (e.g. `silas-t2-finance`)
- Inter-tier communication = `enqueue_message(target_session_id, ...)` via queue
- No new transport, no special delegation mechanism

**What needs building:**
- `SessionRegistry` — expands `agents.toml` into per-session specs at startup
- Per-session drain loops (persistent, not opportunistic)
- Per-session turn builders (T1 gets shell, T2 gets read_file, domain T2s get context overlays)
- Runtime-injected capability manifest (peer sessions, domains, routing info)
- CLI queue-write via shell: `autopoiesis enqueue --session <id> "task"`
- Move T2 plan-json handling from child-drain-only into normal always-on T2 runtime
- Provenance: `causal_principal` + `reply_to_session` + `hop_count` on queue messages

### P1 — Eval Harness
- Eval runner skeleton: task definitions, graders, result storage
- TerminalBench adapter (first public benchmark)
- SWE-bench adapter
- Custom long-running scenarios (FlappyBird, online shop)
- Results shipped to OpenObserve via OTel

### P2 — Subscriptions v2
- Context wiring: materialize subscriptions in turn-context assembly by timestamp
- Topic model work beyond the current subscription topic field
- Topic export/import for cross-session context transfer

### P3 — Provider Abstraction
- Anthropic backend
- OpenRouter backend
- Provider trait already exists, just needs more implementations

### P4 — Security Stack
- Taint tracking: `<meta principal="..."/>` on messages, guard escalation
- Standing approvals: pattern-based pre-approval in agents.toml (partially shipped)
- Budget enforcement: per-turn/session/day ceilings (guards exist, enforcement partially wired)
- Permissions: seccomp/landlock (multi-tenant concern, not first priority)

### P5 — Runtime
- PTY shell support
- Filesystem/network sandboxing
- Hot reload of agents.toml

## Stats

- `src/` Rust source files: `107`
- Rust tests: `624`
- Commits: `183`
