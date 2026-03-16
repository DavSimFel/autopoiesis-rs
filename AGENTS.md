# AGENTS.md

Instructions for AI agents working on this codebase.

## Project overview

Autopoiesis is a Rust agent runtime. ~5.5K lines across 16 source files. One binary that runs as CLI (REPL or one-shot) or HTTP+WS server. The agent has one tool: shell.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # 92 tests, all must pass
cargo test --features integration  # live API tests (skip if no auth)
```

**Every change must pass `cargo test` before committing.** No exceptions.

## Project structure

```
src/
├─ main.rs        CLI + server entrypoint. Arg parsing (clap), REPL loop, server launch.
├─ agent.rs       Agent loop. Orchestrates turns: context → LLM → tool calls → persist.
│                 TokenSink and ApprovalHandler traits for I/O decoupling.
├─ turn.rs        Turn-level orchestration. Composes ContextSource + Tool + Guard.
│                 build_default_turn() is the shared constructor for CLI and server.
├─ context.rs     ContextSource trait. Two impls: Identity (prompt files), History (replay).
├─ tool.rs        Tool trait + Shell impl. Async exec, RLIMIT sandbox, process-group kill.
├─ guard.rs       Guard trait + pipeline. SecretRedactor, ShellSafety, ExfilDetector.
│                 Verdict: Allow | Modify | Approve { severity } | Deny. Deny short-circuits.
├─ session.rs     JSONL persistence. SessionEntry serialization, token tracking, trimming.
├─ store.rs       SQLite backend. Session registry + message queue (enqueue/dequeue/mark).
├─ server.rs      axum HTTP + WebSocket. API key auth middleware. Queue-driven agent exec.
├─ llm/mod.rs     LlmProvider trait, ChatMessage, ChatRole, MessageContent, ToolCall, TurnMeta.
├─ llm/openai.rs  OpenAI Responses API. SSE streaming with incremental parsing.
├─ auth.rs        OAuth device flow. Token storage, refresh, expiry check.
├─ config.rs      agents.toml loading. Model + reasoning_effort.
├─ identity.rs    Loads identity/*.md files, concatenates in order.
├─ template.rs    Resolves {{var}} placeholders in identity text.
├─ util.rs        utc_timestamp(), misc helpers.
└─ lib.rs         Public module re-exports.
```

## Key abstractions

- **`ContextSource`** (context.rs) — assembles messages into the context window. Identity sets the system prompt; History replays past turns within token budget.
- **`Tool`** (tool.rs) — `name()`, `definition()`, `execute()`. Only one impl: Shell. Adding tools is almost certainly wrong — make the prompt smarter instead.
- **`Guard`** (guard.rs) — `check(&mut GuardEvent) -> Verdict`. Pipeline runs in order. Deny short-circuits. Approve escalates by severity.
- **`LlmProvider`** (llm/mod.rs) — `stream_completion()`. Only one impl: OpenAI. Returns `StreamItem` variants (Token, ToolCall, Done).
- **`TokenSink`** / **`ApprovalHandler`** (agent.rs) — decouple the agent loop from I/O. CLI uses stdout/stdin. Server uses WS channels.
- **`Turn`** (turn.rs) — composes context sources, tools, and guards via builder pattern. `build_default_turn()` is the canonical constructor.

## Architecture rules

1. **One tool.** Shell is the universal tool. If you think you need a second tool, you're wrong. Teach the prompt instead.
2. **Guard pipeline order matters.** Deny beats Approve beats Allow. `resolve_verdict()` in turn.rs handles precedence.
3. **No `unsafe` outside RLIMIT.** The only `unsafe` block is the `pre_exec` closure in tool.rs for `setrlimit` and `setpgid`. Keep it that way.
4. **Traits for composition, not inheritance.** ContextSource, Tool, Guard, LlmProvider, TokenSink, ApprovalHandler — all trait objects composed in Turn or agent loop.
5. **Two execution paths share one Turn.** CLI and server both use `build_default_turn()`. Don't let them diverge.

## Coding conventions

- **Error handling:** `anyhow::Result` everywhere. Use `.context("description")` on fallible operations. No `.unwrap()` in non-test code.
- **Async:** tokio runtime. Shell execution is async with `tokio::time::timeout`. Process-group kill on timeout via `libc::killpg`.
- **Serialization:** serde + serde_json. Session entries are one-JSON-per-line (JSONL).
- **Tests:** unit tests in `#[cfg(test)] mod tests` at bottom of each file. Use temp dirs with unique names (timestamp-based) to avoid collisions.
- **Naming:** Rust conventions. Types are PascalCase, functions are snake_case. Test functions describe behavior: `trim_drops_oldest_non_system_messages`.

## Common pitfalls

- **SSE parsing:** The OpenAI streaming parser in openai.rs handles incremental byte boundaries. Don't assume events arrive as complete lines — they can split mid-JSON.
- **Tool call IDs:** Assistant messages with tool calls must include `ToolCall` content blocks in session persistence. The API rejects history replay without matching call IDs.
- **Ghost tool entries:** If the SSE parser encounters a tool event with no identifiable `call_id`, skip it. Never fall back to `"unknown"` — it creates phantom entries.
- **Session JSONL ≠ SQLite queue.** JSONL files persist conversation history. SQLite queue handles message ordering and delivery. They're complementary, not redundant.
- **Process-group kill:** Shell child processes run in their own process group (`setpgid(0,0)`). Timeout kill uses `killpg` to terminate all descendants, not just the parent shell.

## What's next

See VISION.md for the roadmap. Current priorities:
1. Shell output cap + file storage (force subscription pattern)
2. Subscription system (SQLite + CLI)
3. Context assembly rework (subscriptions in timeline)
4. Topics (.md files with code blocks)

## Don't

- Don't add tools. One tool.
- Don't add `# type: ignore` or `#[allow(unused)]` to suppress real issues.
- Don't skip tests. Don't weaken existing test assertions.
- Don't put secrets in code. Auth tokens go through the OAuth flow.
- Don't commit directly to main — use feature branches + PRs.
