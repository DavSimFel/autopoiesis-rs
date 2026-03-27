# Design Analysis: Inter-Tier Message Routing via Queue

## Bottom line

Using the shared SQLite queue as the transport is the right instinct. It already sits in the middle of CLI/HTTP/WS, child completion already propagates by enqueueing into the parent session, and plan failure handoff already does the same. So the queue fits.

What does **not** fit is the claim that this is therefore "just messaging" with no special delegation mechanism. The proposal papers over real protocol decisions:

- role semantics are underspecified
- provenance and taint are underspecified
- `reply_to_session` is not enough to express reply routing safely
- T2/T3 execution in the current codebase is a state machine, not just a mailbox

The short version is:

- **Queue as substrate:** yes
- **Queue as the whole design:** no
- **No new transport:** yes
- **No explicit orchestration protocol above the transport:** no

One blunt way to say it: **transport can be source-agnostic; policy cannot**.

## 1. Does this compose with existing infrastructure?

Partially, and the parts that compose are real:

- The runtime already uses one SQLite queue for CLI, HTTP, and WS inputs.
- Queue claiming is atomic in `src/store/message_queue.rs`.
- Cross-session enqueue already exists in two important places:
  - `src/child_session/completion.rs` enqueues child completion into the parent session.
  - `src/plan/notify.rs` enqueues plan failure notifications back to the owner T2 session.
- Session parentage already exists in `sessions.parent_session_id`.

So at the transport level, the proposal aligns with the grain of the system.

Where it conflicts is the protocol layer:

- The queue row schema is too small. Today a queued message has `session_id`, `role`, `content`, `source`, `status`, `created_at`. That is enough for ingress, not enough for routed conversation.
- The drain loop in `src/session_runtime/drain.rs` only treats `role = "user"` as an agent turn. `system` and `assistant` are just appended to history. Everything else is unsupported.
- There is no generic post-turn routing hook. The current drain loop only knows how to:
  - mark the row processed or failed
  - optionally emit child completion through a special-case callback
- The current `source` field is only used to derive a coarse `Principal`. That is not enough provenance for routed inter-tier traffic.
- Ordinary server queue workers build turns from global config plus subscriptions. They do not load per-session tier/model metadata. Spawned children are special because `finish_spawned_child_drain_observed()` builds a child-specific runtime config from stored metadata first.

So: the queue fits, but the current runtime does not yet have a generic routed-message protocol sitting on top of it.

## 2. What about the plan engine?

The plan engine does **not** become redundant.

This is the part of the proposal I disagree with most strongly if it is taken literally.

The current T2->T3 path is not just "send a message to another session." The plan engine owns:

- durable `plan_runs`
- durable `plan_step_attempts`
- revisioned plan patching
- step status transitions (`pending`, `running`, `waiting_t2`, `completed`, `failed`)
- postcondition checks
- retry accounting
- crash recovery
- plan-specific observability events
- atomic coupling between state transitions and notifications

That logic lives across `src/plan/runner.rs`, `src/plan/notify.rs`, `src/plan/patch.rs`, `src/plan/recovery.rs`, and `src/store/plan_runs.rs`.

If T2 can write directly to a T3 queue, that may be a valid **transport** for some future executor protocol, but it does not replace the plan state machine. If you remove the plan engine and keep only message passing, you lose the exact things that make plan execution reliable.

Also, current T3 execution is not an independently running conversational worker. Spawned child drain is run synchronously through `finish_spawned_child_drain_observed()`. That means "T2 can steer a running T3 by writing into T3's queue" is not true today. There is no long-lived interactive T3 control loop here. There is a spawned child that drains to completion inside the plan runner's control flow.

So the right framing is:

- queue transport can underpin plan execution
- plan execution still needs a state machine above the queue
- "T2 can steer a running T3" is a new executor model, not a small extension of the current one

## 3. Session lifecycle

With the code as it exists now, the natural answer is:

- **T2 should be created on demand**
- **T3 should be ephemeral and explicitly spawned**

Why:

- `create_child_session_with_task()` already creates a child session and seeds its first queued task atomically.
- Spawned children already store tier/model/reasoning/skills metadata in the session record.
- `Config::with_spawned_child_runtime()` exists precisely to derive the right runtime for a spawned child.

What does **not** exist is a generic way for normal queue workers to look at an arbitrary session id and infer "this is a T2 session with model X and reasoning Y." Normal server drains just use the ambient runtime config.

That means a persistent named T2 mailbox is possible in theory, but it is not a natural fit with the current implementation. You would need durable per-session runtime identity, tier, and model resolution in the ordinary drain path, not just in the spawned-child path.

For MVP, I would not invent a persistent free-floating T2 mailbox. I would:

- explicitly spawn a T2 child session from T1
- route one reply back to the parent
- stop there

For T3, I would be stricter:

- outside the plan engine, T3 should still be explicitly spawned
- it should still have a parent/owner link
- it should still be considered ephemeral

A generic persistent T3 mailbox is the wrong shape for the current architecture.

## 4. Circular routing

This is a real risk. Budget guards are not a real solution.

Reasons:

- budgets may be absent
- even when present, they only stop the loop after wasted tokens
- agent-authored messages are currently non-tainting, so a loop can end up with looser guard behavior than the original user-driven chain

If you add `reply_to_session` and then blindly auto-route assistant replies, you can absolutely build T1 <-> T2 ping-pong loops.

You need explicit controls:

- hop count / TTL
- one-shot reply semantics
- clear routing metadata on the delivered reply so it does not auto-bounce
- maybe loop detection on `(origin_message_id, source_session_id, target_session_id)`
- maybe a hard ban on auto-routing replies to already-routed replies unless a new explicit request is created

So yes, this is a real risk. The queue and budget guard do not solve it by themselves.

## 5. Drain concurrency

At the SQLite claim level, this is mostly fine.

- Message claim is atomic.
- Same-session server drains are serialized by the session lock in `src/server/session_lock.rs`.
- Cross-session writes should not deadlock at the session-lock level because the code does not hold two session locks at once.

What you do get is:

- no total ordering across sessions
- potential worker storms if routing triggers repeated background queue worker spawns
- causal races when multiple routed messages target the same session at nearly the same time

That is acceptable if the model is "eventual delivery, per-session FIFO, no cross-session ordering guarantee."

What is **not** acceptable is pretending this gives you live interactive steering of a running T3. Current T3 drain is synchronous inside the runner. There is no concurrency model for "inject a control message into the worker mid-step."

So my answer is:

- no obvious SQLite deadlock problem
- yes to ordering caveats
- yes to background-worker churn risk
- no, this does not currently enable interactive control of in-flight T3 execution

## 6. Guard implications

This is the biggest protocol hole.

Today:

- `Principal::from_source()` maps any `agent-*` source to `Principal::Agent`
- `Principal::Agent` is explicitly **not** a taint source
- turn taint is derived from message principals in history

That means inter-tier forwarding currently launders provenance unless you explicitly preserve it.

Worse: spawned children already demonstrate the problem. `create_child_session_with_task()` seeds the child with `source = format!("agent-{}", parent_session_id)`. When that queued row is drained, the child's inbound principal becomes `Agent`, not `User` or `System`. So the forwarded task is not tainting by principal, even if it was caused by untrusted user input upstream.

That may have been tolerable while child spawning was a narrow internal mechanism. If you generalize "any tier can message any other tier", it becomes a real protocol bug.

The runtime needs at least two provenance concepts:

- **immediate sender principal**: who directly enqueued this row
- **causal principal**: what trust level caused this work in the first place

Guard and taint decisions should use the causal principal conservatively.

My view:

- T1->T2 on behalf of a user should taint T2
- T2->T3 on behalf of user/system-derived work should taint T3
- purely internal operator-originated orchestration can remain untainted

So no, inter-tier messages should not be treated like ordinary safe internal assistant chatter. Not unless you want inter-tier routing to become a taint bypass.

## 7. How does this interact with the delegation hint system?

`delegation.rs` is not obsolete just because the queue can carry the handoff.

Right now it does one thing: it tells T1 "this looks like a case for deeper analysis." That is a policy hint, not a transport.

That can remain useful in either of these worlds:

- the model decides when to hand off and uses a routing primitive
- the runtime auto-escalates under certain thresholds and the hint becomes less important

The part that might become obsolete is not the idea of delegation advice. It is the current wording and wiring if you move from "suggest T2" to an explicit runtime handoff primitive.

So I would say:

- the transport proposal does not obsolete delegation advice
- it only obsoletes it if you intentionally replace model-side delegation with hard runtime policy

## 8. Observability

This needs a new event type. An attribute on existing events is not enough.

Why:

- routing happens outside the target turn
- routing can fail before any target turn starts
- you need cross-session causality, not just more fields on `TurnStarted`

I would add something like `MessageRouted` with fields such as:

- source session id
- source message id
- source turn id
- target session id
- target message id
- role
- immediate sender principal
- causal principal
- reply target session id
- reply target message id
- hop count
- route kind (`delegation`, `completion`, `plan_failure`, `clarification`, `escalation`)

Then, if possible, I would also thread causal ids into `TurnStarted` for the target turn.

I would **not** collapse plan observability into generic routing events. The existing plan events are correct because plan progression is a first-class runtime state machine.

## 9. Minimum viable implementation

If I only got one shot to prove the pattern, I would ship this and nothing more:

1. T1 can explicitly spawn an on-demand T2 child session.
2. The child task is delivered through the existing queue.
3. The child session can send exactly one assistant reply back to the parent through routing metadata.
4. The delivered reply clears routing metadata so it does not auto-bounce.
5. T3 and the plan engine stay exactly as they are.

For the message envelope, I would add more than just `reply_to_session`. Minimum useful metadata is:

- `origin_session_id`
- `origin_message_id`
- `caused_by_turn_id`
- `causal_principal`
- `reply_to_session_id`
- `reply_to_message_id`
- `hop_count`

If you want an even tighter MVP, make it narrower still:

- re-express the current child-completion pattern as a structured routed reply for T2 child sessions only
- do not expose arbitrary session-to-session messaging yet

That would prove the transport idea without pretending the whole protocol is solved.

## 10. Risks and gotchas

The main ones I would flag are:

- **Taint laundering.** This is the biggest one. The current `source -> Principal` mapping is too coarse.
- **Role laundering.** In practice everything important will get jammed into `role = "user"` because only user rows invoke the agent loop. That is a sign you need a better message kind/protocol layer.
- **`reply_to_session` is not enough.** You also need correlation ids and one-shot semantics.
- **Plan/T3 is not just messaging.** Current plan execution relies on durable state transitions, not just delivery.
- **"Steer a running T3" is false today.** Current spawned T3 execution is not an interactive worker.
- **Persistent T2/T3 sessions need runtime resolution.** Normal queue drains do not currently derive tier/model from session metadata.
- **Session queue is not the same thing as user delivery.** Putting a clarifying question into T1's queue is not the same thing as guaranteeing a human sees it in the right transport/UI.
- **Atomicity matters.** Some flows must couple state mutation and notification in one transaction. Plan failure already does this. Generic routing will need the same discipline in some cases.
- **Loop amplification and worker churn.** Reciprocal routing can create useless work quickly.

## Final take

I agree with the core instinct: the queue is the right common transport.

I do **not** agree that this means "no special delegation mechanism" in the broader sense. It means "no new transport." You still need an explicit protocol above the transport for:

- message intent / role semantics
- provenance and taint inheritance
- reply correlation
- lifecycle and ownership of spawned sessions
- plan-state transitions

So my recommendation is:

- unify on the queue as the substrate
- keep explicit spawn/orchestration semantics
- solve provenance before broadening inter-tier messaging
- prove the pattern first with T1 <-> spawned T2 request/reply
- do not flatten plan/T3 execution into generic queue traffic until you design the executor state machine explicitly
