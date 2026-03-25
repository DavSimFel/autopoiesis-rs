# Session Spawn Plan

## 1. Files Read

- `docs/risks.md`
- `docs/architecture/overview.md`
- `Cargo.toml`
- `agents.toml`
- `src/agent.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/gate/budget.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/secret_patterns.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/main.rs`
- `src/principal.rs`
- `src/server.rs`
- `src/session.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### `src/store.rs`

- Extend the `sessions` schema to add `parent_session_id TEXT NULL REFERENCES sessions(id)`.
- Add an index for child lookups, e.g. `idx_sessions_parent_session_id_created_at` on `(parent_session_id, created_at, id)`.
- Add a migration helper parallel to `ensure_messages_claimed_at_column` that runs after `CREATE TABLE IF NOT EXISTS` and adds `parent_session_id` on existing databases.
- Keep `create_session()` as the idempotent root-session helper.
- Add `create_child_session(parent_id, child_id, metadata)` that does a plain `INSERT` with `parent_session_id = parent_id` so parent-missing and child-id-collision errors surface instead of being silently ignored.
- Add `get_parent_session(child_id) -> Result<Option<String>>`.
- Add `list_child_sessions(parent_id) -> Result<Vec<String>>`, ordered by `created_at ASC, id ASC`.
- Add store tests for the new column, self-FK behavior, lookup methods, and migration from the old schema.

### `src/principal.rs`

- Update `Principal::from_source()` so any source with the `agent-` prefix maps to `Principal::Agent`.
- Do the prefix check before the existing `*-operator` / `*-user` suffix checks.
- Add tests that cover `agent-child-123`, plus a regression check that existing `cli`, `http-operator`, and `ws-user` mappings stay unchanged.

### `src/spawn.rs` (new)

- Add `SpawnRequest` with:
  - `parent_session_id: String`
  - `task: String`
  - `model_override: Option<String>`
  - `reasoning_override: Option<String>`
- Add `SpawnResult` with `child_session_id: String`.
- Add a small local child-session ID generator. Reuse the existing timestamp/nanos style already used in `server.rs`; do not widen `server::generate_session_id()` just for this.
- Add `spawn_child(store, config, request) -> Result<SpawnResult>`.
- Keep the canonical implementation here; `agent.rs` should only expose a thin forwarding API so there is one real implementation site.
- `spawn_child()` behavior:
  - Build a small metadata JSON blob for the child session.
  - Metadata should include at least the parent linkage and the requested overrides.
  - Use `config.active_agent` / `config.model` only as metadata context for future use; there is no provider/runtime behavior change in this patch.
  - Create the child with `Store::create_child_session(...)`.
  - Enqueue the task as a `user` message.
  - Use an `agent-*` source string so `Principal::from_source()` yields `Agent`. I would use `agent-{parent_session_id}` for provenance.
  - Return the generated child ID.
- Also put the child-completion helper here so the spawn-specific logic stays in one module:
  - Private helper to extract the latest assistant text from a `Session` by scanning `session.history()` from the end.
  - `pub(crate)` helper that, given `(store, child_session_id, session)`, looks up the parent, formats a completion message, and enqueues it to the parent queue with source `agent-{child_session_id}`.
- Completion message role should be `assistant`, not `user`, so draining the parent stores the child result without triggering a fresh LLM turn. That matches the “no orchestration yet” constraint.
- Completion message body should include the child ID and the last assistant response text, for example:
  - `Child session <id> completed.`
  - blank line
  - `<last assistant response>`
- If no agent-authored assistant text exists, use an explicit fallback sentence instead of enqueueing an empty message.

### `src/agent.rs`

- Add the requested public entrypoint on the `agent` surface as a thin wrapper or re-export, so callers can use `agent::spawn_child(...)` without inventing a new `Agent` struct that does not exist in this codebase.
- Re-export or forward the request/result types from `spawn.rs` so the public API is coherent.
- Do not touch `run_agent_loop()` or either public signature.
- In `drain_queue()`, track whether this call actually processed any messages.
- After the loop exits normally with the queue empty, call the new `spawn::enqueue_child_completion(...)` helper if at least one message was processed in this drain pass.
- Do not emit a completion message on early error returns or denial returns; only do it on successful drain-to-empty.
- Keep all existing queue status transitions exactly as they are now.
- Add agent tests that use the real store + session history to verify parent completion propagation on the CLI/shared `drain_queue()` path.
- Add an explicit no-op drain test so an empty queue does not enqueue a duplicate completion notification.

### `src/server.rs`

- `server.rs` has its own `drain_session_queue()` loop and does not delegate to `agent::drain_queue()`, so it needs the same completion hook.
- Mirror the same “processed-any + successful drain-to-empty” logic here.
- After the loop exits, reacquire the store lock and call the shared completion helper from `spawn.rs`.
- Keep the current session locking and mark-processed / mark-failed behavior intact.
- Add a server-side test that proves the WebSocket/HTTP drain path also enqueues the parent completion message for child sessions.
- Add a server-side no-op drain test so an empty queue does not enqueue a duplicate completion notification on the duplicated server path.

### `src/lib.rs`

- Export the new module with `pub mod spawn;`.
- Land this declaration in the same change that adds `src/spawn.rs` so the new module is compiled immediately.

### `docs/architecture/overview.md`

- Update the source-file count and module map to include `spawn.rs`.
- Update the session/store description to mention parent-child session links and child completion propagation to the parent queue.

## 3. Tests To Write

### Store invariants

- Creating a child session records `parent_session_id` and `get_parent_session()` returns it.
- `get_parent_session()` returns `None` for a root session.
- `list_child_sessions()` returns only that parent’s children, in `created_at/id` order.
- Creating a child for a missing parent fails because the FK is enforced.
- Opening an old database without `parent_session_id` migrates successfully and the new APIs still work.

### Principal invariants

- `Principal::from_source("agent-child-1") == Principal::Agent`.
- Existing mappings for `cli`, `http-operator`, `ws-user`, and unknown sources remain stable.
- `Principal::Agent` still is not a taint source.

### Spawn invariants

- `agent::spawn_child()` returns a new child session ID from the requested public entrypoint.
- The child row exists in `sessions` with the requested parent.
- The queued task row is `role = 'user'`.
- The queued task row source starts with `agent-`.
- The queued task row content is the exact requested task string.
- Metadata captures the requested model/reasoning overrides without changing global config behavior.
- Spawning with a nonexistent parent returns an error.

### Completion propagation invariants

- Draining a child session to empty enqueues exactly one completion message into the parent queue for that drain pass.
- The completion row uses source `agent-{child_session_id}`.
- The completion row uses `role = 'assistant'`.
- The completion content includes the child session ID.
- The completion content includes the child’s last agent-authored assistant text, not a system audit note.
- An empty/no-op drain does not enqueue any parent completion message.
- No completion message is sent for root sessions with no parent.
- No completion message is sent when the drain ends via error or denial instead of successful completion.
- Add explicit no-op drain coverage for both `agent::drain_queue()` and `server::drain_session_queue()` so the duplicate loops cannot diverge silently.
- Add a direct helper-level test for the “latest assistant text” extraction so it skips tool-call-only assistant placeholders and trailing system audit notes.

## 4. Order Of Operations

1. Add the store schema change, migration helper, lookup methods, and store tests first.
2. Add the `agent-*` principal mapping and principal tests.
3. Add `src/spawn.rs` and export it from `lib.rs` in the same change so the new module compiles immediately.
4. Fill in `spawn.rs` with request/result types, ID generation, metadata construction, `spawn_child()`, the shared completion helper, and direct unit tests against `Store` / `Session`.
5. Add the thin `agent::spawn_child(...)` forwarding API and the related request/result re-exports on the `agent` surface.
6. Wire `agent::drain_queue()` to call the shared completion helper only after a successful drain-to-empty, then add/adjust agent tests including the explicit no-op drain case.
7. Wire `server::drain_session_queue()` to the same helper and add a server test for that path.
8. Update `docs/architecture/overview.md`.
9. Run `cargo fmt --check`, `cargo test`, `cargo clippy -- -D warnings`, and `cargo build --release`.

## 5. Risk Assessment

- There is no `Agent` struct in this codebase today. The least disruptive way to satisfy the requested API is to keep the real implementation in `src/spawn.rs` and expose a thin `agent::spawn_child(...)` forwarder/re-export, not invent a new stateful type.
- The biggest semantic choice is the parent completion message role. `assistant` is the safest fit because it preserves provenance without auto-triggering a parent LLM turn; `user` would implicitly create orchestration behavior that the task explicitly says not to add yet.
- Completion deduplication is intentionally scoped to “once per successful drain pass that processed at least one message.” That avoids no-op duplicate notifications on empty drains without adding more state or widening the schema again.
- `model_override` and `reasoning_override` cannot affect runtime behavior in this patch because provider/session execution config is still global. Storing them in session metadata is the correct minimal move.
- The “last assistant response” helper must skip `Principal::System` audit notes and tool-only assistant placeholders. The test plan now needs one direct helper-level test for this, plus end-to-end drain tests, because the existing session history can contain both.
- The store migration is low risk because SQLite accepts `ALTER TABLE ... ADD COLUMN parent_session_id TEXT REFERENCES sessions(id)` for a nullable column. Existing rows will remain valid with `NULL`.
