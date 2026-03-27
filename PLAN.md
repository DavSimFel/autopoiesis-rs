# PLAN

## 1. Files read

- `TASK.md`
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`
- Every Rust source file under `src/` (`src/**/*.rs`, 52 files at planning time), including the modules touched by this MVP: `src/app/{args,mod,session_run}.rs`, `src/config/{agents,mod,spawn_runtime,tests}.rs`, `src/context/{mod,tests}.rs`, `src/lib.rs`, `src/main.rs`, `src/server/{mod,state,queue_worker,http,ws,queue}.rs`, `src/session_runtime/{factory,mod}.rs`, `src/store.rs`, `src/test_support.rs`, `src/turn/{mod,builders,tests}.rs`

No additional file rereads are needed for this revision; this update only fixes the gaps identified in `REVIEW.md`.

## 2. Exact changes per file

- `src/session_registry.rs` (new)
  - Add a small registry type that materializes the session topology from loaded config.
  - Define `SessionSpec` with the canonical session id, display label, effective tier, whether the session is always-on, and the derived per-session `Config` clone that workers and request-owned execution paths will run with.
  - Define `SessionRegistry` with lookup by session id, iteration over all declared sessions, and iteration over only always-on sessions.
  - Keep this module one-way: it depends on config types, but nothing in config depends on the registry, so there is no cycle.

- `src/lib.rs`
  - Export `session_registry` so both server code and binary-side app code can use the same registry API.

- `src/config/agents.rs`
  - Add a helper such as `AgentTierConfig::is_configured()` so registry construction can distinguish "tier exists in schema" from "tier is actually wired with prompt/model/tool policy".
  - Add or extend validation so a session declared as always-on cannot resolve to an unconfigured tier.

- `src/config/spawn_runtime.rs`
  - Add the pure helper that derives a session-local runtime `Config` from the base config plus selected tier/session identity without mutating the shared base config.
  - Use that helper when constructing `SessionSpec.config` so server workers, registry-backed request-owned execution, and CLI registry-backed session runs share one derivation path.

- `src/store.rs`
  - Add the queue/store API needed for per-session worker ownership, such as an atomic `claim_next_message_for_session(session_id)` helper that only claims pending work for that session.
  - Add an idempotent `ensure_session_row(session_id)`-style helper that creates the persisted session row when missing and is safe to call at startup or from CLI bootstrap paths.
  - Keep the existing generic claim path for request-owned execution because both registry-backed non-always-on sessions and non-registry/ad hoc sessions remain on that path.
  - Add store-level tests proving session-scoped claims do not steal rows for other sessions, ensured session rows are idempotent, and queue rows still land in terminal `processed`/`failed` states.

- `src/context/session_manifest.rs` (new)
  - Add a focused builder for the `## Available Sessions` system block described by the MVP.
  - Render from `SessionRegistry` so the same manifest text is used by background workers, registry-backed request-owned turns, and CLI turns.
  - Keep formatting deterministic for tests.

- `src/context/mod.rs`
  - Export the new session-manifest builder/context object.

- `src/turn/builders.rs`
  - Extend the low-level turn-construction inputs so a caller can optionally provide the session manifest block.
  - Inject that block into the system message path for registry-backed sessions without changing tool guard precedence.
  - Do not let builders reach back into global config directly for registry data; accept the manifest as an input to avoid hidden coupling.

- `src/turn/mod.rs`
  - Thread the optional session manifest through the existing shared `build_turn_for_config()` facade; do not add a second facade.
  - Keep existing non-registry call sites compiling by making the manifest input optional at the facade layer and defaulting it to `None`.
  - This preserves the architecture rule that turn assembly goes through the shared facade, not direct `builders.rs` calls or competing entrypoints.

- `src/session_runtime/factory.rs`
  - Add helper(s) that take a `SessionSpec` and return the exact turn-builder/provider configuration that session should run with.
  - Ensure those helpers always call the existing shared `build_turn_for_config()` facade with the optional manifest populated and keep subscription loading on the same code path used today.
  - Keep the existing subscription-loading helpers intact; this is additive plumbing.

- `src/session_runtime/mod.rs`
  - Re-export the new registry/session-aware factory helper(s) from `factory.rs`.
  - This keeps binary-side code in `src/app/session_run.rs` on the supported `autopoiesis::session_runtime` surface instead of forcing it to reach into private modules.

- `src/server/state.rs`
  - Add `SessionRegistry` to `ServerState`.
  - Add storage for always-on worker handles/tasks so startup creates one durable drain loop per declared always-on session and shutdown can cancel/join them cleanly.
  - Keep the existing base config on state for non-registry/ad hoc request handling, but treat the registry as the source of truth for any registry-backed session behavior.

- `src/server/mod.rs`
  - Build the registry once at server startup from the loaded base config.
  - Before starting workers, idempotently ensure persisted session rows exist for every declared registry-backed session so queue-owned sessions are usable on a fresh store and manifests map to real session ids.
  - Create `ServerState` with that registry.
  - Start one persistent queue-drain task per always-on session from the registry.
  - Land this together with the HTTP/WS routing changes below so each session id has one exact execution mode.

- `src/server/queue_worker.rs`
  - Change from ad hoc "drain whatever is queued with `state.config`" behavior to a per-session durable loop that owns exactly one always-on session id.
  - Inside the loop, always resolve provider, turn builder, subscriptions, and queue claims from that session's `SessionSpec.config` and session id, not from the global base config.
  - Use the new store/session-scoped claim API so workers cannot steal work across sessions.
  - Preserve existing processed/failed queue-row terminal states.

- `src/server/ws.rs`
  - Make session routing exact and three-way.
  - If `session_id` exists in `SessionRegistry` and `always_on == true`, WS is queue-only: enqueue the request, attach the socket to the session's output stream/subscription path, and never call inline drain for that id.
  - If `session_id` exists in `SessionRegistry` and `always_on == false`, keep request-owned inline execution, but run it with that session's `SessionSpec.config` and the registry-backed manifest-aware turn path.
  - If `session_id` is not present in `SessionRegistry`, keep the existing ad hoc request-owned inline drain behavior with the base config.

- `src/server/http.rs`
  - Apply the same exact three-way routing rule as WS.
  - If `session_id` exists in `SessionRegistry` and `always_on == true`, HTTP is enqueue-only and the dedicated background worker is the sole drainer for that id.
  - If `session_id` exists in `SessionRegistry` and `always_on == false`, keep request-owned execution, but run it with that session's `SessionSpec.config` and the registry-backed manifest-aware turn path.
  - If `session_id` is not present in `SessionRegistry`, keep the existing ad hoc request-owned behavior with the base config.

- `src/server/queue.rs`
  - Update/add tests around queue ownership and session routing.
  - Cover the case where an always-on session message is enqueued through WS/HTTP and is not directly drained by request-handling code.
  - Cover the case where a registry-backed but non-always-on session id on WS/HTTP still uses the request-owned path, but with registry-derived config.
  - Cover the case where a non-registry session id on WS/HTTP still takes the legacy ad hoc inline/request-owned path.

- `src/test_support.rs`
  - Extend server test scaffolding to build a `ServerState` with a registry, any required always-on worker bookkeeping, and helpers for asserting ensured session rows after startup or CLI bootstrap.

- `src/app/args.rs`
  - Add the `enqueue` CLI subcommand and its arguments.

- `src/app/mod.rs`
  - Wire the new `enqueue` command into app dispatch.

- `src/app/enqueue_command.rs` (new)
  - Resolve the target session id through `SessionRegistry` before queue insert.
  - If the session id is registry-backed, call the shared idempotent `ensure_session_row(session_id)` helper before inserting the queue row so declared sessions are usable even on a fresh store before server startup.
  - If the session id is not registry-backed, keep the existing fail-fast behavior for missing session rows; do not auto-create arbitrary ad hoc sessions.
  - Emit a precise user-facing error for unknown non-registry session rows.

- `src/main.rs`
  - Dispatch the new `enqueue` subcommand through the app module.

- `src/app/session_run.rs`
  - Resolve the requested session id through `SessionRegistry` before falling back to legacy ad hoc session behavior.
  - If the resolved registry-backed session has `always_on == true`, do not run it directly; return a clear error that this session is queue-owned and must be targeted through `autopoiesis enqueue`.
  - If the resolved registry-backed session has `always_on == false`, use the new `session_runtime` re-exported helper so CLI runs get the same manifest-aware turn builder, subscription wiring, and session-local config as the server request-owned path.
  - If the session id is not in `SessionRegistry`, keep the existing ad hoc direct-run behavior.

- `src/turn/tests.rs`
  - Add focused facade/builder tests proving that when a manifest is supplied it appears in the system context for registry-backed sessions, and when not supplied legacy behavior stays unchanged.
  - Add an explicit regression test that manifest-aware turn construction still preserves subscription context through the existing `build_turn_for_config_with_subscriptions()` path.

- `src/context/tests.rs`
  - Add deterministic rendering tests for the session manifest block.

- `src/config/tests.rs`
  - Add validation tests for "always-on session resolves only to configured tier" and any new helper edge cases.

- `docs/architecture/overview.md`
  - Update the architecture narrative/diagram so it reflects the registry, startup/CLI session-row bootstrap for declared sessions, one-worker-per-always-on-session ownership model, the exact three-way HTTP/WS/CLI routing rule, and the CLI enqueue path.

## 3. What tests to write

- Registry/config tests
  - Building the registry from config yields the expected session ids, tiers, and always-on flags.
  - Declaring an always-on session against an unconfigured tier is rejected.
  - The derived per-session config keeps the intended active tier/session identity and does not mutate the shared base config.

- Store/bootstrap tests
  - `claim_next_message_for_session(session_id)` only claims rows for the requested session.
  - Session-scoped claims do not steal pending rows for other sessions.
  - `ensure_session_row(session_id)` is idempotent and leaves exactly one persisted session row after repeated calls.
  - Queue rows for always-on sessions still terminate in `processed` or `failed`.

- Startup/bootstrap tests
  - Server startup ensures persisted session rows exist for every declared registry-backed session before the first enqueue.
  - Repeated startup/bootstrap passes do not duplicate session rows.
  - An always-on session can be enqueued immediately after startup on a fresh store without any prior manual seeding.

- Context/turn tests
  - The session manifest renderer is deterministic and stable.
  - Registry-backed turns include the `## Available Sessions` block exactly once.
  - Legacy non-registry turns remain unchanged when no manifest is supplied.
  - Manifest-aware turn construction still preserves subscription context through `build_turn_for_config_with_subscriptions()`.
  - CLI session-run uses the same manifest-aware, subscription-preserving builder path for registry-backed non-always-on sessions as the server request-owned path.

- HTTP/WS routing tests
  - An always-on session enqueued through WS is queued and left for the dedicated worker; WS does not inline-drain it.
  - An always-on session enqueued through HTTP is queued and left for the dedicated worker; HTTP does not inline-drain it.
  - A registry-backed but non-always-on session id on WS uses request-owned execution with registry-derived config.
  - A registry-backed but non-always-on session id on HTTP uses request-owned execution with registry-derived config.
  - A non-registry session id on WS is treated as ad hoc and still follows the legacy inline/request-owned path.
  - A non-registry session id on HTTP is treated as ad hoc and still follows the legacy inline/request-owned path.

- CLI routing tests
  - `session_run` for a registry-backed `always_on` session fails with the explicit "use enqueue" error and does not execute the turn directly.
  - `session_run` for a registry-backed non-always-on session uses the registry-derived config and manifest-aware turn path.
  - `session_run` for a non-registry session keeps the existing ad hoc direct-run behavior.
  - `enqueue` for a declared registry-backed session succeeds on a fresh store before any server startup by using the shared idempotent session bootstrap helper.
  - `enqueue` for a missing non-registry session id still fails and does not create a session row.

- Startup/shutdown tests
  - Server startup creates one worker per always-on session and not more than one per session.
  - Shutdown/cancellation stops those workers cleanly.

## 4. Order of operations

1. Add the registry/config primitives and the store support they need (`src/session_registry.rs`, `src/config/agents.rs`, `src/config/spawn_runtime.rs`, `src/store.rs`, `src/lib.rs`) plus unit tests. This is additive and should keep existing behavior green.
2. Add the manifest context and turn/runtime plumbing (`src/context/session_manifest.rs`, `src/context/mod.rs`, `src/turn/{builders,mod}.rs`, `src/session_runtime/{factory,mod}.rs`) plus focused tests. Keep existing call sites compiling by threading the manifest as an optional argument through the existing `build_turn_for_config()` facade with a default of `None`.
3. Land all server-side bootstrap and routing changes in one step (`src/server/{state,mod,queue_worker,ws,http,queue}.rs`). This step is intentionally atomic: session-row bootstrap, worker startup, session-scoped queue claims, and both request-path routing branches must land together so every session id immediately follows one of the three explicit server execution modes.
4. Update CLI and test scaffolding (`src/test_support.rs`, `src/app/{args,mod,enqueue_command,session_run}.rs`, `src/main.rs`) after the registry/runtime facade exists, then add enqueue/session-run tests. Keep the always-on CLI rule and the registry-backed enqueue bootstrap in the same step so the binary never directly runs a queue-owned session and can still enqueue declared sessions on a fresh store.
5. Update docs (`docs/architecture/overview.md`) and run the full required checks: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release` with zero warnings.

## 5. Risk assessment

- Highest risk is routing drift between always-on workers, registry-backed request-owned sessions, and non-registry ad hoc sessions. This plan removes that ambiguity with one exact three-way rule shared by HTTP, WS, and CLI: always-on sessions are queue-owned, registry-backed non-always-on sessions are request-owned with registry config, and non-registry sessions stay ad hoc.
- Fresh-store bootstrap is the main end-to-end risk for queue-owned or declared sessions. Mitigation: add an idempotent `ensure_session_row` helper in `src/store.rs`, call it for all declared registry-backed sessions during server startup, and reuse the same helper in CLI enqueue for declared registry-backed session ids.
- The main compile risk is facade plumbing. Mitigation: thread manifest support through the existing `src/turn/mod.rs` facade and re-export the new runtime helpers from `src/session_runtime/mod.rs` instead of bypassing established module boundaries.
- The main behavioral risk is silently dropping subscription context while switching to manifest-aware session helpers. Mitigation: keep subscription loading on the existing path and add an explicit regression test for it.
- The main persistence risk is enqueueing into missing sessions. Mitigation: keep arbitrary non-registry enqueue as "existing session only", bootstrap declared session rows through the shared helper, test both success and failure paths, and avoid hidden auto-creation for unknown ids.
- No new dependency cycle is required: config stays foundational, registry depends on config, store remains a persistence boundary, and server/app/context/turn depend on those lower layers.
