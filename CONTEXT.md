# CONTEXT.md — autopoiesis-rs

Synthesized from ~/.openclaw and ~/.openclaw.bak — David's personal input across all sessions.

---

## About David

- **Name:** David Feldhofer
- **Email:** david@feldhofer.cc | **Telegram:** @realmrggg (id: 5660535751)
- **Timezone:** Europe/Vienna (CET/CEST)
- Direct, dry, critical. No filler. Lead with substance.
- Truth over reassurance — unverified claims are a failure mode.
- Executes via delegation: orchestrators plan/review, Codex implements.
- When next steps are clear, execute — don't wait for permission.

---

## Project Identity

**autopoiesis-rs** — Autonomous AI agent runtime in Rust.
- Codebase: `/home/user/autopoiesis-rs`
- Stack: Rust (tokio, sqlx/SQLite, serde, axum)
- Size: ~38k lines, 606 tests
- Architecture: tiered execution (T1/T2/T3), guard pipeline, SQLite queue, plan engine
- Intended persona running on it: Silas

---

## Priority

**#2 — Maintenance only.** No proactive coding unless a real issue appears.

Global priority order:
1. wpp — urgent, profitability window closing
2. **aprs — maintenance only** ← this project
3. bitflip — deferred until wpp + aprs stable

---

## Current State (as of 2026-04-05)

- **Hold.** Maintenance mode. No proactive work.
- 606 tests passing — keep them passing.
- Architecture documentation deferred until wpp is stable.
- If a runtime or test issue appears, route through orchestrator with full delegation/audit rules.

---

## David's Rules for This Project

- Tests are non-negotiable. Nothing changes without green `cargo test`.
- Guard pipeline is the safety layer — changes require explicit reasoning and David's approval.
- Type system first. Prefer compile-time guarantees over runtime checks.
- Don't optimize prematurely. Correctness first, then performance.
- Read existing code before touching anything. 38k lines — context matters.
- No push to remote without David's approval.
- No direct code implementation by orchestrators. Ever.

---

## Agent Setup

- Orchestrator delegates all non-trivial work to Codex.
- Codex spawn: `exec pty:true workdir:/home/user/autopoiesis-rs background:true command:"codex --yolo exec '<prompt>'"` 
- Every Codex prompt must begin: *"First read /home/user/autopoiesis-rs/AGENTS.md..."*
- Report format: Status / Result / Files changed / Checks / Commit sha(s) / Recommendation

---

## Infrastructure Notes

- Zoekt code search running on :6070 (58MB RAM, 15min reindex cron) — set up 2026-04-04
- Scaffolded: Makefile, .githooks, linting, static analysis
- Codex sandbox can't git commit (read-only .git/index.lock) — commit from main session after Codex finishes

---

## Session History Highlights

- **2026-04-02:** David named the agent Silas. Set up local embedding. Fixed exec access and subagent spawning.
- **2026-04-03:** AGENTS.md updated with full operational block — coding standards, pre-merge checklist, approved/banned dependencies, Codex guidance.
- **2026-04-04:** wpp/aprs/bitflip orchestrator agents created. Zoekt live. Repo scaffolded.
- **2026-04-05:** Workflow standard locked: plan loop (Codex xhigh → Opus review) + impl loop (Sonnet impl → Codex mini review).
