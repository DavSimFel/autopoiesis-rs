# AGENTS.md

How to work in this repo. Read this first.

> **⚠️ Read [docs/risks.md](docs/risks.md) before trusting any invariant claim.** Shell guards remain heuristic, there is no PTY or real sandboxing yet, and subscriptions are not wired into turn context.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # all must pass
cargo fmt --check              # must pass
cargo clippy -- -D warnings    # must pass
cargo test --features integration  # live API tests (skip if no auth)
```

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
| `src/agent/shell_execute.rs` | Shared guarded shell execution path |
| `src/turn.rs` | Tier-aware turn assembly and guard composition |
| `src/store.rs` | SQLite sessions, queue, subscriptions, and plan tables |
| `src/plan/*.rs` | Plan execution, patching, notifications, recovery |
| `src/config.rs` | `agents.toml` loading and policy/config validation |
| `src/context.rs` | Identity, skill, and history context assembly |
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

- `src/llm/openai.rs` has the SSE parser.
- `src/session.rs` handles JSONL replay.
- Prefer libraries for new parsers unless the format is tiny and fully owned.

### Dependencies

- Minimize dependencies.
- `tracing` and `tracing-subscriber` are already in the stack.
- `thiserror` is already in the stack.
- Do not switch rusqlite to sqlx without measured contention.

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
