# Analysis

## Hard truths

1. The current system is not "always-on addressable actors". It is "one ambient `Config`, one resolved tier, plus spawned child sessions". The single biggest blockers are `src/config/agents.rs`, `src/config/load.rs`, `src/server/state.rs`, `src/server/queue_worker.rs`, `src/turn/tiers.rs`, and `src/child_session/create.rs`.

2. "Queue is the universal transport" is not implemented today as an agent capability. Agents only get `execute` or `read_file`. There is no agent-facing `enqueue_message(...)` tool. Current inter-tier behavior is a special mechanism: `spawn_child(...)` plus automatic child completion and T2 plan handoff in `src/agent/child_drain.rs`.

3. T3 reuse is not a small extension. Current child sessions have a generated ID, a single `parent_session_id` in SQLite, and unbounded JSONL history. Reusing that object across unrelated jobs or different owners will produce bad routing and bad context by default.

4. T1 and T2 are not just "one brain, two speeds" in code. T2 loses `agent.md`, loses shell, and gets `read_file` only. That is a real behavioral boundary in `src/identity.rs` and `src/turn/builders.rs`.

## Answers

1. Startup flow

You need a startup-built session registry, not one global `Config`.

Each always-on session spec should contain:

- stable `session_id`
- tier
- per-session `Config` clone
- per-session turn builder
- per-session provider factory config
- discovery metadata for prompts

At startup:

1. Load `agents.toml`.
2. Expand it into concrete always-on session specs: `silas-t1`, `silas-t2`, `silas-t2-finance`, etc.
3. `INSERT OR IGNORE` those session IDs into SQLite.
4. Spawn one long-lived drain task per session ID.
5. Each task loads that session's JSONL history and then loops on `dequeue_next_message(session_id)`.

Do not give each tier its own long-lived provider instance. Keep the current model of a shared `reqwest::Client` plus per-turn provider construction. Different tiers already need different `model` / `base_url` / `reasoning_effort`, so the right boundary is "provider factory per session spec", not "one provider for the server".

Different tool surfaces are not a problem if you stop pretending there is one ambient config. T1's session spec builds a shell-backed turn. T2's session spec builds a `read_file` turn. Domain T2s are just more T2 session specs with different prompt/context overlays.

The real missing piece is wakeup. Today workers are spawned opportunistically from HTTP/WS. Always-on tiers need persistent drain loops or a registry-level notify channel.

2. `agents.toml` schema

Do not use `[agents.silas.t2.finance]`. `t2` is currently a leaf config, not a namespace.

Do not use `[agents.silas.domains.finance]`. `domains` already means prompt/context packs in the current config loader. Reusing that name for runtime sessions will be confusing and error-prone.

Use a separate runtime-session table. Example:

```toml
[agents.silas]
identity = "silas"

[agents.silas.t1]
model = "gpt-5.4-mini"
reasoning = "medium"

[agents.silas.t2]
model = "gpt-5.4-mini"
reasoning = "low"

[agents.silas.sessions.t1]
tier = "t1"
session_id = "silas-t1"
description = "Fast operator-facing tier"

[agents.silas.sessions.t2]
tier = "t2"
session_id = "silas-t2"
description = "General deep analysis tier"

[agents.silas.sessions.finance]
tier = "t2"
session_id = "silas-t2-finance"
description = "Finance specialist"
selected_domains = ["finance"]
reasoning = "high"
```

A domain T2 does not need new execution capabilities beyond a generic T2. It needs:

- stable `session_id`
- `tier = "t2"`
- human-readable `description`
- prompt/context specialization, probably `selected_domains` or per-session `context_extend`
- optional model/reasoning overrides

Use explicit `session_id`, not `session_name`. In current code `session_name` is CLI-defaulting behavior, not a stable routing identifier.

3. T3 reuse

My recommendation: do not ship reuse in the first version.

Current code creates a fresh child every time in `src/child_session/create.rs`. That is the safe behavior for the current storage model.

If you insist on reuse, T2 needs an explicit field in structured plan spawn steps, not a hidden heuristic. Something like:

- `worker_id`
- `reuse = "never" | "if_exists" | "require"`
- `history_policy = "reset" | "keep"`

Do not decide reuse purely by task kind or skill set. That becomes opaque and nondeterministic.

History should reset by default. Otherwise reused T3 sessions accumulate unrelated job history forever, and `Session::load_today()` will replay all of it.

There is another hard blocker: the current child-session table stores exactly one `parent_session_id`. A reused T3 cannot safely serve multiple owners without redesigning completion routing.

4. Session identity across restarts

Yes, stable IDs are compatible with the current persistence model.

- JSONL history already lives under `sessions/<session_id>/...`.
- SQLite session rows are durable.
- queued messages are durable.
- stale `processing` rows are requeued on restart.

So `silas-t1` can resume after restart with the same JSONL history and same queue row.

What you need to stop doing is generating random IDs for runtime-owned sessions. `src/server/http.rs` and `src/child_session/create.rs` currently do that for ad hoc sessions and spawned children.

One caveat: if you keep the same session ID but materially change its prompt/capabilities on restart, old history still comes back. That is probably fine for T1/T2, but it argues for a config-version note or startup system message.

5. Concurrency model

The good news:

- queue claims are atomic
- same-session work is serialized by a session lock
- different sessions can run concurrently
- the shared store mutex is released before long LLM execution

So T1 and T2 draining simultaneously is viable.

The real concerns are:

- no persistent wakeup mechanism today
- one shared store mutex may become a bottleneck under many always-on sessions
- there is no cycle control if T1 and T2 start bouncing messages forever
- approval-needed work in unattended background workers currently auto-denies

Ordering is only guaranteed within one session queue. Cross-session ordering is causal only if your own protocol enforces it.

6. CLI mode

In an always-on model, CLI should target T1 by default.

I would support:

- default target: `silas-t1`
- explicit target: `--session silas-t2`
- explicit target: `--session silas-t2-finance`

I would not overload the current local CLI session runner and pretend it is the same thing. Today `src/app/session_run.rs` drains a local session directly. In the always-on architecture, CLI should enqueue to a stable session and stream that session's response, ideally over WS so approvals still work.

Keep the current direct local runner only as a compatibility mode.

7. T1 needs to know about T2

This should come from two places:

- a real `enqueue_message` tool
- a runtime-injected capability/session manifest

The manifest should list:

- target session ID
- tier
- description
- domain coverage
- expected use

Do not rely on static identity files alone. Session topology is runtime config, not fixed persona text.

8. T2 needs to know about T3

Today T2-to-T3 is expressed through `plan-json`, and the real structured interface is `PlanStepSpec::Spawn`.

So T2 needs a runtime-injected schema block that tells it:

- plan-json exists
- allowed step kinds are `shell` and `spawn`
- spawn fields
- check structure
- retry semantics

If you add reuse, put reuse fields into the structured schema. Do not leave reuse as prompt prose only.

Also: current plan handoff is wired only through spawned T2 child completion in `src/agent/child_drain.rs`. That has to move into the normal always-on T2 runtime path, or always-on T2s will never start/patch plans.

9. T1 needs to know its own capabilities

For the current code, identity files plus tool definitions were barely enough.

For the always-on design, they are not enough. T1 also needs:

- peer-session manifest
- routing conventions
- explicit note that it can enqueue to named peer sessions

So yes, you need a capability manifest. It should be runtime-generated context, not a static file.

10. Domain awareness

Use the same runtime-injected session manifest. That is the simplest correct answer.

Static prompt text is too brittle. A discovery tool is unnecessary for MVP. The registry already knows which domain T2s exist.

11. Cross-tier capability negotiation

T1 should know about domain T2s.

T1 should not know about the T3 skill routing matrix.

Generic T2 should own T3 selection, because:

- T2 already owns planning semantics
- T3 skills are already snapshotted at spawn time in current code
- pushing T3 routing knowledge up into T1 couples the whole stack unnecessarily

So the rule should be:

- if the task is obviously domain-specific, T1 may choose a domain T2
- otherwise T1 hands off to generic T2
- T2 decides whether to spawn T3 and which skills to load

12. Runtime capability changes

MVP answer: restart-based propagation, no hot reload.

Current config is loaded once. That is acceptable here.

After restart, the next turn for T1/T2 should see the updated runtime manifest automatically because turns are rebuilt fresh. You do not need explicit notification to make it work.

If you want nicer UX, enqueue a system note after startup saying capabilities changed. But that is optional.

13. What breaks in the current codebase

Several design assumptions break immediately:

- `src/config/agents.rs` only supports one active brain.
- `src/config/runtime.rs` stores one `active_agent`.
- `src/turn/tiers.rs` resolves one tier from that one active agent.
- `src/server/state.rs` holds one ambient `Config`.
- `src/server/queue_worker.rs` clones that same config for every drained session.
- `src/app/session_run.rs` assumes one local direct-drain session, not an addressable always-on registry.
- `src/child_session/create.rs` always generates fresh child IDs.
- `src/agent/child_drain.rs` is where T2 plan-json handoff currently lives, so always-on T2 will not behave correctly until that logic is moved.
- there is no agent-facing queue-send tool, so "queue universal transport" is not actually available to the model

The migration path I would use:

1. Add a `SessionRegistry` that expands config into per-session specs.
2. Change server runtime lookup from `state.config` to `state.registry[session_id]`.
3. Add a real `enqueue_message` tool and runtime capability manifest.
4. Move T2 plan-json handling out of child-drain-only code into the normal T2 runtime.
5. Keep T3 ephemeral until the rest is stable.

14. MVP

The smallest proof that this architecture works is:

- stable always-on `silas-t1`
- stable always-on `silas-t2`
- startup-created session rows
- one background drain loop per always-on session
- per-session turn builders so T1 gets shell and T2 gets `read_file`
- runtime-injected peer-session manifest
- CLI defaults to `silas-t1`
- T3 stays exactly as it is today: spawned, ephemeral, non-reused

I would explicitly not ship in MVP:

- domain T2s
- T3 reuse
- hot reload
- shared T3 pools

That MVP proves the important part: stable IDs, restart persistence, concurrent tier-specific tool surfaces, and queue-based inter-tier routing. Everything else can layer on top once the system is an actual registry of session runtimes instead of one global config plus child-session exceptions.

## Bottom line

The architecture you want is viable, but it is not a small tweak to the current code. The codebase is still centered on one resolved runtime config and spawned child sessions as the special case. The correct direction is:

- make always-on sessions first-class runtime objects
- give agents an explicit queue-send tool
- inject runtime capability manifests
- keep T3 ephemeral until parent routing and history reset semantics are redesigned

If you try to do always-on T1/T2 plus reusable T3 in one step, you will mix three migrations together and make the failure modes hard to reason about.
