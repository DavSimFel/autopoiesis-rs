# Operator Vision

Scope: this extracts only David/operator decisions about the tier architecture. Direct operator docs (`ROUND3_OPERATOR.md`, `ROUND4_OPERATOR.md`) are prioritized. Original AP vision docs authored by David and Silas memory entries explicitly marked as David decisions are used where they preserve the decision. Any inference is labeled.

## 1. T1 / T2 / T3 lifecycle

David's stable tier roles are:

> "- **T1** is the fast conversational layer  
> - **T2** does deeper planning, curation, and re-organization  
> - **T3** executes concrete work: connectors, transforms, imports, code, actions"  
> Source: `ap-VISION.md:80-82`

T3 is the only tier David states as disposable in explicit terms:

> "The executor is disposable. The artifact persists."  
> Source: `ap-VISION-SELF-IMPROVEMENT.md:164-168`

The Rust rewrite keeps that as:

> "T3 is ephemeral execution. It receives the skills and model selected for the task, then runs the shell-backed work and exits."  
> Source: `aprs-identity-v2.md:83-85`

For T1 and T2, David does not literally write "persistent" in the operator intervention files, but the settled model clearly treats them as durable session owners:

> "Persistent named sessions with default session (`--session <name>`)."  
> Source: `silas-MEMORY.md:20`

> "Topic trigger → T2 session → T2 emits plan → plan runs steps → T3s execute → results flow back → T2 evaluates → adjusts if needed → done/escalate"  
> Source: `ROUND4_OPERATOR.md:31`

Inference: T1 is the long-lived operator-facing session; T2 is a durable reasoning session that can receive triggers, failures, and replanning messages; T3 is the disposable worker tier.

Python AP nuance:

> "`REUSE` (default): You SHOULD send follow-up work items to the same T3 instance when tasks share context... Spawning is expensive — justify it."  
> Source: `ap-t2-planner.md:96-99`

That means the original Python design allowed T3 reuse inside a related task, even though the higher-level vision still called the executor disposable.

## 2. "One brain, two speeds"

David's direct formulation is:

> "**Silas is the brain.**"  
> Source: `ap-VISION-SELF-IMPROVEMENT.md:34`

> "The tier model remains one mind operating at different speeds:"  
> Source: `ap-VISION-SELF-IMPROVEMENT.md:38`

Silas's memory preserves the settled design in compact form:

> "**One brain, two speeds.** T1/T2 = same identity (System 1/System 2). Multiple T2s = parallel deep thought. T3 = tools/hands."  
> Source: `silas-MEMORY.md:59`

The exact meaning is:

- T1 and T2 are not separate beings. They are the same identity in fast mode and slow mode.
- Multiple T2s are parallel deep-thought threads of the same brain, not separate planners with separate identities.
- T3 is not part of the brain. It is the hands.
- Domain changes do not create new identities:

> "**Domain context = context extension** (domains/*.md), not identity layer. Same brain + different domain = same personality, different knowledge."  
> Source: `silas-MEMORY.md:61`

The Rust rewrite encodes that as:

> "T1 and T2 are the same identity expressed at different speeds. T3 is a task worker spawned for a specific job."  
> Source: `aprs-identity-v2.md:14`

## 3. Session persistence

The settled persistence model is:

- Named sessions persist.

> "Persistent named sessions with default session (`--session <name>`)."  
> Source: `silas-MEMORY.md:20`

> "JSONL stores durable session history."  
> Source: `aprs-vision.md:65`

- T2 sessions are the durable owners for automation, replanning, and failure recovery.

> "Notifications use the message queue (enqueue to T2's session)"  
> Source: `ROUND4_OPERATOR.md:18`

> "Failure notification goes to T2."  
> Source: `ROUND4_OPERATOR.md:13`

> "Plans can trigger from topics. A topic trigger (cron or webhook) can start a plan."  
> Source: `ROUND4_OPERATOR.md:22`

- T3 sessions do not live forever. They are spawned, do the job, and exit.

> "T3 is disposable execution. It is spawned for a specific task, gets the appropriate model and skills, then runs the shell-backed work and exits."  
> Source: `aprs-vision.md:36`

What survives after T3 exits is the durable record, not the worker:

> "`plan_step_attempts` records execution history, check outcomes, and crash state for each step attempt."  
> Source: `aprs-plan-engine.md:66`

Inference: the "live forever" sessions in David's model are the named T1/operator session and any durable T2 owner session. The sessions that die are T3 child sessions; only their outputs, artifacts, and attempt history persist.

## 4. Context management between tiers

The original Python rule was explicit:

> "Create a topic with your plan. Add subscriptions for code sections T3 will need."  
> Source: `ap-t2-planner.md:32`

> "You MUST set up topic subscriptions before delegating so T3 starts with the right context."  
> Source: `ap-t2-planner.md:72`

T3 was supposed to manage its working set explicitly:

> "You SHOULD use `subscribe()` to persist important code sections across turns."  
> Source: `ap-t3-executor.md:70`

Large outputs were intentionally pushed out of inline context and back into the system through subscriptions:

> "**Shell results → files.** All output saved to `sessions/{id}/results/{call_id}.txt`. Below threshold: also inline. Above: metadata only, agent must SUBSCRIBE. Forces subscription pattern."  
> Source: `silas-MEMORY.md:45`

> "**Subscriptions are always to files.** Never HTTP/DB. Shell is the universal adapter. Trigger → shell writes file → subscription sees mtime change → bubbles forward in timeline."  
> Source: `silas-MEMORY.md:47`

Cross-tier knowledge transfer is explicit, not hidden:

> "**T2→T1 handoff via explicit artifacts** through message queue, not shared state."  
> Source: `silas-MEMORY.md:63`

> "T2 writes structured conclusions back to the queue. T1 reads the conclusion as a normal inbound message."  
> Source: `aprs-identity-v2.md:108-109`

Portable context across sessions is topic-backed:

> "**Topics as portable context.** `topic export/import` for cross-session transfer. Scout T3 adds subs → coder T3 loads topic → zero exploration."  
> Source: `silas-MEMORY.md:55`

Skill/context loading is intentionally asymmetric:

> "T1 and T2 see summaries. Spawned T3 workers receive full instructions."  
> Source: `aprs-vision.md:54-55`

Net decision: knowledge moves between tiers through subscriptions, topic packaging, queue artifacts, and durable files. It does not move through a hidden shared mind-state.

## 5. T2 as the plan engine

David's Round 3 direction established the durability requirement:

> "Each step has a state. Pending, running, completed, failed, retrying. Persisted in SQLite. Crash at any point → resume from last completed step."  
> Source: `ROUND3_OPERATOR.md:31-32`

Round 4 then reframed where that durability lives:

This is David's clearest direct correction:

> "The plan engine IS T2. Not a separate layer bolted on — it's how T2 works. T2 is a problem decomposer, reasoner, planner, and orchestrator. Its native output is a plan."  
> Source: `ROUND4_OPERATOR.md:5`

The consequences David immediately attached are:

> "No mandatory operator approval. T2 is the stronger reasoner. It can author and execute plans autonomously... Use those, don't add a new gate."  
> Source: `ROUND4_OPERATOR.md:9`

> "Plans can be tiny. A plan with one step and one T3 is just... spawning a T3 with a task."  
> Source: `ROUND4_OPERATOR.md:11`

> "Failure notification goes to T2. When a plan step fails, T2 gets notified and can intervene..."  
> Source: `ROUND4_OPERATOR.md:13`

> "Don't over-engineer."  
> Source: `ROUND4_OPERATOR.md:26`

His mental model is explicit:

> "Topic trigger → T2 session → T2 emits plan → plan runs steps → T3s execute → results flow back → T2 evaluates → adjusts if needed → done/escalate"  
> Source: `ROUND4_OPERATOR.md:31`

The settled Rust form follows that directly:

> "Plans are T2's structured execution format. T2 emits a fenced `plan-json` block..."  
> Source: `aprs-plan-engine.md:8`

> "**T2 emits plans as `plan-json` fenced blocks** — no new tool, T2 stays read-only, runtime parses from assistant output"  
> Source: `silas-memory-2026-03-25.md:76`

So the decision is not "T2 plus a planner." The decision is that planning is what T2 is.

## 6. Topics, subscriptions, triggers

David's original AP definition of topics was rich and central:

> "topics are the data acquisition unit: source, curator, trigger, surface generator"  
> Source: `ap-VISION.md:26-28`

> "Topics are not passive folders. A topic is a live operating unit with four jobs:"  
> Source: `ap-VISION.md:95`

> "- **source**: where data enters  
> - **curator**: how incoming material becomes claims, metrics, summaries, and links  
> - **trigger**: what changes should produce new signals or work  
> - **surface generator**: which artifacts should exist because this topic exists"  
> Source: `ap-VISION.md:97-101`

T2's place in that loop was:

> "3. **Interpret** - T2 decides what changed in the user's world  
> 4. **Generate** - T2 creates or updates markdown artifacts and surface cards"  
> Source: `ap-VISION.md:158-160`

In the settled APRS operational model, topics became lighter-weight and storage-backed:

> "**Topics = pure SQLite indexes.** No special files, no `topics/` directory. A topic is a name + activation state + subscriptions + triggers + relations."  
> Source: `silas-MEMORY.md:51`

> "**Two trigger types only.** Cron + webhook. No file-watching..."  
> Source: `silas-MEMORY.md:53`

David's operator rule for how triggers feed the tiers is:

> "Plans can trigger from topics. A topic trigger (cron or webhook) can start a plan."  
> Source: `ROUND4_OPERATOR.md:22`

The converged implementation path is:

> "**Topic triggers = normal messages.** Cron/webhook enqueue to T2 session. No new trigger system."  
> Source: `silas-memory-2026-03-25.md:81`

That means the feed path is:

- subscriptions and topic state detect or package change
- cron/webhook triggers enqueue a normal message
- the message lands in a T2 session
- T2 interprets and emits the plan
- T3 executes concrete steps
- results return as files, queue messages, and durable attempt history

## 7. Places David corrected Silas or Codex

1. Misunderstanding: the plan engine was a separate workflow layer.

> "The plan engine IS T2. Not a separate layer bolted on..."  
> Source: `ROUND4_OPERATOR.md:5`

2. Misunderstanding: T2-authored plans needed a brand-new approval gate.

> "No mandatory operator approval... Use those, don't add a new gate."  
> Source: `ROUND4_OPERATOR.md:9`

3. Misunderstanding: failures should go to a generic retry engine.

> "Failure notification goes to T2."  
> Source: `ROUND4_OPERATOR.md:13`

> "The agent IS the error handler."  
> Source: `ROUND4_OPERATOR.md:24`

4. Misunderstanding: topic-driven planning needed a separate trigger subsystem.

> "Plans can trigger from topics. A topic trigger (cron or webhook) can start a plan."  
> Source: `ROUND4_OPERATOR.md:22`

> "Topic triggers = normal messages. Cron/webhook enqueue to T2 session. No new trigger system."  
> Source: `silas-memory-2026-03-25.md:81`

5. Misunderstanding: T1 and T2 were different agents with different personalities.

Correction record:

> "We were wrong about T1/T2 being different agents with different personalities."  
> Source: `BRAIN_MODEL.md:5`

David's corrected model in his own words:

> "**Silas is the brain.**"  
> Source: `ap-VISION-SELF-IMPROVEMENT.md:34`

> "The tier model remains one mind operating at different speeds:"  
> Source: `ap-VISION-SELF-IMPROVEMENT.md:38`

## 8. Contradictions / drift: Python AP vision vs Rust APRS implementation

1. T2's capability boundary changed sharply.

Python AP:

> "You are a senior architect with full filesystem and shell access."  
> Source: `ap-t2-planner.md:12`

Rust APRS:

> "T2 is reasoning-first. It uses `read_file` only, not shell."  
> Source: `aprs-identity-v2.md:81`

Result: APRS makes T2 much stricter than the original Python T2 prompt.

2. T3 moved from reusable worker-in-context to stricter disposable worker.

Python AP:

> "`REUSE` (default): You SHOULD send follow-up work items to the same T3 instance when tasks share context..."  
> Source: `ap-t2-planner.md:96-99`

Rust APRS:

> "T3 is disposable execution... spawned for a specific task... exits."  
> Source: `aprs-vision.md:36`

Result: Python AP allowed persistent T3 context within a task; APRS normalizes T3 into per-job disposable workers.

3. Topics moved from the core operating unit to an optional grouping layer built on subscriptions.

Python AP:

> "topics are the data acquisition unit: source, curator, trigger, surface generator"  
> Source: `ap-VISION.md:26-28`

Rust APRS direction / current build state:

> "**Topics = pure SQLite indexes.** No special files, no `topics/` directory."  
> Source: `silas-MEMORY.md:51`

> "Step 1 of 4 toward real-world usage (subs → topics → triggers → systemd)"  
> Source: `silas-memory-2026-03-26.md:22`

> "Topics — SQLite grouping layer on subscriptions"  
> Source: `silas-memory-2026-03-26.md:56`

> "What Still Remains" includes "Subscriptions v2 and context wiring" and "Topic export/import."  
> Source: `aprs-vision.md:75-79`

Result: the original AP architecture was topic-first; APRS is being built subscription-first, with topics simplified and still incomplete.

4. T2's output shifted from topic/artifact authoring to read-only structured control output.

Python AP:

> "T2 creates or updates markdown artifacts and surface cards"  
> Source: `ap-VISION.md:159`

> "Create a topic with your plan."  
> Source: `ap-t2-planner.md:32`

Rust APRS:

> "**T2 emits plans as `plan-json` fenced blocks** — no new tool, T2 stays read-only..."  
> Source: `silas-memory-2026-03-25.md:76`

> "T2 is the reasoning layer. It uses `read_file` only."  
> Source: `aprs-vision.md:32`

Result: APRS narrows T2 from authoring topic artifacts toward pure planning/orchestration output.

These drifts are not necessarily accidental. In several cases, APRS is following David's later operator corrections rather than the earlier Python prompt architecture.
