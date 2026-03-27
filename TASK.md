# Task: Observability Layer — Structured Trace Events

## Design (from prior analysis — follow this exactly)

Observability is NOT tracing subscriber scraping. It is structured domain events at orchestration boundaries only, stored in SQLite, shipped via OpenTelemetry to OpenObserve.

Session JSONL = transcript (do not touch).
Trace events = operational history (new).

## What to build

### 1. Trace event schema (`src/observe/mod.rs` or `src/observe/events.rs`)

Define a Rust enum covering exactly these event types:

```rust
pub enum TraceEvent {
    EvalRunStarted { eval_run_id, case_id, git_sha, config_digest, started_at },
    EvalRunFinished { eval_run_id, outcome, finished_at },
    TurnStarted { session_id, turn_id, tier, parent_session_id, resolved_model, context_message_count, trimmed, budget_snapshot },
    TurnFinished { turn_id, stop_reason, input_tokens, output_tokens, reasoning_tokens, duration_ms },
    CompletionFinished { turn_id, provider, model, provider_response_id, stop_reason, input_tokens, output_tokens, reasoning_tokens, duration_ms, tool_call_count, error },
    GuardDenied { turn_id, tool_call_id, stage, gate_id, reason, severity },
    GuardModified { turn_id, stage, gate_id },
    GuardApprovalRequested { turn_id, tool_call_id, gate_id, severity, command_summary },
    GuardApprovalGranted { turn_id, tool_call_id, gate_id },
    GuardApprovalDenied { turn_id, tool_call_id, gate_id },
    ToolCallStarted { turn_id, tool_call_id, tool_name, command_summary },
    ToolCallFinished { turn_id, tool_call_id, tool_name, approved, denied, exit_code, duration_ms, artifact_ref, truncated },
    PlanRunCreated { plan_run_id, owner_session_id, revision, step_count, caused_by_turn_id },
    PlanRunPatched { plan_run_id, revision, new_step_count, caused_by_turn_id },
    PlanStepAttemptStarted { plan_run_id, revision, step_index, step_id, attempt, child_session_id },
    PlanStepAttemptFinished { plan_run_id, revision, step_index, step_id, attempt, status, failure_reason, duration_ms },
    PlanWaitingT2 { plan_run_id, revision, step_index, step_id, attempt, failure_summary },
    PlanRecovered { plan_run_id, revision },
    PlanCompleted { plan_run_id, revision, total_attempts, duration_ms },
    PlanFailed { plan_run_id, revision, failure_summary },
    FailureNotifiedToT2 { plan_run_id, revision, target_session_id },
}
```

All string fields use `String`. All option fields use `Option<String>` or `Option<i64>`. Derive `Debug, Serialize, Deserialize`.

### 2. Observer trait (`src/observe/mod.rs`)

```rust
pub trait Observer: Send + Sync {
    fn emit(&self, event: TraceEvent);
}

pub struct NoopObserver;
impl Observer for NoopObserver {
    fn emit(&self, _event: TraceEvent) {}
}
```

Keep it minimal. No async required — emit is fire-and-forget.

### 3. SQLite trace store (`src/observe/sqlite.rs`)

`SqliteObserver` implements `Observer`:
- Opens a dedicated `traces.sqlite` in the sessions dir (separate from queue.sqlite)
- Creates one table: `trace_events(id INTEGER PRIMARY KEY, event_type TEXT NOT NULL, session_id TEXT, turn_id TEXT, plan_run_id TEXT, eval_run_id TEXT, timestamp TEXT NOT NULL, payload_json TEXT NOT NULL)`
- Indexes on: `(eval_run_id, timestamp)`, `(session_id, timestamp)`, `(plan_run_id, timestamp)`
- `emit()` serializes the event to JSON and inserts synchronously
- Thread-safe via `Mutex<Connection>`

### 4. OTel exporter (`src/observe/otel.rs`)

`OtelObserver` implements `Observer`:
- Uses `opentelemetry` + `opentelemetry-otlp` crates
- Each `emit()` call creates an OTel span with event_type as span name and all fields as attributes
- Ships to the endpoint configured in `ZO_OTEL_ENDPOINT` env var (default: `http://localhost:5081`)
- If endpoint is not set or connection fails, silently drops (never crash the agent for observability)

### 5. Fan-out observer (`src/observe/mod.rs`)

```rust
pub struct MultiObserver {
    observers: Vec<Box<dyn Observer>>,
}
impl MultiObserver {
    pub fn new(observers: Vec<Box<dyn Observer>>) -> Self { ... }
}
impl Observer for MultiObserver {
    fn emit(&self, event: TraceEvent) {
        for obs in &self.observers { obs.emit(event.clone()); }
    }
}
```

### 6. Wire into orchestration boundaries

Wire `Arc<dyn Observer>` into these locations only (not everywhere):

**`src/agent/loop_impl.rs`**:
- Emit `TurnStarted` at the start of a turn
- Emit `TurnFinished` when the turn returns
- Emit `GuardDenied/Modified/ApprovalRequested/Granted/Denied` when guard verdicts fire
- Emit `CompletionFinished` when `provider.stream_completion()` returns

**`src/agent/shell_execute.rs`**:
- Emit `ToolCallStarted` before execution
- Emit `ToolCallFinished` after execution

**`src/plan/runner.rs`**:
- Emit `PlanRunCreated` when a new plan run is created
- Emit `PlanRunPatched` on revision
- Emit `PlanStepAttemptStarted/Finished` around step execution
- Emit `PlanWaitingT2` when moving to waiting state
- Emit `PlanCompleted/Failed` on terminal transitions

**`src/plan/recovery.rs`**:
- Emit `PlanRecovered` after crash recovery

**`src/plan/notify.rs`**:
- Emit `FailureNotifiedToT2` after successful notification

### 7. Threading the observer through

- Add `observer: Arc<dyn Observer>` to `Config` OR pass it separately at the call site
- The drain path in `src/session_runtime/drain.rs` already receives `Config` — thread observer from there
- Add a `turn_id` generator: `uuid::Uuid::new_v4().to_string()` at turn start, passed down through the turn

### 8. CLI integration (`src/main.rs` or `src/app/session_run.rs`)

```rust
fn build_observer(sessions_dir: &Path) -> Arc<dyn Observer> {
    let mut observers: Vec<Box<dyn Observer>> = vec![
        Box::new(SqliteObserver::new(sessions_dir.join("traces.sqlite")).unwrap_or_else(|_| NoopObserver)),
    ];
    if let Ok(endpoint) = std::env::var("ZO_OTEL_ENDPOINT") {
        if let Ok(otel) = OtelObserver::new(&endpoint) {
            observers.push(Box::new(otel));
        }
    }
    Arc::new(MultiObserver::new(observers))
}
```

### 9. Cargo.toml additions

```toml
[dependencies]
uuid = { version = "1", features = ["v4"] }
opentelemetry = "0.27"
opentelemetry-otlp = { version = "0.27", features = ["grpc-tonic"] }
opentelemetry_sdk = { version = "0.27", features = ["rt-tokio"] }
```

## Constraints

- Observer is `Arc<dyn Observer>` everywhere — not a global
- `NoopObserver` is the default — zero overhead when not configured
- Never panic or return errors from `emit()` — observability must not crash the agent
- Do NOT add observer to test helpers unless specifically needed
- Do NOT touch session/jsonl.rs
- All 606 tests must still pass
- After implementation: `cargo test` count must be 606 or higher
- Commit message: `feat: observability layer — structured trace events, SQLite + OTel`

When completely finished, run: openclaw system event --text "Done: observability layer implemented — trace events, SQLite store, OTel exporter" --mode now
