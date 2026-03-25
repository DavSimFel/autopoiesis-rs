# Vision

> For current state, see [architecture/overview.md](architecture/overview.md).
> For what's broken, see [risks.md](risks.md).
> For build order, see [roadmap.md](roadmap.md).

## What this is

A personal agent runtime. One brain runs your operational life through shell — businesses, projects, communications, decisions — restricted only by an approval system. You direct strategy. The agent executes.

Not a framework (you don't import it). Not a platform (there's no multi-tenant). Not a chatbot (it acts, not just talks). Not an IDE plugin (it's the runtime, not an extension).

## Three bets

### 1. One tool: shell

Shell is the only tool. The prompt teaches the agent what to do with it. Zero tool surface to secure, zero integrations to maintain. Same capability as any human at a terminal. PTY extends this to persistent interactive sessions — SSH, REPLs, monitoring.

Skills provide reusable knowledge (API docs, auth patterns, example calls) that make shell effective for specific domains. The agent learns skills, doesn't import libraries.

### 2. Agent-controlled context

The agent decides what's in its context window. Subscriptions are explicit — the agent subscribes to files, applies filters, manages token budgets. Output above threshold goes to disk; the agent reads what it needs. Nothing is silently truncated.

The runtime enforces policy (token ceilings, trimming, taint tracking). The agent proposes what to load. Hybrid control.

### 3. Autonomy through trust

Four gates work together:
- **Approval** — human-in-the-loop escalation for risky actions
- **Taint** — tracks provenance of untrusted input; tainted commands escalate
- **Budget** — cost/resource ceilings per turn, session, day
- **Permissions** — what the agent can touch (filesystem, network, resources)

Standing approvals let the agent act freely on proven-safe patterns. Taint tracking prevents injection from exploiting those approvals. The operator starts with full oversight and loosens gates as trust is earned.

**Known gap:** These gates reduce blast radius but don't prevent semantic mistakes — the agent doing the wrong thing inside permitted scope. The honest answer: semantic safety comes from the model getting smarter, not the runtime getting more complex. We make guardrails as good as possible and accept that model judgment is the last line of defense.

## One brain, two speeds, ephemeral hands

T1 and T2 are the same identity at different clock speeds. T3 is disposable execution.

### T1 — fast thinking (System 1)

Low-latency model. Personality (agent.md). Skills. Shell access. Talks to the operator.

Below a complexity/risk threshold, T1 acts directly. Above threshold, T1 delegates to T2 immediately. T1 doesn't try to be smart about hard things.

### T2 — slow thinking (System 2)

Reasoning model. No personality. No shell. Reads local state through a structured read API with provenance tags. Explores and assigns skills.

T2 plans, analyzes, and decomposes. **T2 never acts externally.** It always spawns T3s for actions and external exploration. T2 is a manager who never opens their own laptop.

Multiple T2 instances run as parallel deep-thought threads. They're isolated by default — no shared scratchpad, no groupthink. Domain-specific T2s spin up when a domain has enough load (gym operations, real estate, coding).

**Security model:** T2 has no shell, so it can't be injected into executing arbitrary commands. Its attack surface is tainted subscription content and tainted T3 results — both carry provenance tags. The constitution + guard pipeline escalate when T2 plans based on tainted input.

### T3 — the hands

Task-appropriate model chosen from catalog. Skills fully loaded by T2. Shell access. No personality, no persistence.

Spawned for a specific task, executes, reports back, dies. T3 is the only tier that touches external systems. Each T3 gets exactly the skills and permissions it needs, nothing more.

### Handoff

T2 → T1: structured conclusion written to message queue (decision, evidence, confidence, risks, next action). T1 reads it as a normal inbound message and communicates the result.

T1 → T2: delegation message with task description and domain context.

T2 → T3: spawn with skills loaded, task prompt, model from catalog.

## Identity

Three files. Constitution and context always load. agent.md loads for T1 only.

| Layer | File | Owner | Loads for |
|-------|------|-------|-----------|
| **Policy** | `constitution.md` | Operator (immutable) | All tiers |
| **Character** | `agent.md` | Operator (refreshed on start) | T1 only |
| **Context** | `context.md` | System (rendered per session) | All tiers |

Character is T1-only because T2 doesn't communicate with anyone and T3 is ephemeral. The model is the mode switch — a fast model with the same agent.md naturally produces concise conversation; a reasoning model naturally produces deep analysis. Same identity, different gear.

Domain knowledge lives in domain packs (`identity-templates/domains/*.md`) loaded as context extensions. Packs are declared under `[domains.*]` and explicitly selected via `[domains] selected=[...]`. Same brain + different domain = same personality, different working knowledge.

Full spec: [specs/identity-v2.md](specs/identity-v2.md).

## Skills

Two tiers: shipped and custom.

**Shipped skills** are core cognitive capabilities: web search, deep research, coding, planning. Thought patterns in context, not runtime plugins.

**Custom skills** are self-built integrations. The agent reads API docs, writes the connector, tests it, deploys it — all through shell. A Notion skill is: API docs subscribed, auth pattern in standing approvals, example calls in context. Not a library import.

**Skill loading by tier:**
- **T1/T2** gradually explore skills — browse descriptions, understand capabilities
- **T3** gets skills fully loaded by T2 — arrives with everything it needs, zero exploration

Skills compose from existing primitives: subscriptions + topic context + standing approvals. Not a new abstraction.

## Model repository

T3 models chosen from a catalog in `agents.toml`:

- **Catalog** — available models with provider, capabilities, cost, context window
- **Routes** — task kinds listed in `requires`, then mapped to preference order
- **Default** — fallback when no route matches
- **Selection** — fail-closed: explicit override → route match → default → reject

Budget checked before spawn. Never silently exceeds. One model per T3 lifetime.

## Context management

**Source-agnostic inbox.** Cron, webhook, user, agent — all feed SQLite queue. The agent never knows the transport.

**Subscriptions** inject file content into context. Filters: full, lines, regex, head/tail, jq. Positional by timestamp. Reactive to file changes.

**Shell output cap.** Full output saved to file. Below threshold: also inline. Above: metadata pointer only. Agent reads what it needs.

**Topics** are optional grouping on subscriptions. Indexes, not containers.

## Memory

File-based. Journal (append-only daily), summaries (agent-curated), topic files (working notes), workspace files (code, configs, data).

**Provenance required.** Every entry records source. Memory without provenance is a poisoning vector. Taint inherited from untrusted sources.

Separate system from identity. Not in v1.

## Surfaces

CLI-first. All surfaces talk to the same server/queue.

- **Built:** CLI + HTTP/WS server
- **Next:** Web GUI (lightweight SSE client)
- **Later:** Android app, desktop clients

No surface-specific logic in the runtime.

## Principles

1. **One tool.** If you're adding a tool, you're probably wrong.
2. **One queue.** All messages enter the same way.
3. **Agent controls its context.** No opaque truncation.
4. **Files for storage, CLI for control plane.**
5. **Small surface.** Every line of code is a liability.
6. **Crash and resume.** SQLite queue means nothing is lost.
7. **Composition over accretion.** Richer behavior from primitives, not features.
