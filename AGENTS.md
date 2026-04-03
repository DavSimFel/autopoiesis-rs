# AGENTS.md

How to work in this repo. Read this first.

> **Global coding standards:** See [CODING_STANDARDS.md](/home/user/.openclaw/workspace/CODING_STANDARDS.md) — applies to all coding work. This repo's rules take precedence on conflicts.

> **⚠️ Read [docs/risks.md](docs/risks.md) before trusting any invariant claim.** Shell guards remain heuristic, there is no PTY or real sandboxing yet, and subscriptions are wired into turn context through `build_turn_for_config_with_subscriptions()`.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # all must pass
cargo fmt --check              # must pass
cargo clippy -- -D warnings    # must pass
cargo test --features integration  # live API tests (skip if no auth)
```

### Running specific tests

```bash
cargo test test_name                    # run a single test by name
cargo test module::submodule            # run all tests in a module
cargo test -- --nocapture test_name     # run with stdout visible
cargo test -p autopoiesis test_name     # explicit package (for when workspace grows)
```

Use file-scoped tests during iteration. Run the full suite before committing.

**Every change must pass all four checks before committing.** Pre-commit hooks enforce this.

## Reading Order

- Fixing a bug: this file -> [docs/risks.md](docs/risks.md) -> [docs/architecture/overview.md](docs/architecture/overview.md) -> code.
- Building a feature: this file -> [docs/risks.md](docs/risks.md) -> [docs/architecture/overview.md](docs/architecture/overview.md) -> [docs/roadmap.md](docs/roadmap.md) -> [docs/vision.md](docs/vision.md).
- Understanding a spec: [docs/index.md](docs/index.md) -> relevant spec in [docs/specs/](docs/specs/).

## Project Structure

```text
src/                 52 Rust source files (~34.8K lines)
  agent/             Turn loop, queue drain, spawn helpers, guarded shell executor
  server/            HTTP, WebSocket, auth, queue draining
  gate/              Budget, shell safety, redaction, exfil detection, output capping
  plan/              Plan runner, executor, patching, recovery, notifications
  llm/               Provider trait + OpenAI backend
identity-templates/  Git-tracked runtime prompt files (constitution, agent, context)
agents.toml          Model config, shell policy, read policy, queue limits, domains
sessions/            JSONL history + SQLite queue + subscriptions (gitignored)
tests/               Integration + shipped policy tests
docs/                Architecture, specs, risks, vision, roadmap
```

## Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI entrypoint, server launch, tracing setup |
| `src/agent/loop_impl.rs` | Core agent loop and turn orchestration |
| `src/agent/audit.rs` | Denial/audit persistence helpers and shared denial formatting |
| `src/agent/usage.rs` | Token charging and post-turn budget helpers |
| `src/agent/shell_execute.rs` | Shared guarded shell execution path |
| `src/turn/mod.rs` | Tier-aware turn assembly and guard composition facade |
| `src/turn/{builders,tiers,verdicts}.rs` | Focused turn construction and guard policy submodules |
| `src/store.rs` | SQLite sessions, queue, subscriptions, and plan tables |
| `src/plan/*.rs` | Plan execution, patching, notifications, recovery |
| `src/config/mod.rs` | `agents.toml` loading and policy/config validation across split submodules |
| `src/context/mod.rs` | Identity, skill, subscription, and history context facade |
| `src/context/{identity_prompt,skill_summaries,skill_instructions,subscriptions,history}.rs` | Focused context submodules |
| `src/subscription.rs` | Subscription records, filters, and token accounting |
| `src/skills.rs` | Skill catalog loading and summaries |
| `src/server/*.rs` | HTTP, WS, auth, and queue management |

## Architecture Rules

1. Tiered tools are real: T1 and T3 use shell, T2 uses `read_file` only.
2. Guard precedence is deny, then approve, then allow.
3. Shared shell execution must go through `src/agent/shell_execute.rs`.
4. `build_turn_for_config()` is the shared turn constructor for CLI and server paths.
5. `agent/`, `server/`, `gate/`, and `plan/` are the current major responsibility boundaries.
6. Docs must stay synced with `src/` changes in the same merge.
7. OpenTelemetry spans are wired via `opentelemetry-otlp` (gRPC/tonic). Do not add alternative exporters without discussion.

## Architecture Diagram

```text
CLI / HTTP / WS
   -> SQLite queue / session store
   -> drain queue
   -> build_turn_for_config()
   -> identity + context + skills + tier tool surface + guards
   -> LLM stream
   -> guarded shell or read tool
   -> JSONL session append + SQLite metadata
   -> response / token stream / plan notification

Plan engine:
T2 plan-json -> plan.rs / plan/* -> spawn or guarded shell -> checks -> notify T2
```

## Coding Standards

### Junior Readability Rule

A junior dev should be able to read any file and understand what it does.

### Error Handling

- Use `anyhow::Result` for orchestration, CLI wiring, and glue code.
- Use `thiserror` at boundaries where callers branch on failure kind.
- Add context to fallible operations.
- Do not use `.unwrap()` in non-test code.

### Logging

- Use `tracing`.
- `info` for lifecycle events.
- `warn` for policy denials and recoverable failures.
- `debug` for state transitions and guard evaluation.
- `trace` for wire-format parsing and queue details.

### Comments and Documentation

- Comment policy decisions and security boundaries.
- Comment non-obvious invariants.
- Do not restate code.
- Add doc comments on public types and async entrypoints.

### Module Structure

- Split by responsibility, not line count.
- Keep policy separate from I/O and state mutation.
- Prefer named helpers over nested closures.

### Async Patterns

- Use small async fns for I/O.
- Keep policy and state mutation synchronous where possible.
- Use `spawn_blocking` for rusqlite work.

### Protocols

- SSE for streaming token output.
- WebSocket for approvals and bidirectional interaction.
- HTTP for control plane operations.

### Parsers

- `src/llm/openai/sse.rs` has the SSE parser.
- `src/session.rs` handles JSONL replay.
- Prefer libraries for new parsers unless the format is tiny and fully owned.

### Dependencies

- Minimize dependencies. Do not add new dependencies without justification.
- Do not switch rusqlite to sqlx without measured contention.

**Approved stack (already in Cargo.toml):**

| Purpose | Crate | Notes |
|---------|-------|-------|
| HTTP client | `reqwest` (rustls-tls) | No openssl. No `ureq`, `hyper` direct. |
| Async runtime | `tokio` (full) | No `async-std`. |
| Serialization | `serde` + `serde_json` | — |
| Config | `toml` | — |
| CLI | `clap` (derive) | — |
| Errors (app) | `anyhow` | Orchestration, CLI, glue. |
| Errors (boundary) | `thiserror` | Where callers branch on kind. |
| Logging | `tracing` + `tracing-subscriber` | No `log`, no `env_logger`. |
| Database | `rusqlite` (bundled) | No sqlx without measured contention. |
| HTTP server | `axum` | No `actix-web`, no `warp`. |
| Tokenizer | `tiktoken-rs` | — |
| Telemetry | `opentelemetry` + `opentelemetry-otlp` | OTLP/gRPC export. |
| UUIDs | `uuid` (v4) | — |

**Do not introduce:**
- `async-std` — we use tokio exclusively
- `openssl` / `native-tls` — we use rustls
- `log` / `env_logger` — we use tracing
- `actix-web` / `warp` — we use axum
- `sqlx` — not without measured contention proving rusqlite insufficient
- `diesel` — overkill for our schema

## Common Pitfalls

- SSE events can split across chunk boundaries.
- Tool call IDs must survive replay.
- JSONL history and SQLite queue are separate persistence layers.
- Process-group kills are required to terminate descendants.
- Taint comes from user and system messages, not assistant messages.

## Pre-Merge Checklist

1. `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass.
2. No execution path consumes prompts without claiming a queue row.
3. Every claimed queue row ends in `processed` or `failed`.
4. Guard pipeline covers inbound, tool calls, and outbound text.
5. Session reload preserves system, assistant, and tool messages plus tool-call metadata.
6. Trimming never splits assistant/tool round-trips.
7. No secrets in code.
8. Docs are updated if `src/` changed.

## Don't

- Don't add tools without updating the tier rules.
- Don't add `#[allow(unused)]` to suppress real issues.
- Don't skip tests or weaken assertions.
- Don't put secrets in code.
- Don't mix policy, I/O, and state mutation in one function.
- Don't add dependencies without justification.

## Working with Codex and other coding agents

This file is read automatically by Claude Code, Codex, and other agents at session start.

- **Keep sessions focused.** One task per session. Don't combine a refactor + new feature + docs update.
- **Use file-scoped tests during iteration.** Only run the full suite before your final commit.
- **Don't choose new dependencies.** Use only what's in the approved stack above. If you need something not listed, stop and ask.
- **Review the architecture rules and pre-merge checklist before declaring done.**
- **If you're unsure about an architectural decision, stop and ask.** Agents implement; humans architect.
- **Commit messages:** `<type>: <summary>` where type is feat/fix/refactor/test/docs/chore. Explain *why*, not *what*.
