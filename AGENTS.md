# AGENTS.md

How to work in this repo. Read this first.

> **⚠️ Read [docs/risks.md](docs/risks.md) before trusting any invariant claim.** The guard pipeline, approval system, and queue semantics have known broken invariants.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # all must pass
cargo fmt --check              # must pass
cargo clippy -- -D warnings    # must pass
cargo test --features integration  # live API tests (skip if no auth)
```

**Every change must pass all four checks before committing.** Pre-commit hooks enforce this.

## Reading order

- **Fixing a bug:** this file → [docs/risks.md](docs/risks.md) → [docs/architecture/overview.md](docs/architecture/overview.md) → code
- **Building a feature:** this file → [docs/risks.md](docs/risks.md) → [docs/architecture/overview.md](docs/architecture/overview.md) → [docs/roadmap.md](docs/roadmap.md) → [docs/vision.md](docs/vision.md)
- **Understanding a spec:** [docs/index.md](docs/index.md) → relevant spec in [docs/specs/](docs/specs/)

## Project structure

```
src/                 27 Rust source files (~14.3K lines)
  gate/              Guard pipeline (budget, secret redaction, shell safety, exfil detection, protected paths)
  llm/               LLM provider trait + OpenAI backend
identity/            Runtime prompt files (constitution, identity, context)
identity-templates/  Git-tracked operator-authored identity files (future: identity v2)
agents.toml          Model config, shell policy, budget limits
sessions/            JSONL history + SQLite queue + subscriptions (gitignored)
tests/               Integration + shipped policy tests
docs/                Architecture, specs, risks, vision, roadmap (see docs/index.md)
```

## Architecture rules

1. **One tool.** Shell is the universal tool. Don't add tools — teach the prompt.
2. **Guard pipeline order.** Deny > Approve > Allow in `resolve_verdict()` (turn.rs).
3. **No `unsafe` outside tool.rs.** `set_resource_limits()`, `signal_process_group()`, and the `pre_exec` closure.
4. **Traits for composition.** ContextSource, Tool, Guard, LlmProvider, TokenSink, ApprovalHandler.
5. **Two paths share one Turn.** CLI and server both use `build_default_turn()`. Don't diverge.
6. **Specs sync on every merge.** Every merge that changes `src/` must update relevant docs. Pre-commit hook auto-updates architecture stats.

## Coding standards

### Junior readability rule
**A junior dev should be able to read any file and understand what it does.** This is the bar for all decisions below.

### Error handling
- `anyhow::Result` for orchestration, CLI wiring, and glue code.
- `thiserror` at boundaries where callers branch on failure kind: server HTTP responses, config loading.
- `.context("description")` on every fallible op. No bare `?`.
- No `.unwrap()` in non-test code. Never silently swallow errors.

### Logging
- Use `tracing` (not `log`/`env_logger`). Spans per agent turn for correlated debugging.
- `info` — lifecycle events (session start, turn complete, server bind).
- `warn` — policy denials, recoverable failures, tainted input.
- `debug` — state transitions, guard evaluations, token counts.
- `trace` — SSE frame parsing, JSONL line details (only when debugging wire issues).

### Comments and documentation
- **Comment policy decisions and security boundaries.** "Why this command is denied." "Why we strip tool_calls on denial."
- **Comment non-obvious invariants.** "Trimming never splits assistant/tool round-trips because..."
- **Don't restate code.** If the code needs a comment to be understood, the abstraction is probably too big.
- **Doc comments (///)** on public types, async entrypoints, and guard invariants.
- **Format spec at the top** of hand-rolled parsers (SSE parser, JSONL persistence).

### Module structure
- Split by **responsibility**, not by line count. Each file does one thing.
- `agent.rs` → agent loop, approval flow, tool execution (split when adding identity v2).
- `server.rs` → when splitting: `server/http.rs`, `server/ws.rs`, `server/sse.rs`. Protocol obvious from filename.
- **Separate policy from I/O from state mutation.** Never mix all three in one function.

### Async patterns
- **Small async fns for I/O.** Network calls, file reads, queue operations.
- **Sync helpers for policy and state mutation.** Guard evaluation, verdict resolution, session trimming.
- **`tokio::task::spawn_blocking`** for rusqlite. Keep DB work isolated in store.rs.
- **No nested closures** unless they genuinely simplify. Prefer named functions.
- **Small enums/structs for flow state** over giant match chains inside closures.

### Protocols
- **SSE** for streaming token output to clients (stateless, resumable, mobile-friendly).
- **WebSocket** for bidirectional needs only (approval prompts during execution).
- **HTTP** for control plane (sessions, health, queue management).
- Make the protocol split obvious in module names and routing.

### Parsers
- **SSE parser** (llm/openai.rs): hand-rolled, ~100 lines, 12 tests. Justified — format is tiny and fully owned. Add format spec comment at the top.
- **JSONL** (session.rs): `serde_json::from_str` per line. No library needed.
- If adding a new parser, prefer a library unless the format is trivial and fully tested.

### Dependencies
- **Minimize.** Justify every new crate in the PR description.
- **Current stack:** axum, tokio, rusqlite, reqwest, tiktoken-rs, serde/serde_json, clap, anyhow.
- **Next adds:** `tracing` + `tracing-subscriber` (observability), `thiserror` (typed errors at boundaries).
- Don't switch rusqlite → sqlx unless SQLite contention becomes a measured bottleneck.

### Testing
- **Unit tests** in `#[cfg(test)] mod tests` at the bottom of each source file.
- **Integration tests** in `tests/` behind feature flags.
- **Missing gap:** end-to-end tests that boot temp SQLite + exercise a full agent turn across modules.
- Test names describe behavior: `trim_drops_oldest_non_system_messages`.
- Unique temp dir names (timestamp-based) to avoid test interference.

## Git workflow

- Commit messages: `feat:`, `fix:`, `chore:`, `refactor:`, `docs:` prefix. Imperative mood.
- Pre-commit hooks run: fmt + clippy + test + secret scan + architecture stats auto-update.
- Direct to main for codex-loop work. Feature branches for parallel or experimental work.

## Common pitfalls

- **SSE parsing:** Events can split mid-JSON across byte boundaries. Don't assume complete lines.
- **Tool call IDs:** Assistant messages must include `ToolCall` content blocks for API replay.
- **Ghost tool entries:** Skip SSE tool events with no `call_id`. Never fall back to `"unknown"`.
- **JSONL ≠ SQLite queue.** JSONL persists history. SQLite handles delivery. Complementary, not redundant.
- **Process-group kill:** `setpgid(0,0)` + `killpg` — kills all descendants, not just parent shell.
- **Taint propagation:** Only User and System messages taint. Agent messages don't. Standing approvals skip when tainted.

## Pre-merge checklist

1. `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check` all pass.
2. No execution path consumes prompts without claiming a queue row.
3. Every claimed queue row ends in `processed` or `failed`.
4. Guard pipeline covers inbound, tool calls, and outbound text.
5. Session reload preserves system/assistant/tool messages + tool_call metadata.
6. Trimming never splits assistant/tool round-trips.
7. No secrets in code. Token files use `0600`.
8. Docs updated if `src/` changed.

## Don't

- Don't add tools. One tool.
- Don't add `#[allow(unused)]` to suppress real issues.
- Don't skip tests or weaken assertions.
- Don't put secrets in code.
- Don't mix policy, I/O, and state mutation in one function.
- Don't use nested closures where a named function works.
- Don't add dependencies without justification.
