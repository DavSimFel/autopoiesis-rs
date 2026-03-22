# Current Risks and Broken Invariants

> **Single source of truth for known hazards.** Both AGENTS.md and docs/vision.md link here.
> Updated: 2026-03-22

## P1 — High

### P1-1: Global server serialization
- `worker_lock` is a global mutex held for the full agent turn (LLM call + tool execution + persistence). One slow session blocks all others.
- The store mutex is also held across `drain_queue()`.
- **Files:** `src/server.rs` (worker_lock, state.store)

### P1-2: Queue claiming is not atomic
- `dequeue_next_message()` does SELECT then UPDATE in a transaction, but without an atomic claim predicate. Two processes sharing the same SQLite DB can claim the same row.
- CLI and server both use `sessions/queue.sqlite`. Running both concurrently = duplicate execution risk.
- **Files:** `src/store.rs` (dequeue_next_message)

### P1-4: SSE parser drops trailing events
- If the stream ends without a trailing newline, final non-text events (function_call_arguments.done, response.completed, [DONE]) are parsed then ignored.
- Tool calls and completion metadata can silently disappear.
- **Files:** `src/llm/openai.rs` (streaming loop trailing-buffer handling)

### P1-5: Session replay silently drops unknown entries
- Unknown roles are mapped to `None` and discarded. Malformed tool entries are dropped with a warning.
- Corrupt or partially written history can mutate the replayed conversation without failing fast.
- **Files:** `src/session.rs` (message_from_entry)

### P1-6: History abstraction is unused and unsafe
- `History` context source is heavily tested but not wired into `build_default_turn()`. Real history replay happens ad-hoc in `agent.rs`.
- If `History` were used, it can split tool-call/tool-result pairs during trimming.
- **Files:** `src/context.rs` (History), `src/turn.rs` (build_default_turn)

## Architectural risks (not bugs, but structural)

### Shell as self-management surface
- The agent manages subscriptions/topics/identity via CLI through shell. That means context management goes through the same uncontained shell. Without taint tracking, prompt injection can instruct the agent to subscribe malicious files.

### Identity hierarchy has no enforcement
- constitution.md and operator.md are described as immutable/operator-only. No guard rule blocks writes to these paths. `echo "new rules" > identity/constitution.md` works.

### No caller principal beyond operator/user key split
- Server auth distinguishes operator vs user via separate API keys, but there is no per-caller identity. Once you have the user key, you have full access to all sessions.

## Fixed

### ~~P0-1: Shell is not meaningfully contained~~ (a54f212)
- Shell default flipped to approve-unless-whitelisted. `ShellSafety::with_policy()` reads `[shell]` config from `agents.toml` with explicit allow/deny patterns and configurable default severity.
- **Note:** the guard pipeline is still heuristic, not a security boundary. Real sandboxing (seccomp/landlock) remains a later milestone (roadmap 3d).

### ~~P0-2: HTTP callers can inject arbitrary message roles~~ (b03843b)
- `Principal` enum enforces role based on auth key. User-key callers always enqueue as `user`. Only operator-key callers may request alternate roles (defaulting to `user`).

### ~~P0-3: Approval denial does not terminate the turn~~ (33ef098)
- `make_denial_verdict()` increments a denial counter; after `MAX_DENIALS_PER_TURN` (2), the loop returns `TurnVerdict::Denied`. All denial paths use `break 'agent_turn` to exit cleanly.

### ~~P1-3: Provider-controlled call_id is unsanitized~~ (8e743c3)
- `call_id` is sanitized before being used in filesystem paths and shell command suggestions in `cap_tool_output()`.
- **Files:** `src/agent.rs`
