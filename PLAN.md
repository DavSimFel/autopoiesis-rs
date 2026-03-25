# Phase 5c Plan: Split `src/server.rs` into `src/server/`

## 1. Files Read

### Required docs and config

- `AGENTS.md`
- `docs/risks.md`
- `docs/roadmap.md`
- `docs/architecture/overview.md` (server/queue/auth sections)
- `Cargo.toml`
- `agents.toml`

### All Rust source files under `src/`

- `src/agent/loop_impl.rs`
- `src/agent/loop_impl/tests.rs`
- `src/agent/mod.rs`
- `src/agent/queue.rs`
- `src/agent/queue/tests.rs`
- `src/agent/spawn.rs`
- `src/agent/spawn/tests.rs`
- `src/agent/tests.rs`
- `src/agent/tests/common.rs`
- `src/agent/tests/regression_tests.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/delegation.rs`
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
- `src/model_selection.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/server.rs` (full file read)
- `src/session.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### Delete

- `src/server.rs`
  - Remove the monolithic file after its contents are redistributed.
  - Preserve every public symbol currently reachable as `crate::server::*` by re-exporting from `src/server/mod.rs`.

### Add

- `src/server/mod.rs`
  - Become the module root replacing `src/server.rs`.
  - Declare `mod auth;`, `mod http;`, `mod queue;`, and `mod ws;`.
  - Keep `pub async fn run(port: u16) -> Result<()>` with the same startup flow:
    - load `agents.toml`
    - read `AUTOPOIESIS_API_KEY`
    - open `sessions/queue.sqlite`
    - recover stale queue rows
    - build `ServerState`
    - bind socket
    - `axum::serve(...)`
  - Keep `pub fn router(state: ServerState) -> Router` with the same route table and the same auth middleware attachment order.
  - Define `pub struct ServerState` with the same fields and `Clone`.
  - Keep shared private session-id helpers here so both HTTP and WS ingress paths use the same implementation without cross-module coupling:
    - `generate_session_id()`
    - `validate_session_id()`
  - Preserve the existing public API exactly:
    - keep `run(...)`, `router(...)`, and `ServerState` defined in `mod.rs`
    - re-export `pub use http::HttpError;`
    - do not publicly re-export route handlers or websocket handlers that are currently private
  - Require route handlers and websocket entrypoints used by `router(...)` to be `pub(super)` so `mod.rs` can call them without widening the API surface.
  - Add a small `#[cfg(test)]` shared test-support module only if needed to avoid copying the current server test fixtures into multiple files. Keep it inside `mod.rs`, not as a new file, to stay within the requested target structure.

- `src/server/http.rs`
  - Move REST DTOs here:
    - `HealthResponse`
    - `CreateSessionRequest`
    - `CreateSessionResponse`
    - `SessionListResponse`
    - `EnqueueMessageRequest`
    - `EnqueueMessageResponse`
    - `ErrorBody`
  - Move `pub enum HttpError`, its constructors, and `IntoResponse` impl here.
  - Move REST handlers here with bodies unchanged:
    - `health_check`
    - `create_session`
    - `list_sessions`
    - `enqueue_message`
  - Keep those handlers `pub(super)` for router wiring; do not re-export them publicly.
  - Keep `enqueue_message` behavior exactly the same:
    - reject invalid session ids
    - create session if missing
    - derive role from `Principal`
    - derive source via `principal.source_for_transport("http")`
    - enqueue queue row
    - drop the store lock before `spawn_http_queue_worker(...)`
  - Import `spawn_http_queue_worker` from `server::queue`.
  - Use shared `validate_session_id()` / `generate_session_id()` from `mod.rs` or keep them `pub(super)` in `http.rs` and import into `ws.rs`; do not duplicate logic.
  - Move HTTP-focused tests here:
    - health endpoint
    - create session endpoint
    - list sessions endpoint
    - invalid session id is rejected on `POST /api/sessions/:id/messages`
    - user/operator/default role enqueue behavior
    - `HttpError` response mapping tests

- `src/server/ws.rs`
  - Move WebSocket-only protocol types here:
    - `WsFrame`
    - `WsApprovalRequest`
    - `WsApprovalDecision`
  - Move WebSocket handlers here:
    - `ws_session`
    - `websocket_session`
  - Keep `ws_session(...)` `pub(super)` for router wiring; do not re-export it publicly.
  - Move WebSocket helpers here:
    - `route_ws_client_message`
    - `WsTokenSink`
    - `send_ws_terminal_denial`
    - `WsApprovalHandler`
    - `severity_label`
  - Keep `websocket_session` behavior exactly the same:
    - same reader/writer task split
    - same session auto-create-on-connect best effort
    - same prompt and approval channels
    - same provider factory construction
    - same denial handling and `Done` frame behavior
  - Preserve the existing `use crate::auth as root_auth;` alias or an equally explicit alias here so calls to `get_valid_token()` continue to bind the crate-level auth module rather than the new sibling `server::auth` module.
  - Import `drain_session_queue` from `server::queue`.
  - Move WS-focused tests here:
    - invalid session id is rejected on `GET /api/ws/:session_id`
    - `ws_approval_handler_waits_for_client_response`
    - `ws_terminal_denial_emits_error_then_done`
  - Add targeted unit coverage for `route_ws_client_message(...)` only if extraction exposes an untested gap. Keep assertions narrow and behavior-preserving.

- `src/server/auth.rs`
  - Move auth middleware constants and functions here:
    - `API_KEY_HEADER`
    - `authenticate`
    - `principal_for_token`
  - Keep WS query-string auth behavior exactly the same:
    - header auth first
    - query-string fallback only for `/api/ws/`
    - only `api_key=...`
  - Expose `authenticate` as `pub(super)` so `server::router(...)` can keep the same middleware layer.
  - Move auth-focused tests here:
    - `invalid_api_key_returns_unauthorized`
    - header auth precedence over query-string auth
    - WS-only query-string `api_key` fallback
  - Keep auth middleware tests owned here rather than split between `http.rs` and `auth.rs`.

- `src/server/queue.rs`
  - Move queue-drain and worker logic here:
    - `impl ServerState::session_lock(&self, ...)`
    - `SessionLockLease`
    - `drain_session_queue`
    - `spawn_http_queue_worker`
  - Move queue-worker-only sinks/approval helpers here:
    - `NoopTokenSink`
    - `RejectApprovalHandler`
  - Preserve the existing `use crate::auth as root_auth;` alias or an equally explicit alias here so the HTTP queue worker continues to build providers against the crate-level auth module rather than the new sibling `server::auth` module.
  - Keep per-session lock behavior identical:
    - same `ServerState::session_lock(...)`
    - same weak-reference eviction in `Drop`
    - same single-session serialization and cross-session concurrency
  - Keep queue semantics identical:
    - dequeue loop
    - processed/failed status transitions
    - denial early-return behavior
    - child completion enqueue on successful drain
    - store mutex dropped during provider execution
  - Move queue-focused tests here:
    - `drain_queue_marks_target_message_processed`
    - `drain_queue_uses_supplied_approval_handler`
    - child completion enqueue/no-op tests
    - different sessions do not block each other
    - same session processing is serialized
    - store mutex is not held across agent turn
    - session lock entry is evicted after drain

### Docs to update in the implementation pass

- `AGENTS.md`
  - Update the module-structure guidance from `server.rs -> ... server/sse.rs` to the actual split being implemented:
    - `server/mod.rs`
    - `server/http.rs`
    - `server/ws.rs`
    - `server/auth.rs`
    - `server/queue.rs`
  - Keep the “split by responsibility” rule intact.

- `docs/architecture/overview.md`
  - Update the source layout description from a single `server.rs` to `server/`.
  - Update any internal path references that assume the queue drain code lives in one file.

- `docs/roadmap.md`
  - Update the Phase 5 server-split entry so it matches the implemented module layout instead of the older `http + ws + sse` shorthand.

### Files that should not need changes

- `src/lib.rs`
  - Keep `pub mod server;` unchanged. Rust will resolve it to `src/server/mod.rs`.

- `src/main.rs`
  - Keep `server::run(port).await?;` unchanged.

- `Cargo.toml`
  - No dependency changes.

- `agents.toml`
  - No config-shape changes.

## 3. Tests To Write

### Existing tests to relocate with their code

- `http.rs`
  - `health_endpoint_returns_ok`
  - `create_session_returns_session_id_and_persists_metadata`
  - `list_sessions_returns_created_sessions_in_store_order`
  - `enqueue_rejects_invalid_session_id`
  - `enqueue_with_user_api_key_forces_role_to_user`
  - `enqueue_with_operator_key_keeps_requested_role`
  - `enqueue_without_role_defaults_to_user`
  - `http_error_bad_request_maps_to_400_with_json_body`
  - `http_error_unauthorized_maps_to_401_with_json_body`
  - `http_error_internal_maps_to_500_with_json_body`

- `auth.rs`
  - `invalid_api_key_returns_unauthorized`
  - `header_auth_wins_over_query_string_auth`
  - `query_string_api_key_is_accepted_only_for_ws_paths`

- `queue.rs`
  - `drain_queue_marks_target_message_processed`
  - `drain_queue_uses_supplied_approval_handler`
  - `drain_queue_enqueues_child_completion_message_for_parent_session`
  - `drain_queue_does_not_enqueue_completion_for_empty_child_queue`
  - `drain_queue_enqueues_completion_when_persisted_history_exists_but_new_assistant_response_is_empty`
  - `different_sessions_do_not_block_each_other`
  - `same_session_processing_is_serialized`
  - `store_mutex_is_not_held_across_agent_turn`
  - `session_lock_entry_is_evicted_after_drain`

- `ws.rs`
  - `ws_session_rejects_invalid_session_id`
  - `ws_approval_handler_waits_for_client_response`
  - `ws_terminal_denial_emits_error_then_done`

### Small new tests justified by the split

- `ws.rs`
  - Verify `route_ws_client_message(...)` accepts prompt payloads from both supported shapes:
    - `{"data":{"content":"..."}}`
    - `{"content":"..."}`
  - Verify approval frames require both `request_id` and `approved`.

- `mod.rs` or `http.rs`
  - A router smoke test is already effectively covered by the health/enqueue tests; do not add a second redundant route-table test unless the split introduces a wiring regression.

### Invariants each test set must continue asserting

- Public HTTP behavior is byte-for-byte equivalent for status codes and JSON bodies.
- `create_session` still creates a session row and returns a usable session id.
- `list_sessions` still returns the session registry in store order.
- Invalid session ids are still rejected on both HTTP message enqueue and WS session upgrade paths.
- `Principal`-based role forcing remains unchanged.
- Auth middleware still rejects missing/invalid keys and still supports WS query fallback.
- Queue rows still end in `processed` or `failed`.
- `drain_session_queue(...)` still serializes same-session work and does not serialize different sessions.
- Store mutex is still released before provider execution.
- WebSocket approval flow still blocks until the matching client decision arrives.
- Terminal denial still emits `Error` then `Done`.

## 4. Order Of Operations

1. Rename the module root first: move `src/server.rs` to `src/server/mod.rs` without changing behavior.
   - This is the only unavoidable atomic step because Rust cannot use both `src/server.rs` and `src/server/mod.rs` for the same module at once.
   - Run `cargo fmt`, `cargo test`, and a targeted `cargo clippy -- -D warnings` check immediately after the rename if the file still compiles unchanged.

2. Extract `auth.rs`.
   - Move `API_KEY_HEADER`, `authenticate`, and `principal_for_token`.
   - Keep `router(...)` in `mod.rs` and call `auth::authenticate`.
   - Run targeted server/auth tests.

3. Extract `http.rs`.
   - Move HTTP DTOs, `HttpError`, and REST handlers.
   - Keep `run(...)` and `router(...)` in `mod.rs`.
   - Run targeted HTTP tests.

4. Extract `queue.rs`.
   - Move `SessionLockLease`, `drain_session_queue(...)`, `spawn_http_queue_worker(...)`, `NoopTokenSink`, and `RejectApprovalHandler`.
   - Run queue/concurrency tests immediately after extraction because this is the highest-risk move.

5. Extract `ws.rs`.
   - Move WS handler types, `ws_session(...)`, `websocket_session(...)`, `WsApprovalHandler`, and frame-routing helpers.
   - Run WS and auth-path tests.

6. Consolidate shared test support.
   - If the moved tests need shared helpers such as `test_state()`, provider doubles, or queue inspection helpers, place them in a `#[cfg(test)]` section of `mod.rs` with `pub(super)` visibility.
   - Avoid introducing extra production helpers just to make tests compile.

7. Update docs required by repo rules.
   - `AGENTS.md`
   - `docs/architecture/overview.md`

8. Run the full verification pass.
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy -- -D warnings`
   - `cargo build --release`
   - If auth is available: `cargo test --features integration`

## 5. Risk Assessment

### Highest risk

- Queue behavior drift during extraction.
  - `drain_session_queue(...)` contains the session lock, store lock lifetime, status transitions, denial return path, and child-completion enqueue. A visibility or helper-placement mistake here can change behavior without obvious compile errors.

- Route wiring drift.
  - The server currently applies auth middleware as a route layer over the whole router. The split must preserve the exact route list and middleware placement. Moving handlers into submodules is safe; rebuilding the router differently is not.

- Public API drift.
  - `crate::server::run`, `crate::server::router`, `crate::server::ServerState`, and `crate::server::HttpError` must remain reachable at the same paths.

### Medium risk

- WS approval flow regression.
  - `WsApprovalHandler::request_approval(...)` relies on request-id ordering and `block_in_place(...)` when a Tokio runtime exists. That exact behavior must move intact.

- Shared helper duplication.
  - `validate_session_id(...)` is used by both HTTP and WS paths. Duplicating it instead of sharing it introduces unnecessary drift risk.

- Test-helper sprawl.
  - The current server tests share a fair amount of setup. If helpers are copied across files instead of centralized under `#[cfg(test)]`, future behavior checks will diverge.

### Low risk

- Import churn and visibility fixes.
  - This is expected during the split. The plan should prefer `pub(super)` over `pub(crate)` for internal submodule access to avoid accidentally widening API surface.

- Docs drift.
  - Repo rules require docs sync for `src/` changes. This is easy to miss because the code split itself is mechanical.

### Explicit non-goals for this split

- No changes to auth rules, queue semantics, WS protocol, or approval behavior.
- No new dependencies.
- No changes to `main.rs` or `lib.rs` import paths.
- No opportunistic cleanup of P2 findings unless they are required to make the split compile.
