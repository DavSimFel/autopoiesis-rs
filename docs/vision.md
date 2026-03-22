# Vision — Future State

> **This is aspirational.** For current state, see [current/architecture.md](current/architecture.md).
> For what's broken right now, see [current/risks.md](current/risks.md).
> For build order, see [roadmap.md](roadmap.md).

## Core ideas

**One inbox, any source.** Cron, webhook, user, agent — all feed the same SQLite queue. The agent loop reads the next message, thinks, acts, responds. Never knows or cares where the message came from.

**Shell is the universal tool.** File I/O, web requests, process management, agent-to-agent calls, self-configuration — all through shell. The prompt teaches the agent what to do. The tool surface stays at one.

**Shell output is capped. Full results live in files.** Every shell execution saves full output to `sessions/{id}/results/{call_id}.txt`. Output below threshold is also inline in history. Output above threshold: only metadata in history. To read the content, the agent **subscribes**. This is the forcing mechanism.

**Subscriptions are explicit context management.** A subscription injects file content into the context pipeline. Subscriptions are standalone — optionally grouped by topic. The agent subscribes via CLI (`./autopoiesis sub add <path>`). Subscriptions are:
- **Instant** — content appears on the very next turn
- **Optional** — the agent decides what to load
- **Positional** — placed in history by `max(activated, updated)` timestamp
- **Reactive** — file changes → new timestamp → bubbles forward in timeline

**Subscriptions are always to files.** Never to HTTP endpoints, databases, or external sources directly. Shell is the universal adapter. Trigger → shell writes file → subscription sees mtime change → content loads. No HTTP in the subscription layer.

**Subscription filtering.** Not every subscription loads the full file:
- **Full** — entire content (default)
- **Lines** — range filter (`--lines 174-230`)
- **Regex** — pattern extraction (`--regex "pub fn"`)
- **Head/Tail** — first/last N lines
- **JQ** — JSON query (`--jq ".status"`)

No hard budget limit. CLI reports utilization on every change. Agent manages its own context. If it overflows, history trimming drops oldest turns. Hard limits are for safety (guards); context management is for intelligence (the agent's job).

**Topics are optional indexes.** A topic is a name + activation state + subscriptions + triggers + relations. All in SQLite, all managed through CLI. When topics aren't needed, subscriptions work standalone (implicitly in `_default` topic).

**CLI is the self-management interface.** The agent manages itself through its own binary via shell. Files for storage, CLI for validated control plane. Every management action is a shell call in history, auditable by guards.

**Safety is multi-dimensional.** Four gate dimensions work together:
- **Permissions** — what the agent CAN touch (filesystem, network, resources)
- **Approval** — human-in-the-loop escalation for risky actions
- **Taint** — tracks provenance of untrusted input; tainted commands escalate even if they match standing approvals
- **Budget** — cost/resource ceilings per turn, session, day

Standing approvals + taint tracking together enable practical safe autonomy. Without taint, standing approvals are exploitable via injection. Without standing approvals, taint makes everything require manual approval.

**Identity is layered.**
1. `constitution.md` — Laws of thought. Immutable. Only operator modifies through direct file access.
2. `operator.md` — Operator policy. Purpose, boundaries, permissions. Agent cannot modify.
3. `identity.md` — Agent persona. Self-modifiable within operator bounds.
4. `context.md` — Operational steering. Working memory, focus, reminders.

Write-protection enforced by guard pipeline (ShellSafety blocks writes to constitution.md + operator.md).

**Messages carry metadata.** `<meta ts="..." principal="operator|user|agent:id|system" />` on every user message. Makes Chain of Command enforceable per-message.

**Agent-to-agent = message.** T1 spawns T3 by posting a message to a new session. One agent can subscribe files for another session — that's delegation with context.

**SQLite is the backbone.** Session state, message queue, subscription records — one database file. ACID, concurrent-safe, crash-recoverable.

## Topics at scale

200 topics, one context window. Agent loads 1-3 at a time. Everything else on disk, indexed, resumable.

Topic as portable context: `topic export/import` bundles metadata for cross-session transfer. Scout T3 adds subs → coder T3 loads topic → zero exploration.

Triggers: cron + webhook only. No file-watching (cron + stat does the same job).

## CLI surface

```bash
# Subscriptions (standalone or topic-scoped)
./autopoiesis sub add <path> [--topic <name>] [--lines 10-50] [--regex "pub fn"]
./autopoiesis sub remove <path> [--topic <name>]
./autopoiesis sub list [--topic <name>]

# Topics (optional grouping)
./autopoiesis topic create/delete/activate/deactivate/list/export/import <name>

# Triggers
./autopoiesis trigger add --topic <name> --type cron --schedule "*/5 * * * *"
./autopoiesis trigger add --topic <name> --type webhook --path "/hooks/deploy"

# Identity (self-modification within operator bounds)
./autopoiesis identity get/set <dimension> <value>

# Messaging
./autopoiesis msg send --session <id> "content"

# Sessions
./autopoiesis session list
```

## Skills

Two-tiered: shipped and custom.

*Shipped skills* are core cognitive capabilities: web search, deep research, coding, planning. Thought patterns injected into context, not runtime plugins.

*Custom skills* are built by the agent. When it needs a new integration, it reads docs, generates the connector, tests, deploys — all through shell. Autopoiesis ships zero vendor connectors.

## Open questions

**Instruction positioning.** Three options:
- A: All identity at top (before history) — maximizes KV cache stability
- B: All identity at bottom — matches training, recency bias helps
- C: Split — constitution + operator at top, identity + context at bottom

Hypothesis: C wins. Untested.

**Persona dimensions.** Whether structured traits (`verbosity: terse`, `autonomy: high`) actually change model behavior needs empirical testing.

## Principles

1. **One tool.** If you're adding a tool, you're probably wrong.
2. **One queue.** All messages enter the same way.
3. **Agent controls its context.** No opaque truncation.
4. **Topics are indexes, not containers.**
5. **Subscriptions are instant and optional.** Cap forces the pattern; subscriptions serve it.
6. **Files for storage, CLI for control plane.**
7. **Small surface.** Every line of code is a liability.
8. **Crash and resume.** SQLite queue means nothing is lost.
9. **Composition over accretion.** Richer behavior from primitives, not features.
10. **SSE over WebSocket for transport.** Stateless, resumable, mobile-friendly.
