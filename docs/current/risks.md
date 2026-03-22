# Current Risks and Broken Invariants

> **Single source of truth for known hazards.** Both AGENTS.md and docs/vision.md link here.
> Updated: 2026-03-22

## P0 — Critical

### P0-1: Shell is not meaningfully contained
- `ShellSafety` and `ExfilDetector` are regex heuristics over command strings. Trivially bypassed via `python -c`, `perl -e`, `node -e`, shell builtins, string concatenation (`co""nstitution.md`), or any unflagged binary.
- RLIMIT restricts NPROC/FSIZE/CPU but not filesystem, network, credentials, or syscalls.
- **Do not treat the guard pipeline as a security boundary.** It is risk reduction, not containment.
- **Files:** `src/guard.rs` (ShellSafety, ExfilDetector), `src/tool.rs`
- **Fix:** Flip shell default to approve-unless-whitelisted. Add `[shell]` policy config to `agents.toml`. Real sandboxing (seccomp/landlock) is a later milestone.

### P0-2: HTTP callers can inject arbitrary message roles
- `EnqueueMessageRequest` accepts `role` from the caller. Anyone with the API key can write `system` or `assistant` messages into persistent history.
- These messages replay on every future turn — prompt integrity is broken.
- **Do not assume the first system message is operator-controlled** when messages can arrive via HTTP.
- **Files:** `src/server.rs` (EnqueueMessageRequest, enqueue handler), `src/agent.rs` (drain_queue)
- **Fix:** HTTP/WS always enqueue as `user`. Only CLI/internal paths may set other roles.

### P0-3: Approval denial does not terminate the turn
- When approval is denied (inbound or tool call), the agent loop appends a denial note and `continue`s back into the model. `TurnVerdict::Denied` exists but is never returned.
- HTTP uses `RejectApprovalHandler`, so every approval-required action auto-denies then loops back into the model indefinitely, burning tokens.
- The claim "Deny short-circuits" is true at the `turn.rs` guard layer but false at the `agent.rs` orchestration layer.
- **Do not assume denied approvals stop execution.**
- **Files:** `src/agent.rs` (run_agent_loop), `src/server.rs` (WS handler)
- **Fix:** Return `TurnVerdict::Denied` on denial, add max-denial counter, terminate cleanly.

## P1 — High

### P1-1: Global server serialization
- `worker_lock` is a global mutex held for the full agent turn (LLM call + tool execution + persistence). One slow session blocks all others.
- The store mutex is also held across `drain_queue()`.
- **Files:** `src/server.rs` (worker_lock, state.store)

### P1-2: Queue claiming is not atomic
- `dequeue_next_message()` does SELECT then UPDATE in a transaction, but without an atomic claim predicate. Two processes sharing the same SQLite DB can claim the same row.
- CLI and server both use `sessions/queue.sqlite`. Running both concurrently = duplicate execution risk.
- **Files:** `src/store.rs` (dequeue_next_message)

### P1-3: Provider-controlled call_id is unsanitized
- `call_id` from SSE events flows directly into filesystem paths (`results/{call_id}.txt`) and into shell command suggestions in the cap metadata. No sanitization.
- Path traversal and shell injection via malformed provider responses are possible.
- **Files:** `src/agent.rs` (cap_tool_output)

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

### No caller principal
- Server auth is one static API key. No concept of who is calling. Once you have the key, you have full access to all sessions and can inject any role (see P0-2).
