# Conversation: Always-On Tier Architecture + Agent Capability Discovery

Design conversation — no code, no PLAN.md. Read the full codebase first.

## The Architecture (binding decisions from operator)

### Session Lifecycle
- T1 and T2 sessions are **always-on**, created at server startup from `agents.toml`
- T1/T2 are NEVER spawned by each other — only configured in `agents.toml`
- T3 is spawned by T2 (plan engine decides), can be ephemeral or reused until session is explicitly deleted
- Domain T2s (e.g. `silas-t2-finance`) are configured explicitly in `agents.toml`

### Session IDs
- Well-known, derived from config (e.g. `silas-t1`, `silas-t2`, `silas-t2-finance`)
- Not random UUIDs — predictable, stable across restarts

### Routing
- By session ID — T1 writes to `silas-t2` queue explicitly by ID
- No content-based routing. The sender picks the target.
- All inter-tier communication = `enqueue_message(target_session_id, role, content, source)`

### Spawning Rules
- T1/T2: NEVER spawned at runtime. Only via `agents.toml`.
- T3: T2 decides the instance — creates new or reuses existing. T3 lives until session explicitly deleted.

### Existing Design Principles (from prior sessions)
- "One brain, two speeds" — T1/T2 are the same identity at different reasoning levels
- "Source-agnostic inbox" — message = message, agent never knows the source
- "Shell is THE universal tool" — one tool, the prompt teaches what to do
- Queue is the universal transport. No special delegation mechanism.
- T3 = ephemeral executors (hands), but sessions can persist for reuse

## Questions for You

Read the full codebase, especially agents.toml, config/, session_runtime/, server/, agent/, plan/. Then:

### Architecture

1. **Startup flow.** Today `server::run()` creates one Store and one set of queue workers. With always-on T1/T2/domain-T2s, what does startup look like? Does each tier get its own drain loop? Its own provider instance? How do you handle the fact that T1 needs shell but T2 needs read_file only — different tool surfaces on concurrent drain loops?

2. **agents.toml schema.** The current schema has `[agents.silas.t1]` and `[agents.silas.t2]`. How should domain T2s be expressed? `[agents.silas.t2.finance]`? Or `[agents.silas.domains.finance]`? What fields does a domain T2 need beyond what a generic T2 has?

3. **T3 reuse.** Today child sessions are created fresh per plan step. If T3 sessions persist and can be reused, how does T2 decide: create new vs reuse existing? By task kind? By skill? By explicit name? What happens to a T3's session history when it's reused — does it accumulate or reset?

4. **Session identity across restarts.** If `silas-t1` is a well-known session ID and the server restarts, does the session resume with its JSONL history? Does the SQLite session row persist? What about in-flight messages in the queue?

5. **Concurrency model.** T1 and T2 drain loops running simultaneously. T1 writes to T2's queue. T2's drain picks it up and runs a turn. T2 writes result back to T1's queue. T1's drain picks it up. Any ordering, contention, or starvation concerns?

6. **CLI mode.** Today `autopoiesis "prompt"` runs a single session. With always-on tiers, does CLI mode target T1 by default? Can you address a specific tier from CLI?

### Agent Capability Discovery

This is the second part of the design question: **How do the agents know what they can do?**

7. **T1 needs to know about T2.** T1 must know: which T2 sessions exist, what domains they cover, how to address them. Where does this come from? System prompt? A tool definition? A runtime-injected context block?

8. **T2 needs to know about T3.** T2 must know: it can emit plan-json blocks, what step types exist (shell, spawn), what skills are available for T3, how to request T3 reuse vs creation. Is this all in the prompt, or does the tool surface communicate it?

9. **T1 needs to know its own capabilities.** T1 has shell, skills, delegation ability. How is this communicated? Today it's identity files + tool definitions. Is that sufficient, or does the always-on model need something more (e.g. a capability manifest)?

10. **Domain awareness.** If T1 needs to delegate a finance question to `silas-t2-finance`, it needs to know that domain T2 exists and what it covers. Static config in the prompt? Dynamic discovery via a tool? An injected context block listing available T2 domains?

11. **Cross-tier capability negotiation.** T1 wants to delegate a task that needs code review skills. Should T1 know which T2/T3 has those skills, or should it just send the task to a generic T2 and let T2 figure out the skill routing?

12. **Runtime capability changes.** If a new domain T2 is added to agents.toml and the server restarts, how do existing T1/T2 sessions learn about it? Hot reload? Restart clears context? Explicit notification?

### Risks

13. **What breaks in the current codebase?** The existing server creates one ambient runtime config. Always-on tiers need per-session configs. What's the migration path?

14. **What's the MVP?** If you could ship one thing to prove the always-on tier model works, what would it be?

Write your response to CONVO_RESPONSE.md. Be direct. Disagree with any part of the model if you think it's wrong.
