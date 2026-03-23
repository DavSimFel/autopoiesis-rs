# AGENTS.md

How to work in this repo. Read this first.

> **⚠️ Read [docs/current/risks.md](docs/current/risks.md) before trusting any invariant claim.** The guard pipeline, approval system, and queue semantics have known broken invariants.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # all must pass
cargo fmt --check              # must pass
cargo clippy -- -D warnings    # must pass
cargo test --features integration  # live API tests (skip if no auth)
```

**Every change must pass all four checks before committing.**

## Reading order

- Fixing a bug: this file → [docs/current/risks.md](docs/current/risks.md) → [docs/current/architecture.md](docs/current/architecture.md) → code
- Building a feature: this file → [docs/current/risks.md](docs/current/risks.md) → [docs/current/architecture.md](docs/current/architecture.md) → [docs/roadmap.md](docs/roadmap.md) → [docs/vision.md](docs/vision.md)

## Project structure

```
src/                 27 Rust source files (~13.4K lines)
  gate/              Guard pipeline (budget, secret redaction, shell safety, exfil detection, protected paths)
  llm/               LLM provider trait + OpenAI backend
identity/            Runtime prompt files (constitution, identity, context)
agents.toml          Model config, shell policy, budget limits
sessions/            JSONL history + SQLite queue + subscriptions (gitignored)
tests/               Integration + shipped policy tests
docs/current/        How the code works today + known risks
docs/vision.md       Future-state design
docs/roadmap.md      Build order and priorities
research/            Non-authoritative explorations
```

## Architecture rules (intended — see risks.md for what's broken)

1. **One tool.** Shell is the universal tool. Don't add tools — teach the prompt.
2. **Guard pipeline order.** Deny > Approve > Allow in `resolve_verdict()` (turn.rs). Note: this is local verdict precedence only — see [risks.md](docs/current/risks.md) for orchestration-layer gaps.
3. **No `unsafe` outside tool.rs.** `set_resource_limits()`, `signal_process_group()`, and the `pre_exec` closure — all in tool.rs.
4. **Traits for composition.** ContextSource, Tool, Guard, LlmProvider, TokenSink, ApprovalHandler.
5. **Two paths share one Turn.** CLI and server both use `build_default_turn()`. Don't diverge.

## Coding conventions

- `anyhow::Result` everywhere. `.context("description")` on fallible ops.
- No `.unwrap()` in non-test code. Never silently swallow errors indicating corruption or I/O failure.
- tokio for async. `tokio::time::timeout` for shell. `libc::killpg` for timeout cleanup.
- serde + serde_json. JSONL for sessions.
- Unit tests in `#[cfg(test)] mod tests` at the bottom of each source file. Integration tests go in `tests/` (behind feature flags). Unique temp dir names (timestamp-based).
- Test functions describe behavior: `trim_drops_oldest_non_system_messages`.

## Git workflow

- Never commit directly to `main` — use feature branches (`feat/*`, `fix/*`, `chore/*`).
- Commit messages: `feat:`, `fix:`, `chore:`, `refactor:` prefix. Imperative mood.
- Run tests before committing. Create PR with `gh pr create --base main`.

## Common pitfalls

- **SSE parsing:** Events can split mid-JSON across byte boundaries. Don't assume complete lines.
- **Tool call IDs:** Assistant messages must include `ToolCall` content blocks for API replay.
- **Ghost tool entries:** Skip SSE tool events with no `call_id`. Never fall back to `"unknown"`.
- **JSONL ≠ SQLite queue.** JSONL persists history. SQLite handles delivery. Complementary, not redundant.
- **Process-group kill:** `setpgid(0,0)` + `killpg` — kills all descendants, not just parent shell.

## Pre-merge checklist

1. `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check` all pass.
2. No execution path consumes prompts without claiming a queue row.
3. Every claimed queue row ends in `processed` or `failed`.
4. Guard pipeline covers inbound, tool calls, and outbound text.
5. Session reload preserves system/assistant/tool messages + tool_call metadata.
6. Trimming never splits assistant/tool round-trips.
7. No secrets in code. Token files use `0600`.

## Don't

- Don't add tools. One tool.
- Don't add `#[allow(unused)]` to suppress real issues.
- Don't skip tests or weaken assertions.
- Don't put secrets in code.
- Don't commit directly to main.
