# Roadmap

> Updated: 2026-03-24
> Origin: adversarial debate sessions (Silas × Codex), operator design decisions
> Specs: [specs/identity-v2.md](specs/identity-v2.md)

## Phase 1a — Observability (no dependencies)

| Item | Files | Scope |
|------|-------|-------|
| Replace eprintln/println with `tracing` | All files | 1 session |
| Spans per agent turn, guard eval, shell exec | agent.rs, turn.rs, tool.rs, server.rs | |
| `tracing-subscriber` with env filter | main.rs | |

**Risk:** Touches every file. Run full test suite after each module.

## Phase 1b — Foundation (after 1a)

| Item | Files | Scope |
|------|-------|-------|
| `[agents.silas]` with `.t1`/`.t2` subtables | config.rs, agents.toml | 2 sessions |
| `[models]` catalog + routes parsing | config.rs | |
| `[domains]` config parsing | config.rs | |
| Identity v2: configurable file list (not hardcoded triple) | identity.rs, context.rs | |
| agent.md loading for T1 | identity.rs, turn.rs | |
| `identity-templates/` in ProtectedPaths | gate/secret_patterns.rs | |
| Write Silas agent.md | identity-templates/agents/silas/agent.md | |

**Risk:** Changes every startup path. Bad config merge bricks CLI + server.
**Test:** New `[agents.silas]` config boots. Protected paths deny writes.

## Phase 2 — Model routing + delegation

| Item | Files | Scope |
|------|-------|-------|
| Fail-closed model selection from catalog | config.rs | 2-3 sessions |
| Budget check before T3 spawn | config.rs, agent.rs | |
| T1 delegation threshold mechanism | agent.rs | |
| Spawn API: create session with tier + model + task | agent.rs, store.rs, server.rs | |

**Risk:** Wrong defaults silently route to wrong model or block all delegation.
**Test:** Model selection returns correct model for task kind. Budget exceeded → spawn rejected. Unknown task kind → default model.

## Phase 3 — T2 capability layer

| Item | Files | Scope |
|------|-------|-------|
| T2 structured read API (provenance-tagged file reads) | new read_tool.rs | 3 sessions |
| T2 has read API only, no shell | turn.rs (tool selection by tier) | |
| Domain packs loaded into context assembly via `[domains] selected=[...]` | context.rs, identity.rs | |
| T2 session spawning (T1 → T2 with reasoning model) | agent.rs, store.rs | |
| T3 session spawning (T2 → T3 with catalog model) | agent.rs, store.rs | |
| T2→T1 handoff via message queue | store.rs, agent.rs | |

**Risk:** Provenance and access restrictions easy to regress if bolted onto current agent loop.
**Test:** T2 cannot access shell tool. T2 reads file → provenance tag present. T2 spawns T3 → T3 gets shell + skills. T2 conclusion arrives in T1 queue.

## Phase 4 — Skills

| Item | Files | Scope |
|------|-------|-------|
| Skill format definition (what a skill contains) | new skills.rs, `skills/` TOML files, docs | 2-3 sessions |
| Skill discovery (T1/T2 browse descriptions) | skills.rs, context.rs | |
| Skill loading (T3 gets preloaded by T2) | skills.rs, turn.rs, context.rs | |
| Catalog-based model selection for T3 | config.rs, agent.rs | |
| End-to-end: T2 → T3 with skill + model + task | integration test | |

**Risk:** Skill loading can explode context size. Fix load budget early.
**Test:** T3 context contains skill content. Skill token budget respected. Unknown skill → error, not silent skip.

## Phase 5 — Hardening

| Item | Files | Scope |
|------|-------|-------|
| Split agent.rs → agent loop + approval + tool exec | agent/ | 2 sessions |
| Split server/ → http + ws + auth + queue | server/ | |
| thiserror at boundaries (server responses, config) | server/, config.rs | |
| End-to-end integration tests (temp SQLite + full turn) | tests/ | |
| Persona stability eval (fixed scenario battery) | tests/constitution/ | |

**Risk:** Premature refactoring is churn. Only do after tier seam is stable.

## MVP Slice (proves the architecture)

One integration test with stubbed LLM:
1. T1 loads new config, selects model
2. T1 delegates task to T2
3. T2 reads files via structured read API
4. T2 spawns T3 with skill + model from catalog
5. T3 executes via shell, returns result
6. T2 writes conclusion → T1 reads it

If this passes, the tier architecture is real.

## Parallel tracks (after Phase 1b stable)

- **Track A:** config.rs + agents.toml (one Codex)
- **Track B:** identity.rs + context.rs (another Codex)
- **Track C:** test fixtures (third Codex)
- **Don't parallelize:** main.rs, server.rs, agent.rs until shared boot API defined

## Not building yet

- Topic export/import
- Multi-domain schedulers
- Persistent T2/T3 worktree orchestration
- Custom skill marketplace
- Provider abstraction (Anthropic, local models) — after tiers work
- Permissions/sandboxing (seccomp/landlock) — after tiers work
- PTY shell — after tiers work

## Completed

<details>
<summary>All done items (click to expand)</summary>

### Security stack ✓
- P0-1: Shell default-approve with configurable policy
- P0-2: HTTP role enforcement via Principal enum
- P0-3: Approval denial terminates turn
- P0-4: Shell metacharacter bypass — compound commands force approval
- P0-5: Credential file exposure — protected paths always denied
- P1-7: Taint over-fire — only User/System taint
- P1-8: Denied tool-call replay — text-only persistence
- Standing approvals (taint-gated)
- Taint tracking (Principal propagation)
- Budget enforcement (per-turn/session/day)

### Infrastructure ✓
- Agent loop (async, real SSE streaming)
- Shell tool (RLIMIT-sandboxed, process-group kill)
- Guard pipeline (SecretRedactor, ShellSafety, ExfilDetector)
- Turn architecture (ContextSource + Tool + Guard composition)
- Session persistence (daily JSONL, tool_call round-trip)
- Identity system v1 (constitution + identity + context)
- Constitution v1 (4 laws, 1st person)
- OAuth device flow auth
- Token estimation + context trimming
- SQLite message queue + session store
- axum HTTP + WebSocket server
- Subscription system (SQLite, CLI, filters, content loading)
- CI pipeline (GitHub Actions)
- Pre-commit hooks (fmt + clippy + test + secrets + auto-stats)
- Docs restructure (index, specs/, architecture/, archive/)
- Phase 6 plan engine (crash recovery, CLI commands, startup wiring)

</details>

## Estimates

| Phase | Sessions | Days |
|-------|----------|------|
| 1a (tracing) | 1 | 0.5 |
| 1b (foundation) | 2 | 1 |
| 2 (model routing) | 2-3 | 1-2 |
| 3 (T2 layer) | 3 | 1-2 |
| 4 (skills) | 2-3 | 1-2 |
| 5 (hardening) | 2 | 1 |
| **Total** | **12-14** | **5-8** |
