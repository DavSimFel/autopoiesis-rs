# Current Risks and Broken Invariants

> **Single source of truth for known hazards.** Both AGENTS.md and docs/vision.md link here.
> Updated: 2026-03-23

## P0 — Critical

## P1 — High

### P1-9: Session.append not atomic (memory before disk)
- `Session::append` mutates in-memory history and token counters before writing to disk. If `append_entry_to_file` fails, memory and JSONL are out of sync.
- **Files:** `src/session.rs` (append method)

### P1-10: Budget enforcement is post-turn, not a ceiling
- Budget guard only checks already-completed turn/session/day totals before the next inbound message. The current turn can exceed every configured ceiling. Guard only notices on the following turn.
- **Files:** `src/agent.rs`, `src/gate/budget.rs`, `src/session.rs`

### P1-11: Inbound approval shows system prompt, not user message
- Approval prompt displays the first text block in assembled context (usually system prompt/identity), not the actual user content being approved.
- **Files:** `src/agent.rs` (approval handler call)

### ~~P1-4: SSE parser drops trailing events~~ (2026-03-25)
- Fixed by running trailing-buffer events through the same SSE reducer as newline-terminated frames, so final non-text events are no longer ignored.
- **Files:** `src/llm/openai.rs`

### ~~P1-5: Session replay silently drops unknown entries~~ (2026-03-25)
- Fixed by warning on unknown roles during replay and keeping malformed tool-entry drops explicit instead of silent.
- **Files:** `src/session.rs` (`message_from_entry`, replay tests)

### P1-6: History abstraction is unused and unsafe
- `History` context source is tested but not wired into `build_default_turn()`. If used, its trimming logic splits tool-call/tool-result pairs.
- **Files:** `src/context.rs` (History), `src/turn.rs`

## Architectural risks (not bugs, but structural)

### Guard pipeline is heuristic, not a security boundary
- All shell guards (ShellSafety, ExfilDetector, compound command detection, protected paths) are regex/glob/substring checks. They are trivially bypassable via inline interpreters (`python -c`, `perl -e`), variable expansion, or command substitution. Real containment (seccomp/landlock/uid-drop) is roadmap 3d. Until then, guards are risk reduction, not a sandbox.

### Shell as self-management surface
- Context management goes through the same uncontained shell. With taint tracking built, injection risk is reduced but not eliminated — taint only forces approval, it doesn't block the command.

### Identity hierarchy has no enforcement
- constitution.md is described as immutable. Guard rules now block writes to `identity-templates/`, but this is still heuristic shell-pattern enforcement rather than a real filesystem sandbox. Direct reads remain allowed. See [specs/identity-v2.md](specs/identity-v2.md) for the intended hierarchy.

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

### ~~P1-2: Queue claiming is not atomic~~ (2026-03-23)
- `dequeue_next_message()` now claims rows with a single `UPDATE ... RETURNING` statement and records `claimed_at`.
- Startup recovery only requeues `processing` rows whose claims are older than the configured stale threshold (default 300s); fresh in-flight work is left alone.
- **Files:** `src/store.rs`, `src/main.rs`, `src/server.rs`, `src/config.rs`

### ~~P1-3: Provider-controlled call_id is unsanitized~~ (8e743c3)
- `call_id` sanitized before filesystem paths.

### ~~P1-7: Taint permanently on after first assistant reply~~ (2026-03-23)
- Fixed by adding `Principal::is_taint_source()` — only User and System taint. Agent-authored messages (assistant replies) no longer poison the session or disable standing approvals.

### ~~P1-8: Denied tool calls persisted without matching tool_result~~ (2026-03-23)
- Fixed by delaying assistant `tool_call` persistence until all approval and deny checks complete, and by persisting only sanitized assistant text on denied mixed-content turns.
