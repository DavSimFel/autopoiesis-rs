# AGENTS.md

Instructions for AI agents working on this codebase.

## Project overview

Autopoiesis is a Rust agent runtime. ~7.6K lines across 17 source files, 124 tests. One binary that runs as CLI (REPL or one-shot) or HTTP+WS server. The agent has one tool: shell.

## Build and test

```bash
cargo build --release          # must succeed with zero warnings
cargo test                     # 124 tests, all must pass (1 ignored)
cargo test --features integration  # live API tests (skip if no auth)
cargo fmt --check              # must pass
cargo clippy -- -D warnings    # must pass
```

**Every change must pass `cargo test` before committing.** No exceptions.

## Project structure

```
src/
├─ main.rs        CLI + server entrypoint. Arg parsing (clap), REPL loop, server launch.
├─ agent.rs       Agent loop. Orchestrates turns: context → LLM → tool calls → persist.
│                 TokenSink and ApprovalHandler traits for I/O decoupling.
│                 Shell output cap + file-backed results (4KB threshold).
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

## Known broken invariants (P0 — read before trusting any claim above)

The rules above describe intended behavior. The following are **known violations** in the current codebase. Do not assume these are fixed unless the linked issue/PR is merged.

### P0-1: Shell is not meaningfully contained
- `ShellSafety` and `ExfilDetector` are regex heuristics over command strings. They are trivially bypassed via `python -c`, `perl -e`, `node -e`, shell builtins, string concatenation (`co""nstitution.md`), or any unflagged binary.
- RLIMIT restricts NPROC/FSIZE/CPU but not filesystem, network, credentials, or syscalls.
- **Do not treat the guard pipeline as a security boundary.** It is risk reduction, not containment.
- Fix: flip shell default to approve-unless-whitelisted. Real sandboxing (seccomp/landlock) is a later milestone.

### P0-2: HTTP callers can inject arbitrary message roles
- `EnqueueMessageRequest` accepts `role` from the caller. Anyone with the API key can write `system` or `assistant` messages into persistent history.
- These messages replay on every future turn — prompt integrity is broken.
- **Do not assume the first system message is operator-controlled** when messages can arrive via HTTP.
- Fix: HTTP/WS always enqueue as `user`. Only CLI/internal paths may set other roles.

### P0-3: Approval denial does not terminate the turn
- When approval is denied (inbound or tool call), the agent loop appends a denial note and `continue`s back into the model. `TurnVerdict::Denied` exists but is never returned.
- HTTP uses `RejectApprovalHandler`, so every approval-required action auto-denies then loops back into the model indefinitely, burning tokens.
- The claim "Deny short-circuits" is true at the `turn.rs` guard layer but false at the `agent.rs` orchestration layer.
- **Do not assume denied approvals stop execution.**
- Fix: return `TurnVerdict::Denied` on denial, add max-denial counter, terminate cleanly.

### P1-2: Queue claiming is not atomic
- `dequeue_next_message()` does SELECT then UPDATE in a transaction, but without an atomic claim predicate. Two processes sharing the same SQLite DB can claim the same row.
- CLI and server both use `sessions/queue.sqlite`. Running both concurrently = duplicate execution risk.
- **Do not assume queue rows are exclusively claimed** in multi-process scenarios.

### P1-3: Provider-controlled call_id is unsanitized
- `call_id` from SSE events flows directly into filesystem paths (`results/{call_id}.txt`) and into shell command suggestions in the cap metadata. No sanitization.
- Path traversal and shell injection via malformed provider responses are possible.
- **Do not trust `call_id` as safe for filesystem or shell use.**

## Coding conventions

- **Error handling:** `anyhow::Result` everywhere. Use `.context("description")` on fallible operations. No `.unwrap()` in non-test code. `.expect("reason")` is allowed only for compile-time invariants (e.g., regex constants). `.unwrap_or(default)` / `.unwrap_or_default()` are fine when the default is semantically correct. Never silently swallow errors that indicate corruption or I/O failure — log them.
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

## Pre-merge checklist

1. **Queue paths:** No execution path may consume prompt content without first claiming a queue row. Every claimed row must end in `processed` or `failed`.
2. **Guard paths:** Inbound text, tool calls, tool batches, streamed model output, and tool output all go through guards. No server path may substitute auto-approve for interactive approvals.
3. **Prompt handling:** First `system` message is preserved as instructions. Later `system` messages are appended as replayable conversation state.
4. **Persistence:** Reload covers all session day files, not only today. Reload preserves `system`, `assistant`, `tool`, and tool-call metadata needed for replay.
5. **Trimming:** Role-aware. Never splits assistant/tool round-trips.
6. **Shell execution:** Timeout cleanup terminates the whole process group. Docs describe RLIMIT caps honestly — not called a sandbox.
7. **Secrets:** Token files use `0600`. Tests cover both inbound and outbound redaction.
8. **Error visibility:** No fallible operation silently swallowed with `unwrap_or` when error indicates corruption, I/O failure, or lost data. Recovery paths must log before falling through.
9. **Verification:** `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check` all pass.

## Don't

- Don't add tools. One tool.
- Don't add `# type: ignore` or `#[allow(unused)]` to suppress real issues.
- Don't skip tests. Don't weaken existing test assertions.
- Don't put secrets in code. Auth tokens go through the OAuth flow.
- Don't commit directly to main — use feature branches + PRs.
