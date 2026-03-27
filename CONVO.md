# Conversation: Inter-Tier Message Routing via Queue

You are being asked to think through a design for inter-tier communication in autopoiesis-rs. This is a **design conversation** — no code, no PLAN.md, no implementation. Just your honest analysis.

## Context

autopoiesis-rs has a tiered agent runtime:
- **T1** = System 1 (fast, conversational, shell + skills)
- **T2** = System 2 (same brain going deep, read_file only, emits plans)
- **T3** = ephemeral executor (shell + skill instructions, spawned per plan step, dies when done)

Key design principle from the operator (David):
- **"One brain, two speeds."** T1/T2 are the same identity at different reasoning levels. T3 = hands.
- **Source-agnostic inbox.** Message = message. Cron, webhook, user, agent — all feed SQLite queue. Agent never knows the source.

## The Proposal

**Each tier can write into any other tier's message queue.** No special delegation mechanism, no spawn tool, no new transport. Just `enqueue_message(target_session_id, role, content, source)`.

This means:
- T1 can delegate to T2 by writing a task into T2's queue
- T2 can ask T1 a clarifying question by writing back to T1's queue
- T2 can steer a running T3 by writing into T3's queue
- T3 completion already propagates back to T2 via completion.rs — same pattern

The agent sees a message, responds, and the **runtime** routes the response back to wherever it came from.

### Reply routing

A `reply_to_session` field on the queued message tells the runtime where to send the assistant's response back. The agent doesn't parse metadata or know about routing — the infrastructure handles it.

Current messages table:
```sql
session_id TEXT,      -- which queue this is IN
role TEXT,            -- user/system/assistant
content TEXT,
source TEXT,          -- who sent it ("agent:t1:session-abc", "user", "webhook:...")
```

Proposed addition:
```sql
reply_to_session TEXT  -- where to send the answer back (nullable)
```

The drain loop, after generating an assistant response to a message with `reply_to_session` set, enqueues the response into that target session's queue.

### Source metadata

The existing planned `<meta>` tag on messages carries principal identity for taint tracking:
```xml
<meta ts="..." principal="agent:t1:session-abc" reply_to="session-abc" />
```

## Questions for You

Read the full codebase first. Then:

1. **Does this compose with existing infrastructure?** The queue, drain loop, completion propagation, session store — where does this proposal align and where does it conflict?

2. **What about the plan engine?** T2→T3 today goes through plan_runner spawning child sessions. If T2 can just write to T3's queue directly, does the plan engine become redundant? Or do they serve different purposes?

3. **Session lifecycle.** Who creates the T2 session? Does it pre-exist (named, persistent), or does T1 spawn it on demand? Same question for T3 outside the plan engine.

4. **Circular routing.** T1 writes to T2, T2 writes back to T1, T1 writes to T2 again. What prevents infinite loops? Is this a real risk or does the budget guard handle it naturally?

5. **Drain concurrency.** If T1 and T2 are both running drain loops, and T1 writes to T2's queue while T2 writes to T1's queue simultaneously — any ordering or deadlock concerns with the current SQLite-backed claim model?

6. **Guard implications.** Messages from agents (principal=agent:*) are taint sources today. If T1 writes to T2, does T2's session become tainted? Should inter-tier messages be treated differently from external user messages for taint purposes?

7. **How does this interact with the delegation hint system?** Does `delegation.rs` become obsolete, or does it serve a different purpose (suggesting the mode switch vs actually performing it)?

8. **Observability.** The new trace event schema has turn_id and session_id. How should cross-session message routing appear in traces? A new event type? An attribute on the existing queue events?

9. **What's the minimum viable implementation?** If you could only ship one thing to prove the pattern works, what would it be?

10. **What are the risks or gotchas you'd flag?**

Write your response to CONVO_RESPONSE.md. Be direct. Disagree with the proposal if you think parts of it are wrong.
