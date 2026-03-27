# PLAN

## 1. Files Read

- `AGENTS.md`
- `TASK.md`
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`
- Every file under `src/` at the time of planning, including the observability touch points and their direct callers:
  - `src/lib.rs`
  - `src/main.rs`
  - `src/app/session_run.rs`
  - `src/agent/mod.rs`
  - `src/agent/loop_impl.rs`
  - `src/agent/shell_execute.rs`
  - `src/agent/child_drain.rs`
  - `src/plan.rs`
  - `src/plan/patch.rs`
  - `src/plan/runner.rs`
  - `src/plan/recovery.rs`
  - `src/plan/notify.rs`
  - `src/store.rs`
  - `src/session_runtime/drain.rs`
  - `src/server/mod.rs`
  - `src/server/state.rs`
  - `src/server/queue_worker.rs`
  - `src/server/auth.rs`
  - `src/server/http.rs`
  - `src/server/queue.rs`
  - `src/turn/mod.rs`
  - `src/turn/verdicts.rs`
  - `src/llm/mod.rs`
  - `src/llm/openai/mod.rs`
  - The remaining `src/**` files were also read to satisfy the repo instruction to read all source files before planning.

## 2. Exact Changes Per File

### `Cargo.toml`

- Add:
  - `uuid = { version = "1", features = ["v4"] }`
  - `opentelemetry = "0.27"`
  - `opentelemetry-otlp = { version = "0.27", features = ["grpc-tonic"] }`
  - `opentelemetry_sdk = { version = "0.27", features = ["rt-tokio"] }`
- Do not add any other dependency unless compilation forces a transitive-support helper already implied by those crates.

### `src/lib.rs`

- Export a new `observe` module.
- Keep existing public surface stable aside from the new module export.

### `src/observe/mod.rs`

- Add:
  - `TraceEvent` enum with every variant from `TASK.md`, matching field names and field types exactly.
  - `Observer` trait with a borrowed fan-out-safe API: `fn emit(&self, event: &TraceEvent)`.
  - `NoopObserver`.
  - `MultiObserver`.
  - `build_observer(sessions_dir: &Path) -> Arc<dyn Observer>`.
- `TraceEvent` derives: `Debug`, `Serialize`, `Deserialize`.
- All event payload strings use `String`; nullable textual and numeric fields use `Option<String>` / `Option<i64>`.
- Make the thread-safety contract explicit:
  - `Observer: Send + Sync + 'static`
  - all stored observer trait objects are `Arc<dyn Observer>`
- `emit()` never returns `Result` and must internally swallow/log failures.
- Resolve the “default observer” ambiguity explicitly:
  - call-site default for low-level APIs and tests: `Arc::new(NoopObserver)` when the caller does not opt in
  - runtime startup path for CLI/server: `build_observer()` constructs `MultiObserver([SqliteObserver, optional OtelObserver])`
  - the code and tests must treat those as two different defaults on purpose, not as a contradiction
- Make the eval-event status explicit:
  - include `EvalRunStarted` and `EvalRunFinished` in the schema exactly as required
  - no additional emitter wiring is planned in this task because the current `src/` tree has no active eval-run execution path to instrument
  - add a short code comment near the enum or observer docs noting that these variants are intentionally dormant until an eval runtime exists
- Re-export `sqlite` and `otel` submodules as needed.

### `src/observe/sqlite.rs`

- Add `SqliteObserver` backed by `Mutex<rusqlite::Connection>`.
- Open a separate SQLite database at `<sessions_dir>/traces.sqlite`.
- On init, create:
  - table `trace_events(id INTEGER PRIMARY KEY, event_type TEXT NOT NULL, session_id TEXT, turn_id TEXT, plan_run_id TEXT, eval_run_id TEXT, timestamp TEXT NOT NULL, payload_json TEXT NOT NULL)`
  - index on `(eval_run_id, timestamp)`
  - index on `(session_id, timestamp)`
  - index on `(plan_run_id, timestamp)`
- Insert one row per event synchronously inside `emit(&TraceEvent)`.
- Populate the top-level id columns by extracting them from each enum variant.
- Serialize the full event as JSON into `payload_json`.
- On failure, log and drop; do not panic.

### `src/observe/otel.rs`

- Add `OtelObserver`.
- Initialize OTLP exporter using `ZO_OTEL_ENDPOINT`, defaulting to `http://localhost:5081`.
- Emit one span per event with stable span/event names derived from the enum variant name.
- Attach common attributes (`session_id`, `turn_id`, `plan_run_id`, `eval_run_id`, event type).
- If setup or export fails, log at low severity and silently drop future emits rather than surfacing errors.
- Implement against the borrowed `emit(&TraceEvent)` API so `MultiObserver` can forward the same event to every child without cloning requirements.
- Structure initialization / export code so tests can exercise the silent-drop path with an injected failing setup or failing export helper rather than depending on a real collector failure.

### `src/app/session_run.rs`

- Build a runtime observer once via `build_observer(config.sessions_dir())` or equivalent already-available sessions path.
- Thread `Arc<dyn Observer>` through the CLI execution path into the shared queue-drain / turn-run entrypoints.
- Keep non-runtime helper signatures unchanged unless this file is the narrowest stable place to inject observer state.

### `src/server/mod.rs`

- Build one runtime observer during server startup using the same `build_observer()` helper.
- Store and clone it into server state / worker startup paths.

### `src/server/state.rs`

- Add `observer: Arc<dyn Observer>` to `ServerState`.
- Update constructor/helper methods in this file to require or fill the observer explicitly.
- This change must land together with all test constructor fallout in the same incremental step so the tree stays compiling.

### `src/server/queue_worker.rs`

- Pass `state.observer.clone()` (or equivalent) into queue-drain / turn-run calls.

### `src/server/auth.rs`

- Update `ServerState` test fixtures / constructors to provide `Arc::new(NoopObserver)` so tests compile immediately after the struct change.

### `src/server/http.rs`

- Update `ServerState` test fixtures / constructors to provide `Arc::new(NoopObserver)`.

### `src/server/queue.rs`

- Update `ServerState` test fixtures / constructors to provide `Arc::new(NoopObserver)`.

### `src/agent/mod.rs`

- Thread observer-aware entrypoints through module exports if this module currently re-exports `run_agent_loop` or related helpers.
- Prefer additive wrappers or signature expansion here instead of forcing unrelated callers to know internals.

### `src/agent/loop_impl.rs`

- Generate a fresh `turn_id` with `uuid::Uuid::new_v4().to_string()` for each assistant turn execution.
- Accept `Arc<dyn Observer>` as a parameter.
- Emit:
  - `TurnStarted` immediately after turn context is assembled and before provider execution
  - `CompletionFinished` once the model stream finishes successfully enough to produce final completion metadata
  - `TurnFinished` on every exit path with status, elapsed timing, token/accounting fields available at this level, and the generated `turn_id`
  - guard events for inbound, tool-call, and tool-batch guard passes
- Make guard-event capture precise, including the synthetic modify path:
  - do not rely only on final `Verdict`
  - extend the guard-check plumbing so each evaluated guard produces a small structured outcome record containing:
    - `gate_id`
    - whether it denied
    - whether it requested approval
    - whether it mutated the message or tool arguments
    - optional reason / severity text already available at that layer
  - `Turn::check_inbound()` must use those per-guard outcomes so a guard that mutates and returns `Allow` still yields a `GuardModified` event attributed to the correct `gate_id`
  - keep the existing external behavior of guard decisions unchanged; this is observability-only plumbing
- Keep the event emission side-effect-only so compile/runtime behavior is unchanged when using `NoopObserver`.

### `src/turn/verdicts.rs`

- Introduce the per-guard traced outcome type used by observability plumbing.
- Preserve existing `Verdict` semantics and public guard behavior.
- Provide helper conversion so existing callers can still work with `Verdict`, while observer-aware callers can inspect mutation / approval / denial attribution per gate.

### `src/turn/mod.rs`

- Update inbound/tool/tool-batch checking helpers to optionally return the traced per-guard outcomes alongside the existing final decision.
- Specifically fix the current synthetic `Modify` upgrade path:
  - capture which guard changed content while it is being evaluated
  - return that attribution even if the final `Verdict::Modify` is only synthesized after a baseline diff
- Avoid changing policy ordering or mutation behavior.

### `src/agent/shell_execute.rs`

- Accept `observer: Arc<dyn Observer>` and `turn_id: Option<String>` or `&str` on the guarded shell execution path that already handles actual tool execution.
- Emit:
  - `ToolCallStarted` immediately before shell execution begins
  - `ToolCallFinished` on every completion path, including denied / failed / timed-out / signaled outcomes if those distinctions are available here
- Include session/turn/plan identifiers already present in the execution context.
- Do not bypass the shared shell execution path.

### `src/session_runtime/drain.rs`

- Add observer-aware queue-drain wrappers or signature expansion so the fresh-turn drain path can propagate both:
  - `Arc<dyn Observer>`
  - the `turn_id` of the last successfully completed assistant turn
- Fix the review gap explicitly:
  - the fresh-turn drain result must carry `Option<String>` for `last_successful_turn_id` in addition to the existing `last_assistant_response`
  - when multiple queue rows are processed, update that field each time a turn finishes successfully, so the final return value is the most recent successful `turn_id`
- Keep older wrappers if needed so unaffected callers do not churn all at once.

### `src/agent/child_drain.rs`

- Consume the new fresh-turn drain result that includes `last_successful_turn_id`.
- Pass that `turn_id` into plan handoff / patching paths so:
  - `PlanRunCreated.caused_by_turn_id`
  - `PlanRunPatched.caused_by_turn_id`
  can be populated correctly.
- Keep the existing T2 child-drain behavior and queue semantics unchanged.

### `src/plan/patch.rs`

- This is the real source of plan creation and patch application, so emit:
  - `PlanRunCreated` when a new plan run row is created
  - `PlanRunPatched` when a waiting plan is patched
- Include `caused_by_turn_id` sourced from `child_drain.rs`.
- If terminal state transitions are set here for special patch outcomes, emit matching terminal events here rather than assuming `runner.rs` owns them.
- Keep all DB mutation ordering unchanged; emit after the corresponding mutation has succeeded.

### `src/store.rs`

- Add one store-layer helper that returns the count of persisted step-attempt rows for a given `plan_run_id` across all steps.
- Expose that helper to `src/plan/runner.rs` so `PlanCompleted.total_attempts` comes from durable state, not in-memory counters or max-attempt-index heuristics.
- Keep this helper read-only and isolated from mutation code so it does not change plan execution semantics.

### `src/plan/runner.rs`

- Accept `Arc<dyn Observer>`.
- Emit:
  - `PlanStepAttemptStarted`
  - `PlanStepAttemptFinished`
  - `PlanWaitingT2`
  - `PlanCompleted`
  - `PlanFailed`
- Do not assume this file is the only producer of plan lifecycle events; creation/patching remain in `patch.rs`, recovery remains in `recovery.rs`.
- Use the new `src/store.rs` helper to compute `PlanCompleted.total_attempts` as the count of persisted attempt rows for the run across all steps.
- If terminal success/failure can happen from more than one location, define one ownership rule per terminal path in the implementation notes to prevent duplicate emits.

### `src/plan/notify.rs`

- Make this file the sole emitter of `FailureNotifiedToT2`.
- Accept `Arc<dyn Observer>` at the durable notification boundary.
- If any helper here currently runs inside a transaction, split responsibilities:
  - transaction-time code returns a small post-commit notification marker/data structure
  - a notify-layer post-commit helper in this file emits `FailureNotifiedToT2` exactly once after durable success
- No other file emits `FailureNotifiedToT2`.

### `src/plan/recovery.rs`

- Accept `Arc<dyn Observer>`.
- Emit `PlanRecovered` when recovery logic successfully transitions or recreates a run as defined by the task.
- If recovery currently invokes failure notification during transaction handling, capture the post-commit marker from `src/plan/notify.rs` and hand control back to the notify-layer post-commit helper after the transaction succeeds.
- `src/plan/recovery.rs` must not emit `FailureNotifiedToT2` directly.

### `src/plan.rs`

- Thread observer through top-level plan entrypoints exported from this facade to `runner`, `patch`, `recovery`, and `notify`.

### `src/main.rs`

- No change.
- Runtime observer construction is owned by `src/app/session_run.rs` for CLI mode and `src/server/mod.rs` for server mode, so `src/main.rs` stays untouched in this task.

### `src/config/runtime.rs`

- No planned change.
- Do not add observer to `Config`; that would cause unnecessary widespread fixture churn and is not required by the task.

### `src/session/jsonl.rs`

- No change. Explicitly excluded by `TASK.md`.

## 3. What Tests To Write

### New observability unit tests

- `src/observe/sqlite.rs`
  - inserting an event writes one row into `trace_events`
  - indexed id columns are extracted correctly for at least one turn event and one plan event
  - `payload_json` round-trips through serde for representative variants
  - observer swallows serialization / insert failures without panicking
- `src/observe/mod.rs`
  - `MultiObserver` forwards the same borrowed event to every child observer
  - `NoopObserver` accepts every event and does nothing
  - `build_observer()` obeys the explicit default rule:
    - low-level/test call sites can inject `NoopObserver`
    - runtime startup builds SQLite plus optional OTEL
  - representative dormant eval variants (`EvalRunStarted`, `EvalRunFinished`) still serialize and persist correctly even without active emitters elsewhere in the tree
- `src/observe/otel.rs`
  - setup failure path is swallowed without panic
  - export failure path is swallowed without panic and does not bubble errors to the caller

### Turn and guard tests

- `src/agent/loop_impl.rs` or nearby tests
  - one successful turn emits `TurnStarted`, `CompletionFinished`, and `TurnFinished` with the same `turn_id`
  - a failed / denied turn still emits `TurnFinished`
  - guard denial emits `GuardDenied`
  - approval-required path emits `GuardApprovalRequested` and then granted/denied event according to the branch
- `src/turn/mod.rs` / `src/turn/verdicts.rs`
  - a guard that mutates in place but returns `Allow` still produces traced per-guard metadata marking that exact `gate_id` as modified
  - multiple guards with only one mutating guard attribute `GuardModified` to the correct guard, not a synthetic anonymous source

### Shell/tool execution tests

- `src/agent/shell_execute.rs`
  - successful shell tool call emits matching `ToolCallStarted` / `ToolCallFinished`
  - failed shell tool call still emits `ToolCallFinished`
  - denied execution path does not lose the finish event if that path is owned here

### Queue drain / child drain tests

- `src/session_runtime/drain.rs`
  - fresh-turn drain returns `last_successful_turn_id` when a turn completes successfully
  - if multiple rows run, the final returned id is the most recent successful turn
  - if no turn succeeds, `last_successful_turn_id` is `None`
- `src/agent/child_drain.rs`
  - T2 plan handoff passes the last successful `turn_id` into the patch layer and resulting plan events carry `caused_by_turn_id`

### Plan lifecycle tests

- `src/plan/patch.rs`
  - creating a new plan emits exactly one `PlanRunCreated` with `caused_by_turn_id`
  - patching a waiting plan emits exactly one `PlanRunPatched` with `caused_by_turn_id`
- `src/store.rs`
  - attempt-count helper returns the number of persisted attempt rows across all steps for a run
- `src/plan/runner.rs`
  - each attempt emits paired `PlanStepAttemptStarted` / `PlanStepAttemptFinished`
  - waiting-for-T2 transition emits `PlanWaitingT2`
  - successful completion emits `PlanCompleted`
  - failed completion emits `PlanFailed`
  - `PlanCompleted.total_attempts` equals the count returned by the store helper, not the max attempt index of one step
- `src/plan/recovery.rs` + `src/plan/notify.rs`
  - recovery path emits `PlanRecovered`
  - failure notification emits `FailureNotifiedToT2` exactly once through the notify-layer post-commit helper, even when recovery triggers the underlying notification work

### Compile-surface and integration tests

- Server tests that construct `ServerState` continue compiling with `NoopObserver`.
- Existing tests around queue draining, plan patching, and runtime startup continue passing with observer plumbing present.
- Full regression run after implementation:
  - `cargo fmt --check`
  - `cargo clippy -- -D warnings`
  - `cargo test`
  - `cargo build --release`
  - `cargo test --features integration` when credentials are available

## 4. Order Of Operations

The sequence below is intended to keep the tree compiling and tests green after each incremental step.

1. Add dependencies in `Cargo.toml`, export `src/observe` from `src/lib.rs`, and implement `src/observe/mod.rs` with `TraceEvent`, `Observer: Send + Sync + 'static`, the borrowed `emit(&TraceEvent)` API, `NoopObserver`, `MultiObserver`, and `build_observer()`.
2. Implement `src/observe/sqlite.rs` and `src/observe/otel.rs` plus unit tests for the observer layer, including the runtime-default rule test, dormant eval-variant coverage, and the OTEL silent-drop failure-path tests. At this point nothing else in the tree depends on the new module yet.
3. Add traced guard outcome plumbing in `src/turn/verdicts.rs` and `src/turn/mod.rs`, including explicit attribution for in-place mutation guards that return `Allow`. Keep existing external behavior unchanged and add focused tests for the synthetic-modify case.
4. Thread observer + `turn_id` through `src/agent/loop_impl.rs`, `src/agent/mod.rs`, and `src/agent/shell_execute.rs`, then add turn/tool/guard emission tests. Keep call-site wrappers or default `NoopObserver` shims until all callers are migrated.
5. Extend `src/session_runtime/drain.rs` to return `last_successful_turn_id` alongside existing data, then update `src/agent/child_drain.rs` to pass that id into plan patching. Add queue-drain and child-drain tests before moving to plan events.
6. Add the persisted attempt-count helper in `src/store.rs`, then instrument `src/plan/patch.rs`, `src/plan/runner.rs`, `src/plan/notify.rs`, `src/plan/recovery.rs`, and the `src/plan.rs` facade. In this same step, make `src/plan/notify.rs` the sole emitter of `FailureNotifiedToT2` and add the post-commit helper boundary so `recovery.rs` does not emit that event directly.
7. Wire runtime observer construction through the CLI path in `src/app/session_run.rs`.
8. Wire runtime observer construction through the server path in `src/server/mod.rs`, and in the same change update `src/server/state.rs`, `src/server/queue_worker.rs`, and all affected server test fixtures in `src/server/auth.rs`, `src/server/http.rs`, and `src/server/queue.rs` so the tree never enters a non-compiling intermediate state.
9. Run the full required validation suite: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release`, and `cargo test --features integration` when auth is available.

## 5. Risk Assessment

### Highest-risk implementation areas

- Guard attribution is the most subtle part. The current code synthesizes `Modify` after comparing message state, so observability must capture per-guard mutation metadata during guard evaluation instead of inferring it later from the final verdict.
- Plan lifecycle ownership is split across files. `PlanRunCreated` / `PlanRunPatched` belong in `src/plan/patch.rs`, `FailureNotifiedToT2` belongs only to `src/plan/notify.rs`, and recovery must only hand off post-commit notification data rather than emitting that event itself.
- `PlanCompleted.total_attempts` cannot be derived from a max-attempt-index helper if multiple steps exist. The implementation must count persisted attempts across the whole run through the new store-layer helper.
- Threading `observer` through server state is compile-sensitive because many tests construct `ServerState` directly. The struct change and all test fixture updates must land together.
- Returning `last_successful_turn_id` from queue drain changes an existing interface. Wrapper functions or careful signature migration are needed to avoid unnecessary churn.

### Lower-risk areas

- `SqliteObserver` is straightforward because it is append-only and isolated in `traces.sqlite`.
- The borrowed `emit(&TraceEvent)` API keeps `MultiObserver` simple and avoids unnecessary `TraceEvent` cloning requirements.
- `EvalRunStarted` / `EvalRunFinished` are schema-complete but intentionally dormant because there is no current eval runtime to instrument.
- Keeping observer state out of `Config` avoids broad fixture churn and reduces the chance of unrelated compilation fallout.

### Failure modes to watch

- Double-emitting terminal plan events when both patch/recovery/runner believe they own the same transition.
- Emitting `FailureNotifiedToT2` from both recovery and notify layers instead of only the notify layer post-commit boundary.
- Missing `TurnFinished` / `ToolCallFinished` on error paths.
- Emitting guard-modification events without a concrete `gate_id`.
- Blocking runtime progress if OTEL initialization/export is allowed to fail loudly instead of degrading to drop-on-failure behavior.
