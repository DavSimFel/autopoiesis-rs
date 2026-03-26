# Session 8 Plan

## 1. Files Read

- `CODE_STANDARD.md`
- `AGENTS.md`
- `docs/risks.md`
- `agents.toml`
- `Cargo.toml`
- every Rust source file under `src/` from the prior planning pass, including the current entrypoint, terminal I/O, logging/utilities, server orchestration, queue processing, and session runtime paths

## 2. Exact Changes Per File

- `src/main.rs`
  Reduce this file to wiring only.
  Keep only private module declarations needed by the binary such as `mod app;`, `mod terminal_ui;`, and `mod logging;`.
  Move clap type definitions and parse helpers out.
  Move tracing bootstrap out.
  Move plan command dispatch out.
  Move subscription command dispatch out.
  Move one-shot vs REPL session execution out.
  Keep `main()` responsible only for config/bootstrap, command selection, and delegating to extracted helpers.

- `src/lib.rs`
  Do not add `pub mod app;`.
  Do not make `logging` part of the library crate.
  Keep `app` and `logging` binary-private so this cleanup does not widen the library API surface.

- `src/app/mod.rs`
  Add the private `app` module facade used only by `src/main.rs`.
  Re-export only the internal helper entrypoints that `main.rs` needs.

- `src/app/args.rs`
  Move clap structs, enums, subcommands, and parse helpers from `src/main.rs` here.
  Keep this file responsible only for argument shape and parse-time validation.
  Preserve exact command names, flags, defaults, and help text.

- `src/app/tracing.rs`
  Move tracing initialization and subscriber/layer assembly from `src/main.rs` here.
  This module should depend only on binary-private `src/logging.rs`, not on server modules.
  Keep side effects limited to tracing bootstrap.

- `src/app/plan_commands.rs`
  Move plan-related command execution from `src/main.rs` here.
  Keep it focused on command orchestration, not clap parsing.
  Preserve the existing exit paths and user-visible output.

- `src/app/subscription_commands.rs`
  Move subscription CRUD/listing command execution from `src/main.rs` here.
  Preserve current formatting and exit semantics.
  Keep command routing separate from generic terminal session execution.

- `src/app/session_run.rs`
  Move REPL and one-shot runtime entrypoints from `src/main.rs` here.
  Centralize `resolve_session_id`, stale queue-row recovery, fresh-turn drain setup, and the branch between one-shot and interactive loop.
  Use the same `session_runtime::{factory, drain}` primitives as the server path so terminal execution does not rebuild subscription/context setup independently.
  Keep only terminal-specific concerns here: session selection, queue recovery, prompt submission, and REPL loop control.

- `src/cli.rs`
  Delete this file after all imports are switched.

- `src/terminal_ui.rs`
  Add this file as the rename target of `src/cli.rs`.
  Preserve terminal I/O helpers exactly, with only path/module name changes unless a small import fix is required.
  Update all callers to use `terminal_ui` so the name reflects terminal interaction rather than clap parsing.

- `src/logging.rs`
  Add this file and move the tracing formatter, user-output writer/target helpers, and any tracing-specific formatting glue out of `src/util.rs`.
  Keep it binary-private and used only by `src/app/tracing.rs`.
  Do not let `src/server/*` depend on this module.

- `src/util.rs`
  Remove the tracing formatter and user-output target helpers that belong in `src/logging.rs`.
  Leave only non-logging general utilities here.
  Update imports to avoid any lingering dependency from binary tracing bootstrap back into unrelated utility code.

- `src/server/mod.rs`
  Convert this module into a facade with focused submodules.
  Move shared server state types into `state.rs`.
  Move session-lock coordination into `session_lock.rs`.
  Move queue-drain worker/orchestration helpers into `queue_worker.rs`.
  Keep only module declarations, re-exports, and top-level docs here.

- `src/server/state.rs`
  Extract server-owned shared state from `src/server/mod.rs`.
  Keep this file limited to state structs, constructors, and state access helpers.
  Do not place queue-drain behavior or websocket protocol logic here.

- `src/server/session_lock.rs`
  Extract per-session locking and lock-map helpers from `src/server/mod.rs`.
  Preserve lock-key semantics and concurrency behavior.
  Keep the implementation independent from HTTP or websocket transport code.

- `src/server/queue_worker.rs`
  Extract the queue-drain worker and shared server-side session runtime setup here.
  Make the shared abstraction explicit for all three callers, not just HTTP and WS.
  Define one hook bundle or adapter surface that covers:
  HTTP request/response calls: buffered or final-output sink, no interactive approvals.
  WebSocket calls: incremental token sink plus interactive approval handler.
  Background queue calls: no live client token sink, no interactive approvals, and explicit completion/failure hooks for queue-row finalization and any existing notification path.
  This module should own the common setup around `session_runtime::{factory, drain}` and nothing transport-specific beyond invoking the supplied hooks.
  Keep this module depending on `session_runtime`, not the reverse.

- `src/server/http.rs`
  Replace duplicated session-runtime setup with calls into `server::queue_worker`, which in turn uses `session_runtime::{factory, drain}`.
  Use explicit HTTP hooks so request/response behavior stays unchanged.

- `src/server/ws.rs`
  Replace duplicated session-runtime setup with calls into `server::queue_worker`, which in turn uses `session_runtime::{factory, drain}`.
  Use explicit websocket hooks so token streaming and approval prompts continue to work.
  Do not reduce WS to the HTTP behavior model.

- `src/server/queue.rs`
  Remove duplicated subscription/session setup and route queue processing through the same `session_runtime::{factory, drain}`-backed orchestration used by the other server paths.
  Use the background-queue hook variant from `server::queue_worker`: no live streaming sink, no interactive approvals, and explicit success/failure callbacks that preserve current queue row state transitions and notification behavior.
  Keep queue ownership/concurrency behavior unchanged.

- existing `src/session_runtime` module files
  Keep `factory` and `drain` as the shared runtime primitives used by both `src/app/session_run.rs` and `src/server/queue_worker.rs`.
  Only make narrow signature changes if required to support the shared hook/caller shape.
  Do not move transport logic into `session_runtime`.

- `AGENTS.md`, `docs/architecture/overview.md`, and any repo docs that name `src/cli.rs` or describe `src/main.rs` as owning clap/tracing/dispatch logic
  Update path references and responsibility descriptions to match thin `main.rs`, `src/terminal_ui.rs`, the new `src/app/*` modules, and the split `src/server/*` modules.

## 3. What Tests To Write

- `src/app/args.rs` parser tests
  Assert command names, flags, defaults, and subcommand routing match the pre-refactor behavior.

- `src/app/plan_commands.rs` and `src/app/subscription_commands.rs` dispatch tests
  Assert extracted handlers preserve exit behavior and user-facing output for representative plan and subscription commands.

- `src/app/session_run.rs` unit tests
  Assert `resolve_session_id` preserves explicit IDs and creates a new session only when required.
  Assert stale claimed queue rows are recovered before running the terminal path.
  Assert one-shot mode takes the single-turn drain path and does not enter the REPL loop.
  Assert interactive mode enters the REPL path and does not accidentally run one-shot exit logic.
  Assert terminal execution uses the shared `session_runtime::{factory, drain}` path instead of rebuilding setup locally.

- `src/logging.rs` unit tests
  Assert the extracted formatter still produces the same user-output vs tracing-target split.
  Assert user-facing output is routed to the dedicated target/writer and does not leak into the normal tracing stream.

- `src/server/session_lock.rs` unit tests
  Assert the same session key serializes concurrent work.
  Assert different session keys do not share a lock.
  Assert extracted lock helpers preserve the previous reuse semantics.

- `src/server/queue_worker.rs` unit tests
  Assert the shared worker always builds the runtime through `session_runtime::factory` and drains through `session_runtime::drain`.
  Assert common subscription/session setup runs once in the shared path, not once per caller.
  Assert HTTP, WS, and background-queue hook variants each receive the correct sink/approval/completion behavior.

- HTTP/WS integration coverage
  Add or extend server tests so HTTP still gets the non-interactive behavior and websocket still gets token streaming plus approval handling after both are moved onto the shared helper.
  Assert the websocket path emits incremental tokens and surfaces approval requests, not just the final response.
  Assert the HTTP path does not unexpectedly require websocket-only hooks.

- queue lifecycle regression coverage
  Add tests around `src/server/queue.rs` after rerouting through the shared worker.
  Assert a queue row is claimed before drain begins.
  Assert every claimed row still ends in `processed` or `failed`.
  Assert the background queue hook path does not attempt websocket approvals or live token streaming.
  Assert existing completion/failure notification behavior still fires on the queue path.

- terminal/server wiring regression coverage
  Add a regression test that proves the terminal path and the server path both go through the same `session_runtime::{factory, drain}` primitives for subscription/context setup.

## 4. Order Of Operations

1. Extract `src/app/args.rs`, `src/app/plan_commands.rs`, `src/app/subscription_commands.rs`, and `src/app/session_run.rs` from `src/main.rs`.
   Keep tracing/bootstrap and terminal module names unchanged in this step so the binary stays green while entrypoint logic is being thinned.
   Make `src/app/session_run.rs` call the shared `session_runtime::{factory, drain}` primitives immediately so terminal setup does not fork from server setup.

2. Add `src/logging.rs` and `src/app/tracing.rs` together, then switch `src/main.rs` to call the extracted tracing bootstrap.
   Remove the moved logging helpers from `src/util.rs` only after the new binary-private path compiles.

3. Rename `src/cli.rs` to `src/terminal_ui.rs` in one compile-safe step.
   Move the file, update every module import in the same change, then delete the old path.

4. Split `src/server/mod.rs` into `state.rs` and `session_lock.rs`, preserving re-exports from `server/mod.rs`.
   This keeps server references stable before changing queue orchestration.

5. Introduce `src/server/queue_worker.rs` with explicit caller variants for HTTP, WS, and background queue work, all backed by `session_runtime::{factory, drain}`.
   Keep old HTTP/WS/queue call sites working by routing them through thin adapters first.

6. Migrate `src/server/http.rs`, `src/server/ws.rs`, and `src/server/queue.rs` to the shared queue-worker path.
   Remove duplicate setup code only after all three callers are on the shared path and the transport-specific plus queue-lifecycle tests are green.

7. Update docs that reference the old file names or old `main.rs` responsibilities.
   Then run `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `xtask/lint.sh`.
   Run `openclaw system event --text 'Session 8 done: entrypoints split' --mode now` only after the full suite is green.

## 5. Risk Assessment

- Binary/library boundary risk
  `app` and `logging` must stay binary-private. If either is made part of the library crate just to make imports convenient, this refactor will unintentionally widen the public API.

- Shared runtime drift risk
  The terminal path and all three server callers need to use `session_runtime::{factory, drain}` directly. If any caller keeps a bespoke setup branch, this cleanup will only move duplication around.

- Dependency-cycle risk
  `server::queue_worker` may depend on `session_runtime`, but `session_runtime` must not depend back on `server`. Caller-specific hooks stay outside `session_runtime`.

- Websocket regression risk
  WS has behavior HTTP and background queue do not: incremental token streaming and interactive approvals. The shared helper must be explicit about those hooks so the refactor does not silently erase websocket semantics.

- Queue lifecycle risk
  Moving `src/server/queue.rs` onto the shared worker can break claim/process/fail transitions if row ownership is not preserved. The queue caller needs its own explicit hook variant and regression tests.

- Rename risk
  `src/cli.rs` to `src/terminal_ui.rs` is mechanically simple but broad; missed imports or module declarations will fail the build immediately.

- Logging behavior risk
  Moving formatter/target code out of `util` can subtly change layer ordering or output routing. Preserve current targets and assert them directly in tests.

- Session coordination risk
  Extracting state and session locks can change lock identity or lifetime if ownership is altered. Preserve the exact session-keyed locking model and validate it with focused concurrency tests.

- Documentation drift risk
  The repo rules require docs to track `src/` changes. File/path renames and responsibility moves need doc updates in the same change set.
