# Current Risks and Broken Invariants

> **Single source of truth for known hazards.** Both AGENTS.md and docs/vision.md link here.
> Updated: 2026-03-23

## P0 — Critical

## P1 — High

### P1-7: Taint permanently on after first assistant reply
- Taint is computed from `!message.principal.is_trusted()` across all history. Since assistant replies are stored as `Principal::Agent`, any multi-turn session becomes permanently tainted after the first assistant response. This disables standing approvals for every later turn, making them effectively dead.
- **Files:** `src/turn.rs` (check_inbound taint computation), `src/principal.rs`

### P1-9: Session.append not atomic (memory before disk)
- `Session::append` mutates in-memory history and token counters before writing to disk. If `append_entry_to_file` fails, memory and JSONL are out of sync.
- **Files:** `src/session.rs` (append method)

### P1-10: Budget enforcement is post-turn, not a ceiling
- Budget guard only checks already-completed turn/session/day totals before the next inbound message. The current turn can exceed every configured ceiling. Guard only notices on the following turn.
- **Files:** `src/agent.rs`, `src/gate/budget.rs`, `src/session.rs`

### P1-11: Inbound approval shows system prompt, not user message
- Approval prompt displays the first text block in assembled context (usually system prompt/identity), not the actual user content being approved.
- **Files:** `src/agent.rs` (approval handler call)

### P1-2: Queue claiming is not atomic
- `dequeue_next_message()` does SELECT then UPDATE without an atomic claim predicate. Two processes sharing the same SQLite DB can claim the same row.
- **Files:** `src/store.rs` (dequeue_next_message)

### P1-4: SSE parser drops trailing events
- If the stream ends without a trailing newline, final non-text events are parsed then ignored.
- **Files:** `src/llm/openai.rs` (trailing-buffer handling)

### P1-5: Session replay silently drops unknown entries
- Unknown roles mapped to `None` and discarded. Malformed tool entries dropped with only a warning. Corrupt history silently mutates replayed conversation.
- **Files:** `src/session.rs` (message_from_entry)

### P1-6: History abstraction is unused and unsafe
- `History` context source is tested but not wired into `build_default_turn()`. If used, its trimming logic splits tool-call/tool-result pairs.
- **Files:** `src/context.rs` (History), `src/turn.rs`

## Architectural risks (not bugs, but structural)

### Shell as self-management surface
- Context management goes through the same uncontained shell. With taint tracking built, injection risk is reduced but not eliminated — taint only forces approval, it doesn't block the command.

### Identity hierarchy has no enforcement
- constitution.md and operator.md are described as immutable/operator-only. No guard rule blocks writes to these paths. `echo "new rules" > identity/constitution.md` works.

### No caller principal beyond operator/user key split
- Server auth distinguishes operator vs user, but there is no per-caller identity. One user key = full access to all sessions.

## Fixed

### ~~P0-1: Shell is not meaningfully contained~~ (a54f212)
- Shell default flipped to approve-unless-whitelisted. `ShellSafety::with_policy()` reads `[shell]` config from `agents.toml` with explicit allow/deny patterns and configurable default severity.
- **Note:** guard pipeline is still heuristic, not a security boundary. Real sandboxing (seccomp/landlock) remains later milestone (roadmap 3d).

### ~~P0-4: Shell allowlist bypass via metacharacters~~ (2026-03-23)
- Fixed by checking compound commands before allowlist/standing approvals and requiring explicit approval for metacharacter chains such as `;`, `&&`, `||`, `|`, backticks, `$(`, and line breaks.
- **Residual:** quoted metacharacters still fail closed to approval; that is intentional and may require manual confirmation in some cases.

### ~~P0-5: Auth.json exposed via allowlisted commands~~ (2026-03-23)
- Fixed by hard-denying direct reads of protected credential paths, deduplicating the shared path catalog, and removing the shipped broad auto-allow entries that made reads trivial.
- **Residual:** copied credential blobs remain a separate hardening area outside this patch set.

### ~~P0-2: HTTP callers can inject arbitrary message roles~~ (b03843b)
- `Principal` enum enforces role based on auth key.

### ~~P0-3: Approval denial does not terminate the turn~~ (33ef098)
- `MAX_DENIALS_PER_TURN` (2) + `break 'agent_turn`.

### ~~P1-1: Global server serialization~~ (2026-03-22)
- Per-session locking via `HashMap<String, Arc<Mutex<()>>>`. Store mutex released before agent execution. Concurrent sessions no longer block each other.

### ~~P1-3: Provider-controlled call_id is unsanitized~~ (8e743c3)
- `call_id` sanitized before filesystem paths.

### ~~P1-8: Denied tool calls persisted without matching tool_result~~ (2026-03-23)
- Fixed by delaying assistant `tool_call` persistence until all approval and deny checks complete, and by persisting only sanitized assistant text on denied mixed-content turns.
