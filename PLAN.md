# PLAN: Unify queue-driven execution into a single shared function

## Problem
`process_queue()` in `src/main.rs` and `drain_session_queue()` + `process_queued_message()` in `src/server.rs` are two separate implementations of the same dequeue→execute→mark lifecycle. This violates "one queue, one worker contract."

## Files to read

| File | Lines | What to look for |
|------|-------|-----------------|
| `src/main.rs` | 170-230 | `process_queue()` — dequeue loop, calls `run_agent_loop()`, marks processed/failed |
| `src/server.rs` | 420-550 | `drain_session_queue()` — same loop but async, `process_queued_message()` — role dispatch, `QueueProcessingOutcome` enum |
| `src/server.rs` | 390-420 | `spawn_http_queue_worker()` — how HTTP triggers the worker |
| `src/server.rs` | 270-390 | `websocket_session()` — how WS enqueues then triggers the worker |
| `src/server.rs` | 600-620 | `NoopTokenSink`, `RejectApprovalHandler` — HTTP-mode handlers |
| `src/server.rs` | 620-660 | `WsApprovalHandler` — WS-mode handler |
| `src/agent.rs` | 130-280 | `run_agent_loop()` signature, `TurnVerdict`, approval handler and token sink traits |
| `src/store.rs` | full | `dequeue_next()`, `mark_processed()`, `mark_failed()`, `create_session()` |
| `src/session.rs` | 1-30 | `Session::new()` constructor |
| `src/turn.rs` | 120-140 | `build_default_turn()` |

## Analysis: differences between the two paths

| Aspect | CLI (`process_queue`) | Server (`drain_session_queue`) |
|--------|----------------------|-------------------------------|
| Dequeue loop | synchronous-ish (blocking on agent loop) | async with worker_lock mutex |
| Role dispatch | only handles "user", drops others silently | handles "user", "system", "assistant", logs unknown |
| Agent loop call | calls `run_agent_loop()` directly | same, via `process_queued_message()` |
| Approval handler | closure `&mut approval_handler` (stdin-based) | `WsApprovalHandler` or `RejectApprovalHandler` |
| Token sink | closure `&mut token_sink` (stdout) | `NoopTokenSink` or WS channel sender |
| Mark failed | yes, on agent loop error | yes, on agent loop error |
| Session | passed in from CLI | created per drain call from sessions_dir |

## Design

Create a new shared function in `src/agent.rs` (or a new `src/worker.rs` — prefer `agent.rs` since it already owns the agent loop):

```
pub async fn drain_queue<F, Fut, P>(
    store: &mut Store,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut dyn TokenSink,
    approval_handler: &mut dyn ApprovalHandler,
) -> Result<()>
```

This function:
1. Loops on `store.dequeue_next(session_id)`
2. Dispatches by role ("user" → `run_agent_loop()`, "system"/"assistant" → `session.append()`, other → log warning)
3. Marks `processed` on success, `failed` on error
4. Returns `Err` on agent loop failure (caller decides whether to continue)

Both CLI and server become thin wrappers:
- **CLI** (`main.rs`): builds session, turn, CliApprovalHandler, stdout token sink → calls `drain_queue()`
- **Server** (`server.rs`): acquires worker_lock, builds session, turn, appropriate handlers → calls `drain_queue()`

`QueueProcessingOutcome` moves into agent.rs (or becomes part of `TurnVerdict`). `NoopTokenSink` and `RejectApprovalHandler` stay in server.rs since they're server-specific handler implementations.

## Per-file changes

### `src/agent.rs`
- Add `pub enum QueueOutcome { Agent(TurnVerdict), Stored, UnsupportedRole(String) }`
- Add `pub async fn drain_queue(...)` — the single shared dequeue loop
- Add `pub async fn process_message(...)` — single message role dispatch (extracted from server's `process_queued_message`)
- Ensure `TokenSink` and `ApprovalHandler` are `pub` traits (they should already be)

### `src/main.rs`
- Delete `process_queue()` entirely
- In the CLI entrypoint, call `agent::drain_queue()` with CLI-specific handlers
- Keep `CliApprovalHandler` and CLI token sink here

### `src/server.rs`
- Delete `drain_session_queue()` and `process_queued_message()` and `QueueProcessingOutcome`
- In `spawn_http_queue_worker()` and `websocket_session()`, call `agent::drain_queue()` with server-specific handlers
- Keep `NoopTokenSink`, `RejectApprovalHandler`, `WsApprovalHandler` here

## Tests

1. **Unit test in agent.rs**: mock provider, enqueue 3 messages (user, system, unknown role), call `drain_queue()`, assert:
   - User message → agent loop ran (provider called)
   - System message → appended to session
   - Unknown role → logged, marked processed, not appended
   - All queue rows end in processed

2. **Existing tests**: `process_queue_marks_failed_when_agent_loop_errors` in main.rs should move to agent.rs test (or just call the new shared function)

3. **Compilation check**: ensure server.rs and main.rs both call the same `drain_queue()` — if either has its own dequeue loop, that's a build error by design (delete the old functions)

## Order of operations

1. Add `QueueOutcome` enum and `process_message()` to `agent.rs` — cargo test (no callers yet, should compile)
2. Add `drain_queue()` to `agent.rs` — cargo test
3. Update `main.rs` to use `agent::drain_queue()`, delete `process_queue()` — cargo test
4. Update `server.rs` to use `agent::drain_queue()`, delete `drain_session_queue()` + `process_queued_message()` + `QueueProcessingOutcome` — cargo test
5. Move/update the failed-marking test — cargo test
6. Clippy clean — cargo clippy
7. Commit: `refactor: unify CLI and server queue execution into shared drain_queue()`
