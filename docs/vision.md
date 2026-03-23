# Vision — Future State

> **This is aspirational.** For current state, see [current/architecture.md](current/architecture.md).
> For what's broken right now, see [current/risks.md](current/risks.md).
> For build order, see [roadmap.md](roadmap.md).

## Core ideas

**MVP: One inbox, any source.** Cron, webhook, user, agent — all feed the same SQLite queue. The agent loop reads the next message, thinks, acts, responds. Transport is source-agnostic — the queue doesn't care how the message arrived.

**MVP: Authority is source-aware (built).** Every message carries a `principal` (operator/user/agent/system) and taint status. The agent doesn't know the transport; it DOES know the trust level. Implemented via `Principal` enum + `GuardContext.tainted`.

**MVP: Shell is the universal tool.** File I/O, web requests, process management, agent-to-agent calls, self-configuration — all through shell. The prompt teaches the agent what to do. The tool surface stays at one.

**V1: PTY shell is the big brother.** The shell evolves from batch `sh -lc` to a full PTY. This unlocks persistent interactive sessions — SSH connections that stay alive across turns, REPLs, long-running processes the agent monitors and interacts with. The agent can maintain persistent remote SSH connections, run `top` and read the output, interact with database CLIs, or drive any interactive terminal program. Same one tool, dramatically more capable.

**MVP: Shell output is capped. Full results live in files.** Every shell execution saves full output to `sessions/{id}/results/{call_id}.txt`. Output below threshold is also inline in history. Output above threshold: only metadata in history. To read the content, the agent **subscribes**. This is the forcing mechanism.

**V1: Subscriptions are explicit context management.** A subscription injects file content into the context pipeline. The agent subscribes via CLI (`./autopoiesis sub add <path>`). Subscriptions are:
- **Instant** — content appears on the very next turn
- **Optional** — the agent decides what to load
- **Positional** — placed in history by `max(activated, updated)` timestamp
- **Reactive** — file changes → new timestamp → bubbles forward in timeline

**V1: Subscriptions are optionally grouped by topic.** Topics are the index layer, not the source of truth. When they matter, subscriptions are grouped around them; when they do not, subscriptions work standalone in `_default`.

**V1: Subscriptions are always to files.** Never to HTTP endpoints, databases, or external sources directly. Shell is the universal adapter. Trigger → shell writes file → subscription sees mtime change → content loads. No HTTP in the subscription layer.

**V1: Subscription filtering.** Not every subscription loads the full file:
- **Full** — entire content (default)
- **Lines** — range filter (`--lines 174-230`)
- **Regex** — pattern extraction (`--regex "pub fn"`)
- **Head/Tail** — first/last N lines
- **JQ** — JSON query (`--jq ".status"`)

**V1: Two budget layers.** The safety layer enforces ceilings: per-turn token limit, per-session limit, per-day cost cap. These are guard-enforced via `BudgetGuard` (built). **Note:** current implementation is preflight-only — it checks before each turn, but the active turn can exceed the ceiling. True hard ceilings require streaming abort or provider-side output caps (not yet built). The intelligence layer is the agent's own context management: CLI reports utilization on every subscription change, the agent decides what to load/unload, and history trimming drops oldest turns when approaching the ceiling. Budget prevents runaway; the agent optimizes within the limits. Trimming preserves assistant/tool round-trips — never splits a tool call from its result.

**V1: Topics are optional indexes.** A topic is a name + activation state + subscriptions + triggers + relations. All in SQLite, all managed through CLI. When topics aren't needed, subscriptions work standalone (implicitly in `_default` topic).

**V1: CLI is the self-management interface.** The agent manages itself through its own binary via shell. Files for storage, CLI for validated control plane. Every management action is a shell call in history, auditable by guards. Note: taint tracking is now built, but context management through shell remains a privilege escalation surface — taint forces approval but doesn't block the command. See [risks.md](current/risks.md).

**MVP: P0 fixes + standing approvals (done/doing).** The four gate dimensions work together:
- **V2: Permissions** — what the agent CAN touch (filesystem, network, resources)
- **MVP: Approval** — human-in-the-loop escalation for risky actions
- **V1: Taint** — tracks provenance of untrusted input; tainted commands escalate even if they match standing approvals
- **V1: Budget** — cost/resource ceilings per turn, session, day

**V1: Standing approvals + taint tracking together enable practical safe autonomy.** Without taint, standing approvals are exploitable via injection. Without standing approvals, taint makes everything require manual approval.

**MVP: Identity is layered.**
1. `constitution.md` — Laws of thought. Immutable. Only operator modifies through direct file access.
2. `identity.md` — Agent persona. Self-modifiable within operator bounds.
3. `context.md` — Operational steering. Working memory, focus, reminders.

**V1: Add operator.md + guard rules.** Write-protection will be enforced by guard pipeline (ShellSafety blocking writes to constitution.md + operator.md). **Not yet built** — currently the agent can write to any file the process user can access. This depends on the security stack (roadmap items 1a-1c).

**V2: Persona dimensions + self-modification.** The longer-term identity model adds structured traits the agent can tune with evidence.

**MVP: Messages carry metadata (partially built).** Timestamp is prepended to user text as `[YYYY-MM-DD HH:MM:SS UTC]`. Principal is stored as a structured field in JSONL/ChatMessage (not inline XML). **Not yet built:** inline `<meta />` tags for provider-visible principal attribution. Current approach: structured persistence + guard pipeline reads `message.principal`.

**V1: Agent-to-agent = message.** T1 spawns T3 by posting a message to a new session. One agent can subscribe files for another session — that's delegation with context.

**MVP: SQLite + JSONL are the backbone (built).** Message queue and session registry live in SQLite (`sessions/queue.sqlite`). Session history lives in JSONL (`sessions/{name}/*.jsonl`). Subscription records don't exist yet. **Note:** queue claiming is not atomic across processes — see [risks.md](current/risks.md#p1-2). Session append is not atomic (memory before disk) — see [risks.md](current/risks.md#p1-9).

## Topics at scale

**V1: Topics are the grouping layer.** A topic is a name + activation state + subscriptions + triggers + relations. All in SQLite, all managed through CLI.

**V1: Subscriptions still work standalone.** When topics aren't needed, subscriptions work standalone (implicitly in `_default` topic).

**V2: Topic export/import for cross-agent delegation.** `topic export/import` bundles metadata for cross-session transfer. Scout T3 adds subs → coder T3 loads topic → zero exploration.

**V2: 200 topics, one context window.** Agent loads 1-3 at a time. Everything else on disk, indexed, resumable.

**V1: Triggers.** Cron + webhook only. No file-watching (cron + stat does the same job).

## CLI surface

```bash
# V1: Subscriptions (standalone or topic-scoped)
./autopoiesis sub add <path> [--topic <name>] [--lines 10-50] [--regex "pub fn"]
./autopoiesis sub remove <path> [--topic <name>]
./autopoiesis sub list [--topic <name>]

# V1: Topics (optional grouping)
./autopoiesis topic create/delete/activate/deactivate/list <name>

# V2: Topic export/import
./autopoiesis topic export/import <name>

# V1: Triggers
./autopoiesis trigger add --topic <name> --type cron --schedule "*/5 * * * *"
./autopoiesis trigger add --topic <name> --type webhook --path "/hooks/deploy"

# MVP: Messaging / sessions
./autopoiesis msg send --session <id> "content"
./autopoiesis session list

# V2: Identity (self-modification within operator bounds)
./autopoiesis identity get/set <dimension> <value>
```

## Surfaces

**MVP: CLI + server.** CLI-first, not IDE-first. IDE integration is not a priority — the operator can VSCode Remote into the repo if needed. The runtime owns the experience, not the editor.

- **MVP:** CLI — primary interface. One binary, works everywhere.
- **V1:** Web GUI — lightweight client for the HTTP/SSE server. Works on any device with a browser.
- **V2:** Android app — native companion for mobile access.
- **V2:** Desktop — Windows + Linux native clients (or wrapper over web GUI).

**MVP:** All surfaces talk to the same server/queue. The agent doesn't know or care which surface sent the message.

**V2:** No surface-specific logic in the runtime. For code-centric workflows, the CLI + worktree isolation (via T2/T3 tiers) replaces what IDE integrations provide in other runtimes.

## Multi-agent tiers

**MVP: User → T1.** Single-agent CLI; one personal assistant per user.

**V1: User → T1 → T3.** T1 can hand bounded tasks to ephemeral executors. T3 is blind, executes the prompt, reports results, dies.

**V2: User → T1 → T2 → T3.** Three tiers, clear responsibilities:

- **MVP:** T1 (personal assistant) — one per user. Always running. Handles direct conversation, routing, and delegation. Lightweight model (Sonnet-class).
- **V2:** T2 (planner/orchestrator) — domain-scoped. Pure planner, no code. Manages dozens of parallel T3 executors. Every user gets a "personal" T2 by default. When a topic (like "managing my gym") outgrows the personal T2, aprs spins up a dedicated T2 instance for that domain. T1 can talk to any T2 — personal or domain-specific. T2 sets up the workspace: git worktree, subscribed files, relevant context.
- **V1:** T3 (ephemeral executor) — blind worker. Gets a topic + prompt, executes, reports results, dies. The caller sets up the workspace: git worktree, subscribed files, relevant context. T3 does zero exploration.

**V2: T1/T2 = one brain, two speeds.** Same identity, same workspace. T2 is T1 with a bigger model and reasoning budget. T3 is disposable.

**V2: T1 talks to any T2.** The personal T1 is the user's single interface. It routes to the personal T2 for general work, or to domain-specific T2s for specialized domains. T2s are autonomous within their domain — T1 delegates and checks in, doesn't micromanage.

**V2: Worktree isolation for parallel coding.** T2 creates a git worktree in T3's workspace. T3 works on its branch, pushes, opens PR. Multiple T3s work in parallel without conflicts.

**V2: Scaling pattern:**
```
User
 └── T1 (personal, always on)
      ├── T2-gym (dedicated: gym management)
      │    ├── T3 (scheduling task)
      │    ├── T3 (member analysis)
      │    └── T3 (email drafts)
      ├── T2-code (dedicated: autopoiesis dev)
      │    ├── T3 (fix P0-2, worktree /tmp/fix-p0-2)
      │    └── T3 (build subscriptions, worktree /tmp/feat-subs)
      └── T3 (quick one-off, no T2 needed)
```

## Skills

Two-tiered: shipped and custom.

**MVP:** Shipped skills are core cognitive capabilities: web search, deep research, coding, planning. Thought patterns injected into context, not runtime plugins.

**V1:** Custom skills are ALL ingress/egress adapters. The core idea: aprs custom-codes everything. When it needs a new integration, it reads API docs, generates the connector, tests, deploys — all through shell. MCP, A2A, webhooks — ingress/egress adapters managed through shell and queue, not new tools. Autopoiesis ships zero vendor connectors.

## Memory

**MVP: 100% file-based.** Topics + journal + summaries + file workspace.

- **Journal** — append-only daily files. Raw events, decisions, observations.
- **Summaries** — distilled from journal entries. Agent curates what matters.
- **Topic files** — working notes per domain/project. Plans, state, questions.
- **Workspace files** — code, configs, data. First-class context via subscriptions.

**Memory policy (even for MVP):**
- **Provenance** — every memory entry records its source (which session, which message, which tool call). Memory without provenance is a context-poisoning vector.
- **Citation** — when the agent uses a memory, it cites the source. Traceable reasoning.
- **Promotion** — raw journal → reviewed summary → durable fact. Explicit promotion, not silent accumulation.
- **Pruning** — stale entries are archived or deleted. Agent proposes, operator approves pruning of durable facts. Journal entries auto-archive after configurable TTL.
- **Taint inheritance** — memories derived from tainted input inherit taint. A fact learned from an untrusted webhook stays tainted until operator-verified.

**V1: Knowledge engine.** Graph-based. Bitemporal. Source-tracked provenance. Truth scoring. Ontology discovery. Hybrid retrieval (FTS + vector + graph traversal).

**Progression:** files → files + search → graph knowledge engine. Each layer subsumes the previous.

## Observability and evaluation

**MVP: Observability.** Every shell call, guard verdict, approval decision, subscription change, and queue transition is logged and auditable via SQLite + JSONL. Entire execution history replayable from disk.

**V1: Evaluation.** Structured eval framework. Test cases (input + expected behavior), scored results, regression detection, constitution compliance scoring, gate promotion based on eval.

**V2: Autonomous experiments.** The agent designs and runs its own experiments. Tests hypotheses about its behavior. Self-tunes persona dimensions based on measured outcomes. Proposes identity changes backed by evidence.

## Multimodal stance

**MVP:** The CLI is text+shell. The runtime itself stays message-based — modalities are content types in the inbox, not new tools.

**V1:** The GUI introduces rich modality: canvas (interactive visual workspace), image/file rendering, and whatever the surface supports.

**V2:** Voice input/output, rich media, and future surface-specific modalities.

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
