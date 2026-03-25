# Plan Engine — T2's Execution Model

> **Status:** Converged design (4-round adversarial debate, Silas × Codex + operator direction)
> **Date:** 2026-03-25
> **Origin:** `/tmp/plans-debate/` (9 debate files)
> **Supersedes:** Previous plan-engine spec v1

---

## Summary

Plans are T2's native output. T2 decomposes problems, reasons about approach, and emits structured plans that the runtime executes. The plan runner is a tiny sequential executor backed by SQLite. Failures go back to T2 — T2 is the orchestrator, the retry policy, and the error handler.

A one-step plan with a single T3 is just a spawn. A hundred-step plan running over weeks is the same primitive at scale. Plans compose from existing infrastructure: `spawn_child()` for agent steps, guarded shell execution for scripted steps, the message queue for notifications, and SQLite for durable state.

---

## Core Concepts

### Plan
T2's structured output. A sequence of steps with postconditions. Emitted as a `plan-json` fenced block in T2's assistant response. The runtime parses it, validates it, and executes it.

### Step
One unit of work. Two kinds:
- **Spawn step:** Creates a child session (any tier, any model, any skills). The agent decides how to accomplish the goal. Postconditions define what done looks like.
- **Shell step:** Runs a command through the existing guarded shell path (Turn → ShellSafety → ExfilDetector → OutputCap). Deterministic.

### Check
A postcondition on a step. Shell command with typed expectations. Runs through the same guarded path as shell steps.

### PlanAction
T2's control message. Three kinds:
- **Plan:** Create a new run or patch the current one (replace remaining steps).
- **Done:** Mark the run completed.
- **Escalate:** Mark failed and hand upward to T1/operator.

---

## Data Structures

### PlanAction (T2's output)

```rust
struct PlanAction {
    kind: PlanActionKind,              // Plan | Done | Escalate
    plan_run_id: Option<String>,       // None = new run, Some = patch current
    replace_from_step: Option<usize>,  // replace suffix from this index
    note: Option<String>,              // T2's reasoning
    steps: Vec<PlanStepSpec>,
}

enum PlanActionKind { Plan, Done, Escalate }

#[serde(tag = "kind", rename_all = "snake_case")]
enum PlanStepSpec {
    Spawn {
        id: String,
        spawn: SpawnStepSpec,
        checks: Vec<ShellCheckSpec>,
        max_attempts: u32,
    },
    Shell {
        id: String,
        command: String,
        timeout_ms: Option<u64>,
        checks: Vec<ShellCheckSpec>,
        max_attempts: u32,
    },
}

struct SpawnStepSpec {
    task: String,
    task_kind: Option<String>,
    tier: String,                      // t1, t2, t3
    model_override: Option<String>,
    reasoning_override: Option<String>,
    skills: Vec<String>,
    skill_token_budget: Option<u64>,
}

struct ShellCheckSpec {
    id: String,
    command: String,
    expect: ShellExpectation,
}

struct ShellExpectation {
    exit_code: Option<i32>,
    stdout_contains: Option<String>,
    stderr_contains: Option<String>,
    stdout_equals: Option<String>,
}
```

### Check Verdicts

Three states only:
- **Pass** — expectation met
- **Fail** — expectation not met (with observed output)
- **Inconclusive** — runtime cannot evaluate, returns to T2 judgment

No `Waived` in v1. If a check can't be evaluated, it's Inconclusive and T2 decides.

---

## Durable State (SQLite)

Two tables. Not three.

```sql
CREATE TABLE plan_runs (
    id TEXT PRIMARY KEY,
    owner_session_id TEXT NOT NULL,       -- the T2 session that owns this plan
    topic TEXT,                           -- optional topic association
    trigger_source TEXT,                  -- cron, webhook, user, agent
    status TEXT NOT NULL DEFAULT 'pending',
        -- pending | running | waiting_t2 | completed | failed
    revision INTEGER NOT NULL DEFAULT 1,
    current_step_index INTEGER NOT NULL DEFAULT 0,
    active_child_session_id TEXT,         -- set during spawn step execution
    definition_json TEXT NOT NULL,        -- current PlanAction snapshot
    last_failure_json TEXT,               -- structured failure for T2
    claimed_at INTEGER,                   -- lease (same pattern as messages table)
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(owner_session_id) REFERENCES sessions(id)
);

CREATE TABLE plan_step_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    plan_run_id TEXT NOT NULL,
    revision INTEGER NOT NULL,           -- which plan revision
    step_index INTEGER NOT NULL,
    step_id TEXT NOT NULL,               -- stable id from step spec
    attempt INTEGER NOT NULL,            -- 0-based
    status TEXT NOT NULL,                -- running | passed | failed | crashed
    child_session_id TEXT,               -- set for spawn steps
    summary_json TEXT NOT NULL,          -- child response or shell output
    checks_json TEXT NOT NULL,           -- Vec<CheckOutcome>
    started_at TEXT NOT NULL,
    finished_at TEXT,
    FOREIGN KEY(plan_run_id) REFERENCES plan_runs(id) ON DELETE CASCADE
);
```

`definition_json` owns step definitions. `plan_step_attempts` owns execution history. `claimed_at` provides the lease model (same pattern as the message queue's stale-claim recovery).

---

## How It Works

### T2 Emits a Plan

T2 produces a `plan-json` fenced block in its assistant response:

````markdown
```plan-json
{
  "kind": "plan",
  "steps": [
    {
      "kind": "spawn",
      "id": "implement",
      "spawn": {
        "tier": "t3",
        "task": "Implement the fix.",
        "task_kind": "code",
        "skills": ["code-review"]
      },
      "checks": [
        { "id": "tests", "command": "cargo test", "expect": { "exit_code": 0 } }
      ],
      "max_attempts": 3
    }
  ]
}
```
````

The runtime:
1. Extracts the fenced block from T2's last assistant message
2. Parses and validates with serde (reject anything malformed)
3. Creates or patches a `plan_run` row

This keeps T2 read-only (no new tool). T2 still only has `read_file`. The plan is a structured artifact, matching the identity-v2 spec's framing of T2 producing structured handoff artifacts.

### The Runner

```
plan_runner::tick(store, config) -> Result<()>
  │
  ├── Claim next pending/running plan_run (claimed_at lease)
  ├── Read current_step_index from definition_json
  │
  ├── [Spawn step]
  │     ├── Build SpawnRequest from SpawnStepSpec
  │     ├── spawn_child() + drain child queue
  │     ├── Run postcondition checks (guarded shell)
  │     ├── Record attempt in plan_step_attempts
  │     ├── If all checks pass → advance current_step_index
  │     └── If any check fails → waiting_t2, enqueue failure to T2
  │
  ├── [Shell step]
  │     ├── Execute through guarded shell path (Turn + guards)
  │     ├── Run postcondition checks
  │     ├── Record attempt
  │     ├── Pass → advance
  │     └── Fail → waiting_t2, enqueue failure to T2
  │
  ├── All steps done → status = completed
  └── Release lease
```

The runner does not invent policy. It executes steps, runs checks, and hands failures back to T2.

### Failure → T2 Intervenes

When a step fails, the runner:
1. Sets `plan_runs.status = 'waiting_t2'`
2. Stores structured failure in `last_failure_json`
3. Enqueues a message to T2's session with source `agent-plan-{run_id}`

The failure payload:
```json
{
  "kind": "plan_failure",
  "plan_run_id": "plan-123",
  "revision": 2,
  "step_index": 1,
  "step_id": "tests",
  "attempt": 2,
  "reason": "check_failed",
  "child_session_id": "child-456",
  "checks": [
    {
      "id": "cargo-test",
      "verdict": "fail",
      "observed": { "exit_code": 101, "artifact_path": "sessions/.../check.txt" }
    }
  ]
}
```

T2 processes this like any other message. It can:
- Emit a new `PlanAction` with `kind: "plan"` to patch the remaining steps
- Emit `kind: "done"` if the partial result is acceptable
- Emit `kind: "escalate"` to hand upward to T1/operator

### Plan Patching

When T2 patches a plan:
- `replace_from_step` must equal `current_step_index` (can only replace the remaining suffix)
- Completed steps are immutable
- `revision` increments
- `definition_json` is overwritten with the new step list
- `status` resets to `pending`

This supports "retry differently" and "adjust the approach" without a general patch language.

### Crash Recovery

On startup, query for stale `plan_runs WHERE status = 'running' AND claimed_at < stale_threshold`:
1. Mark the current attempt as `crashed` in `plan_step_attempts`
2. Set `plan_runs.status = 'waiting_t2'`
3. Enqueue a crash notification to T2's session
4. T2 decides whether to retry, replace steps, or escalate

**The runtime never blindly replays a crashed step.** Side effects may have already happened. T2 has the context to decide what to do. This is the honest version of durability.

---

## Topic Triggers

Plans can start from topic triggers. No new trigger system needed:
- Cron: enqueue message to T2 session with source `cron`
- Webhook: enqueue message to T2 session with source `webhook`
- User/operator: same as today

The existing "everything becomes a queued message" model handles this. `topic` and `trigger_source` on `plan_runs` are for observability only.

---

## The Single-Spawn Seam

A one-step plan is conceptually identical to `spawn_and_drain()`:

```rust
PlanAction {
    kind: PlanActionKind::Plan,
    steps: vec![PlanStepSpec::Spawn { /* one T3 */ }],
}
```

Implementation: keep `spawn_and_drain()` as a compatibility wrapper. Internally, converge on one-step plans so there aren't two execution paths forever.

---

## Security Model

### Shell Steps Through Guards
Shell steps and checks execute as synthetic `execute` tool calls through the existing Turn → ShellSafety → ExfilDetector → OutputCap pipeline. No second shell path. Standing approvals, taint-gated approvals, and protected path denials all apply.

### Provenance
Plan notifications use `agent-plan-{run_id}` as source → `Principal::Agent` (not a taint source). T2's plan output carries T2's session provenance. Child sessions carry their own provenance via existing `agent-{child_id}` pattern.

### No New Tool Surface
T2 still only has `read_file`. Plans are parsed from assistant text, not from a tool call. No tool surface expansion.

---

## Mapping to Existing Code

| Existing | Role |
|----------|------|
| `spawn_child()` | Creates child sessions for spawn steps |
| `drain_queue()` | Executes agent turns within spawn steps |
| `build_t2_turn()` | T2 planner sessions |
| `build_t3_turn()` | Shell steps, checks, T3 child execution |
| `store.rs` (SQLite) | New `plan_runs` + `plan_step_attempts` tables |
| `principal.rs` | `agent-plan-*` sources → `Principal::Agent` |
| Message queue | Failure notifications to T2, topic triggers |

New: `src/plan.rs` — parse PlanAction, claim/run steps, persist attempts, enqueue failures.

---

## Not Building Yet

- DAG scheduling / parallel steps
- Plan templates with parameters
- Operator-authored TOML plan files (v2 — after T2-authored plans prove the concept)
- Remote agent execution
- Plan marketplace
- Cron-triggered plan execution (trigger infrastructure exists, plan automation is v2)
- PTY-based interactive steps

---

## Implementation Order

1. SQLite schema (`plan_runs`, `plan_step_attempts`, lease helpers)
2. PlanAction parser (extract `plan-json` block from assistant text, serde validation)
3. Guarded shell executor helper (factor from agent.rs for reuse by shell steps + checks)
4. Plan runner (claim → execute step → record attempt → advance or notify T2)
5. Failure notification (enqueue structured failure to T2 session)
6. Plan patching (T2 replaces remaining suffix, revision increment)
7. Crash recovery (stale lease detection → `waiting_t2` → notify T2)
8. Wire into T2 drain path (after T2 turn, check for `plan-json` block)
9. CLI commands (`plan status`, `plan list` — observability only)
10. Migrate `spawn_and_drain()` callers to one-step plans

Estimated: 5-6 codex sessions, ~2-3 days.
