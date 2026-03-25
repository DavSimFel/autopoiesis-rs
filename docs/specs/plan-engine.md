# Plan Engine — Durable Orchestration Spec

> **Status:** Design complete (adversarial debate, 3 rounds, Silas × Codex + operator direction)
> **Date:** 2026-03-25
> **Origin:** `/tmp/plans-debate/` (PROPOSAL → CRITIQUE → REBUTTAL → ROUND2 → ROUND3_OPERATOR → ROUND3)

---

## Summary

A durable plan execution engine that orchestrates multiple agents across steps. Plans mix agent turns (open-ended) with scripted checks (deterministic). State persists in SQLite. Crash at any point → resume from last completed step.

Replaces `codex-loop.sh`. Enables T2→T3 orchestration, repeatable workflows, and self-verifying coding loops — all through one primitive.

---

## Core Concepts

### Plan
A sequence of steps with postconditions. Authored by an operator (TOML file) or by an agent (T2 writes a plan for T3 execution). Compiled to a normalized `PlanSpec` JSON before execution. The compiled spec is snapshotted into `plan_runs.definition_json` at start — resume is deterministic.

### Step
One unit of work. Two kinds:
- **Agent step:** Spawns a session (any tier, any model, any skills). The agent decides *how* to accomplish the goal. The step defines *what done looks like* via postconditions.
- **Script step:** Runs a shell command. Deterministic. Used for tests, linting, builds, health checks.

### Check
A postcondition on a step. Runs after the step claims completion. Shell executor with typed expectations.

### Verdict
Four states: **Pass**, **Fail** (with observed output), **Inconclusive** (returns to agent judgment), **Waived** (explicit override with principal + reason).

---

## Data Model (SQLite)

```sql
CREATE TABLE plan_runs (
    id TEXT PRIMARY KEY,
    root_session_id TEXT NOT NULL,       -- parent session that owns this plan
    definition_json TEXT NOT NULL,       -- compiled PlanSpec snapshot
    status TEXT NOT NULL DEFAULT 'pending', -- pending | running | completed | failed
    current_step_index INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(root_session_id) REFERENCES sessions(id)
);

CREATE TABLE plan_steps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    plan_run_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,                  -- agent | script
    status TEXT NOT NULL DEFAULT 'pending', -- pending | running | completed | failed | retrying
    max_retries INTEGER NOT NULL DEFAULT 3,
    retry_count INTEGER NOT NULL DEFAULT 0,
    child_session_id TEXT,              -- set for agent steps
    agent_config_json TEXT,             -- tier, model, skills, task for agent steps
    script_command TEXT,                -- shell command for script steps
    started_at TEXT,
    completed_at TEXT,
    FOREIGN KEY(plan_run_id) REFERENCES plan_runs(id) ON DELETE CASCADE
);

CREATE TABLE check_results (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    step_id INTEGER NOT NULL,
    attempt INTEGER NOT NULL,           -- which retry attempt (0-based)
    check_id TEXT NOT NULL,             -- stable id from plan definition
    verdict TEXT NOT NULL,              -- pass | fail | inconclusive | waived
    observed_json TEXT NOT NULL,        -- { stdout, stderr, exit_code, artifact_path }
    expectation_json TEXT NOT NULL,     -- what was expected
    waived_by TEXT,                     -- principal who waived (if verdict=waived)
    waived_reason TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(step_id) REFERENCES plan_steps(id) ON DELETE CASCADE
);
```

---

## Plan Spec Format

### TOML (operator-authored, repeatable)

```toml
[plan]
name = "implement-feature"
description = "Plan → implement → test → commit"

[[step]]
name = "write-plan"
kind = "agent"
description = "Read the codebase and write PLAN.md"
tier = "t2"
model = "gpt5_reasoning"
skills = []
task = "Read all source files. Write PLAN.md with exact changes, tests, and order of operations."
max_retries = 3

[[step.check]]
id = "plan-exists"
command = "test -f PLAN.md"
expect = { exit_code = 0 }

[[step.check]]
id = "plan-has-sections"
command = "grep -c '## ' PLAN.md"
expect = { exit_code = 0, stdout_contains = "5" }

[[step]]
name = "implement"
kind = "agent"
description = "Implement changes per PLAN.md"
tier = "t3"
model = "gpt5_mini"
skills = ["code-review"]
task = "Implement the changes described in PLAN.md. Run cargo check after each file change."
max_retries = 5

[[step.check]]
id = "compiles"
command = "cargo check 2>&1"
expect = { exit_code = 0 }

[[step.check]]
id = "tests-pass"
command = "cargo test 2>&1"
expect = { exit_code = 0 }

[[step.check]]
id = "no-clippy-warnings"
command = "cargo clippy -- -D warnings 2>&1"
expect = { exit_code = 0 }

[[step]]
name = "commit"
kind = "script"
command = "git add -A && git commit -m 'feat: implement feature'"
max_retries = 1

[[step.check]]
id = "committed"
command = "git log -1 --oneline"
expect = { exit_code = 0 }
```

### Agent-authored (T2 emits for T3 execution)

T2 can emit a PlanSpec as a structured tool call result. Same normalized shape, different origin. The plan engine doesn't care who authored it.

---

## Execution Flow

```
plan_engine::run(store, config, plan_spec) -> Result<PlanRunResult>
  │
  ├── Create plan_run row (status=running)
  ├── For each step (respecting current_step_index for resume):
  │     │
  │     ├── [Agent step]
  │     │     ├── Build child Config (tier, model, skills from step)
  │     │     ├── spawn_child_session() — reuse existing spawn infrastructure
  │     │     ├── drain_session_until_idle() — reuse existing drain_queue
  │     │     ├── Run postcondition checks
  │     │     ├── If all pass → mark step completed, advance
  │     │     ├── If any fail → build RetryContext from failed checks
  │     │     │     ├── Enqueue retry message with structured findings
  │     │     │     ├── drain_session_until_idle() again (same child session)
  │     │     │     └── Re-run checks. Loop until pass or max_retries.
  │     │     └── If max_retries exceeded → mark step failed, stop plan
  │     │
  │     ├── [Script step]
  │     │     ├── Execute shell command
  │     │     ├── Run postcondition checks (usually just exit code)
  │     │     ├── If pass → advance
  │     │     └── If fail → retry (re-run command) or fail plan
  │     │
  │     └── Update plan_runs.current_step_index after each step
  │
  ├── All steps completed → status=completed
  └── Any step failed after retries → status=failed
```

### Crash Recovery

On startup, query for `plan_runs WHERE status = 'running'`. For each:
1. Read `current_step_index` — that's where we were
2. Check if current step has `status = 'running'` — that's the crashed step
3. Treat running step as failed attempt, increment retry_count
4. Resume from current step (not restart from beginning)

### Retry Context

When an agent step fails checks, the retry message includes structured findings:

```json
{
  "attempt": 2,
  "failed_checks": [
    {
      "check_id": "tests-pass",
      "summary": "3 tests failed: test_spawn_child, test_budget_check, test_model_routing",
      "observed": { "exit_code": 101, "stderr_tail": "..." },
      "artifact_path": "sessions/child-123/results/check-tests-pass-attempt-2.txt"
    }
  ]
}
```

This gets enqueued as the next user message in the child session. The agent's session history already has context from previous attempts. The retry context tells it exactly what failed and why.

---

## Multi-Agent Orchestration

Different steps can use different agents:

```toml
[[step]]
name = "analyze"
tier = "t2"
model = "gpt5_reasoning"    # strong reasoning model
skills = []
task = "Analyze the codebase and identify all affected files"

[[step]]
name = "implement"
tier = "t3"
model = "gpt5_mini"          # fast cheap model
skills = ["code-review"]
task = "Implement the changes from the analysis"

[[step]]
name = "security-review"
tier = "t2"
model = "gpt5_reasoning"    # back to reasoning for review
skills = ["security-audit"]
task = "Review the implementation for security issues"
```

Each step spawns its own session with its own tier/model/skills. The plan engine coordinates. Steps are sequential by default (each sees the previous step's output via postcondition results). No DAG scheduling in v1.

---

## Observability

All state is queryable from SQLite:

```sql
-- What step is the plan on?
SELECT current_step_index, status FROM plan_runs WHERE id = ?;

-- How many retries on the current step?
SELECT retry_count, max_retries FROM plan_steps WHERE plan_run_id = ? AND step_index = ?;

-- What failed on the last attempt?
SELECT check_id, verdict, observed_json FROM check_results
WHERE step_id = ? ORDER BY attempt DESC;

-- Full plan history
SELECT s.name, s.status, s.retry_count, s.completed_at
FROM plan_steps s WHERE s.plan_run_id = ? ORDER BY s.step_index;
```

CLI commands:
```bash
autopoiesis plan run <plan.toml>           # start a plan
autopoiesis plan status [plan-run-id]      # show current step + retries
autopoiesis plan list                      # list active/recent plans
autopoiesis plan resume [plan-run-id]      # resume crashed plan
autopoiesis plan cancel [plan-run-id]      # stop a running plan
```

---

## Relationship to Existing Infrastructure

| Existing | Role in Plan Engine |
|----------|-------------------|
| `store.rs` (SQLite) | Owns plan_runs, plan_steps, check_results tables |
| `spawn_child()` | Creates child sessions for agent steps |
| `drain_queue()` | Executes agent turns within a step |
| `session` (JSONL) | Audit log — human-readable summaries of step results |
| `build_turn_for_config()` | Builds correct tool set per step's tier |
| `SkillCatalog` | Loads skills specified in step config |
| `ModelSelector` | Resolves model from catalog for each step |
| `Shell` tool | Executes script steps and check commands |

The plan engine is a **new outer layer**. It doesn't replace anything — it orchestrates what already exists.

---

## What This Replaces

`codex-loop.sh` becomes a TOML plan file:

```toml
[plan]
name = "codex-loop"

[[step]]
name = "plan"
kind = "agent"
tier = "t2"
model = "gpt5_reasoning"
task = "Read all source files. Write PLAN.md."
max_retries = 10

[[step.check]]
id = "plan-exists"
command = "test -f PLAN.md"
expect = { exit_code = 0 }

[[step]]
name = "implement"
kind = "agent"
tier = "t3"
model = "gpt5_mini"
task = "Implement per PLAN.md. Run cargo check + cargo test after each change."
max_retries = 10

[[step.check]]
id = "fmt"
command = "cargo fmt --check"
expect = { exit_code = 0 }

[[step.check]]
id = "clippy"
command = "cargo clippy -- -D warnings 2>&1"
expect = { exit_code = 0 }

[[step.check]]
id = "tests"
command = "cargo test 2>&1"
expect = { exit_code = 0 }

[[step]]
name = "commit"
kind = "script"
command = "git add -A && git commit -m 'feat: implement changes'"

[[step.check]]
id = "committed"
command = "git log -1 --oneline"
expect = { exit_code = 0 }
```

Same behavior, durable, observable, crash-recoverable.

---

## Not Building Yet

- DAG scheduling (parallel steps, dependency chains)
- Plan templates with parameters
- Remote agent execution
- Plan marketplace
- Approval gates within plans (future: T1 reviews T2's plan before execution)
- Cron-triggered plan execution (future: same engine, scheduled trigger)

---

## Implementation Order

1. SQLite schema (plan_runs, plan_steps, check_results tables)
2. PlanSpec parser (TOML → normalized JSON)
3. Check executor (shell + typed expectations + verdict)
4. Script step executor
5. Agent step executor (reuses spawn_child + drain_queue)
6. Retry loop with structured context injection
7. Plan runner (orchestrates steps sequentially)
8. Crash recovery (resume from current_step_index)
9. CLI commands (plan run/status/list/resume/cancel)
10. Replace codex-loop.sh with a TOML plan

Estimated: 6-8 codex sessions, ~3-4 days.
