# VISION.md — autopoiesis-rs

## What is this

A lightweight agent runtime. One binary. One tool (shell). Messages in, actions out.

## Core ideas

**One inbox, any source.** Cron, webhook, user, agent — all feed the same SQLite queue. The agent loop reads the next message, thinks, acts, responds. Never knows or cares where the message came from.

**Shell is the universal tool.** File I/O, web requests, process management, agent-to-agent calls, self-configuration — all through shell. The prompt teaches the agent what to do. The tool surface stays at one.

**Shell output is capped. Full results live in files.** Every shell execution saves full output to `sessions/{id}/results/{call_id}.txt`. Output below threshold (configurable, e.g. 4KB) is also inline in history. Output above threshold: only metadata in history — the agent sees what exists, how big it is, and where it is. To read the content, the agent **subscribes**. This is the forcing mechanism: the agent cannot avoid subscriptions for substantial content.

**Subscriptions are explicit context management.** A subscription injects file content into the context pipeline. Subscriptions always belong to a topic — there are no free-floating session-level subscriptions. The agent subscribes via CLI (`./autopoiesis sub add --topic <name> <path>`). Subscriptions are:
- **Instant** — content appears on the very next turn
- **Optional** — the agent decides what to load, nothing is auto-loaded above threshold
- **Positional** — content is placed in the history timeline by `max(activated, updated)` timestamp
- **Reactive** — when a subscribed file changes, it gets a new timestamp and bubbles forward in the timeline

**Subscriptions are always to files.** Never to HTTP endpoints, databases, or external sources directly. Shell is the universal adapter for external data — a trigger or manual shell command fetches external state and writes it to a file. The subscription sees the file change and loads the content. This keeps subscriptions simple (local disk reads, microseconds, never fail), avoids latency/caching/auth complexity in context assembly, and preserves shell as the integration layer.

**Subscriptions support filtering.** Not every subscription loads the full file:
- **Full** — entire file content (default)
- **Lines** — range filter (`--lines 174-230`)
- **Regex** — pattern extraction (`--regex "pub fn"`)
- **Head/Tail** — first/last N lines (`--head 100`, `--tail 50`)
- **JQ** — JSON query filter (`--jq ".status"`) for structured data files

Filtering happens at context assembly time. The agent subscribes once with a filter; every turn extracts only the relevant content. This replaces repetitive grep/sed commands the agent would otherwise run manually each turn.

**External data follows the trigger → file → subscription path:**
```
trigger (cron 5m) → shell: curl -s api.example.com/health | jq . > context/health.json
subscription: context/health.json --jq ".status"
```
Trigger fires → shell writes file → subscription sees new mtime → content bubbles forward. No HTTP in the subscription layer. No caching subsystem. No SSRF surface.

No hard budget limit. The CLI reports context utilization on every subscription change ("added. total estimate: 78KB, ~65% of context window"). When approaching capacity, it warns ("~93% — consider disabling inactive topics"). The agent manages its own context. If it overflows, existing history trimming drops the oldest turns. Hard limits are for safety (guards); context management is for intelligence (the agent's job).

Cache optimization: everything before the earliest moved subscription stays cached. Stable subscriptions = stable cache prefix.

**Topics are indexes.** A topic is a single `.md` file that maps everything the agent needs for one concern — plans, state, questions, relevant files. The content lives elsewhere; the topic **points** to it. Topics are the agent's cognitive architecture for managing hundreds of tasks, projects, and goals with a finite context window.

**Topics have two zones with different write rules:**

*Prose sections* (plan, state, questions) — the agent reads and writes freely via shell. This is working memory. The agent updates plans, logs state, writes questions. Direct file edits (`sed`, `echo >>`, etc.) are fine for freeform text.

*Structured sections* (triggers, items/subscriptions, relations) — CLI only. These have schema and affect runtime behavior. `sub add`, `topic add-trigger`, etc. validate before writing. Raw shell edits to TOML code blocks would bypass validation and could break the harness. The CLI is the gatekeeper for structured data.

*Topic lifecycle* (create, delete, activate, deactivate) — CLI only. Auditable, validates state transitions, maintains indexes.

The identity prompt teaches this rule. ShellSafety guard enforcement of structured blocks is a later hardening step.

**Default topic.** A catch-all topic (e.g. `topics/_default.md`) holds subscriptions that don't belong to any specific project — skills, global references, workspace config. Always active. Anything that isn't a dedicated topic's concern goes here.

**CLI is the self-management interface.** The agent manages itself through its own binary via shell: `sub add/remove/list`, `msg send`, `session list`. Files for storage, CLI for validated control plane. Every management action is a shell call, visible in history, auditable by guards.

**Safety is multi-dimensional.** Guards are not a single approval check. The control plane combines four dimensions: **budget** (resource/cost ceilings), **permissions** (what the agent is allowed to touch), **taint** (tracking provenance of untrusted input), and **approval** (human-in-the-loop escalation). A command can pass approval but fail budget. A command can be within permissions but tainted. The guard pipeline evaluates all dimensions; approval alone is insufficient for safe autonomy.

**Skills are two-tiered: shipped and custom.**

*Shipped skills* are core cognitive capabilities that come with the binary: web search, deep research, coding, planning. These are thought patterns — reusable reasoning strategies injected into context, not runtime plugins. They teach the agent HOW to think about a class of problem.

*Custom skills* are built by the agent or the operator. Connectors, procedures, workflows, integrations. When the agent needs a new integration, it reads the docs, generates the connector, tests it, and deploys it — all through shell. Autopoiesis ships zero vendor connectors. The meta-skill of building connectors is a shipped skill; the connectors themselves are custom.

Both tiers live as context (topic files, subscriptions) rather than as code in the binary. The tool surface stays at one (shell). Skills are context, not tools.

**Identity is layered, not flat.** Three files, strict hierarchy:

1. **`constitution.md`** — Laws of thought. Epistemic fidelity, chain of command, reversible action, contextual continuity. Immutable. Only the operator modifies this through direct file access.
2. **`operator.md`** — Operator policy. Purpose of this agent instance, boundaries, permissions, budget ceilings, communication rules, tool constraints. Written by the operator. Agent cannot modify.
3. **`identity.md`** — Agent persona. Self-modifiable via CLI within operator-set bounds. Contains persona dimensions and freeform sections for learned patterns and stance. The agent shapes who it is over time.

context.md is NOT part of the identity stack — it's operational steering, documented separately below.

Constitution answers *how to think*. Operator answers *what you're allowed to do*. Identity answers *who you are*.

Write-protection for constitution.md and operator.md is enforced by the guard pipeline (ShellSafety must block writes to these paths). This guard rule does not exist yet — it's part of the identity v2 work. The agent runs shell and can write anywhere the process user can, so file-permission tricks alone are insufficient.

**Persona dimensions (hypothesis — needs eval).** Structured traits in identity.md that the agent self-tunes:

| Dimension | Range | Controls |
|-----------|-------|----------|
| `voice` | freeform | Tone, style of output |
| `autonomy` | low / moderate / high | Act-first vs ask-first threshold |
| `verbosity` | terse / normal / detailed | Default response length |
| `risk_tolerance` | cautious / moderate / bold | How much to attempt before escalating |
| `focus` | freeform | Current working area, refreshed often |

The idea: an agent starts with defaults. Over sessions, it adjusts — operator never overrides decisions → `autonomy: high`. Makes a mistake → `risk_tolerance: cautious` for that domain. Self-tuning personality within operator rails.

**This is a design hypothesis, not a proven mechanism.** Whether models actually change behavior based on these structured traits in the prompt needs empirical testing (see Open Questions). Storage format is also TBD — structured data inside markdown is fragile; a TOML sidecar (`persona.toml` next to identity.md) may be cleaner for CLI parsing.

**Per-tier identity and workspaces.** T1 and T2 share a workspace and get the full identity stack: constitution + operator + identity + context. They have persona, self-modification, working style. Same brain, two speeds.

T3 gets its own workspace — either ephemeral (spun up, destroyed after) or static (persistent). Workspace setup copies in the files T3 needs: constitution.md (always), plus whatever T2 delegates (topic files, relevant sources). For coding tasks, T3 gets a git worktree. T2's instructions arrive as the first message — they ARE T3's operator directives. No operator.md file, no persona dimensions, no self-modification. T3 is a blind executor — receives instructions, executes exactly, reports results. When done, it's done.

**context.md is LLM steering.** Reminders, behavioral notes, focus directives, active constraints. "Remember to use grep for large files." "Currently prioritizing auth fixes." Not a subscription index — topics own subscriptions. context.md shapes *how* the agent thinks. Topics shape *what* it thinks about. Both positioned at the end of context, right before the latest message, for maximum relevance.

**Messages carry metadata.** Every user-role message gets XML tags injected before content:

```xml
<meta ts="2026-03-17T07:22:00Z" principal="operator" />
How's the build going?
```

`principal` values: `operator`, `user`, `agent:<id>`, `system`. This makes the constitution's Chain of Command (Law 2) enforceable per-message — the model sees WHO is talking, not just what they said. Timestamp is per-message, not a template var. Thin injection in the message builder, zero overhead.

**Agent-to-agent = message.** T1 spawns T3 by posting a message to a new session. One agent can subscribe files for another's session — that's delegation with context. Same inbox, same queue, same processing. Multi-agent tiers are just different model configs.

**SQLite is the backbone.** Session state, message queue, subscription records, history — one database file. ACID, concurrent-safe, crash-recoverable, shell-accessible (`sqlite3`).

## Architecture

```
sources ──→ SQLite queue ──→ agent loop ──→ responses

agent loop:
  1. dequeue next message
  2. assemble context (layout is PROVISIONAL — see Open Questions):
     [system: constitution.md + operator.md]           ← immutable, cached forever
     [history: turns + materialized sub content]       ← sorted by timestamp
       ├─ user msg (13:00) ← <meta ts="..." principal="operator" />
       ├─ assistant msg (13:01)
       ├─ tool result inline (13:02, <4KB)
       ├─ sub: src/auth.rs (max(act=13:10, upd=13:45) = 13:45)
       ├─ user msg (14:00)
       ├─ sub: results/call_abc.txt (max(act=14:02, upd=14:02) = 14:02)
       └─ assistant msg (14:05)
     [active topics: prose from loaded topic files]    ← plans, state, questions
     [identity.md: persona, patterns, stance]          ← self-modifiable, recency helps?
     [context.md: steering, reminders, focus]          ← shapes how the agent thinks
     [current message]                                 ← new, with <meta /> tags
  3. call LLM (stream tokens)
  4. if tool call → guard check → shell execute → save result to file → loop
  5. if done → persist turn, mark message processed

shell results:
  - every result → sessions/{id}/results/{call_id}.txt (always)
  - ≤ threshold → also inline in history
  - > threshold → metadata only:
    "output exceeded limit (500 lines, 24KB) → results/call_abc.txt | subscribe to read"
```

## Topics

A topic is a single `.md` file. Code blocks hold structured data the harness evaluates. Prose sections are free-form — the agent reads and writes them via shell.

```markdown
# Fix Auth Bug

Status: active
Priority: high

## Triggers

​```toml
[[trigger]]
type = "file_change"
paths = ["src/server.rs", "src/auth.rs"]

[[trigger]]
type = "cron"
schedule = "daily"
​```

## Items

​```toml
[[item]]
path = "src/server.rs"
summary = "Auth middleware + WS handler"
lines = "174-230"

[[item]]
path = "src/guard.rs"
summary = "Guard pipeline, may need gate changes"
​```

## Relations

​```toml
blocked_by = ["database-migration"]
related = ["security-audit"]
​```

## Plan

Replace WsAutoApprove with real gate check...

## State

P1 fixes applied. WS approval bypass still open.

## Questions

- Should destructive tools require per-call approval?
```

Three audiences: agent reads/writes the whole file as markdown. Harness parses code blocks for triggers, items, relations. Humans read it like a document.

**Loading a topic does two things:**
1. Its **file subscriptions** (items) activate → content materializes in the history timeline by timestamp
2. Its **prose** (plan, state, questions) appears in the end zone → maximum LLM attention

**Topics at scale:**
- Layer 0: Topic list — "I have 200 topics" (~2KB, always visible in context.md)
- Layer 1: Topic prose — "fix-auth plan, current state, open questions" (~500B–2KB, in end zone when active)
- Layer 2: Item content — actual file content (materialized in history timeline when topic is active)

200 topics, one context window. Agent loads 1-3 at a time. Everything else is on disk, indexed, resumable.

**Topic as portable context:**
- T1 sends scout T3 to explore a codebase
- Scout reads code, subscribes relevant files to the topic, writes plan
- T1 sends coder T3 with same topic
- Coder loads topic → all subs ready, plan written. Zero exploration. Codes immediately.

**Triggers** are evaluated server-side. The server watches trigger conditions (cron ticks, file mtime changes, incoming webhooks) and enqueues a message to the appropriate session when a trigger fires. The agent just sees a new inbox message.

## CLI Self-Management

```bash
# Subscriptions (always topic-scoped, always to files)
./autopoiesis sub add --topic <name> <path>                      # full file
./autopoiesis sub add --topic <name> <path> --lines 174-230      # line range
./autopoiesis sub add --topic <name> <path> --regex "pub fn"     # pattern extraction
./autopoiesis sub add --topic <name> <path> --head 100           # first N lines
./autopoiesis sub add --topic <name> <path> --tail 50            # last N lines
./autopoiesis sub add --topic <name> <path> --jq ".status"       # JSON query
./autopoiesis sub add --topic _default <path>                    # non-project sub
./autopoiesis sub remove --topic <name> <path>
./autopoiesis sub list [--topic <name>]

# Cross-session subscription (delegation)
./autopoiesis sub add --session <id> --topic <name> <path>

# Topics
./autopoiesis topic list                          # all topics with status
./autopoiesis topic validate <name>               # validate code blocks
./autopoiesis topic activate <name>               # load topic into current session
./autopoiesis topic deactivate <name>             # unload topic

# Identity (self-modification within operator bounds)
./autopoiesis identity get persona               # show current dimensions
./autopoiesis identity set voice "terse, technical"
./autopoiesis identity set autonomy high          # low | moderate | high
./autopoiesis identity set verbosity terse        # terse | normal | detailed
./autopoiesis identity set risk_tolerance cautious # cautious | moderate | bold
./autopoiesis identity set focus "axum server"    # freeform, updated often

# Messaging
./autopoiesis msg send --session <id> "content"

# Sessions
./autopoiesis session list
```

CLI validates: path safety (no traversal), file existence. Reports context utilization on every change — warns on overflow, never blocks. Every action is a shell call in history, auditable by guards.

Topic prose sections (plan, state, questions) are edited directly via shell. Structured sections (triggers, subscriptions, relations) and lifecycle operations (create, delete, activate) go through CLI for validation.

## Done

- [x] Agent loop (async, streaming)
- [x] Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- [x] Guard pipeline (secret redactor, shell safety, exfil detector)
- [x] Session persistence (JSONL history)
- [x] Identity system v1 (constitution + identity + context, template vars)
- [ ] Identity system v2 (4-layer: constitution + operator + identity + context, persona dimensions, `identity` CLI)
- [x] OAuth device flow auth
- [x] Token estimation + context trimming
- [x] SQLite message queue + session store
- [x] axum HTTP server + WebSocket
- [x] API key auth middleware (header + WS query param)
- [x] Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler callbacks)
- [x] Kill child process on shell timeout (process-group aware)

## Next

1. **Identity v2** — operator.md file, persona dimensions in identity.md (storage format TBD), `identity set/get` CLI, ShellSafety guard rule blocking writes to constitution.md + operator.md
2. **Message metadata injection** — `<meta ts="..." principal="..." />` tags on every user message in the message builder, principal resolution from session/source metadata
3. **Shell output cap + file storage** — save all results to files, cap inline output, force subscription pattern
4. **Subscription system** — SQLite table, CLI commands (`sub add/remove/list`), budget enforcement
5. **Context assembly rework** — materialize sub content in history by `max(activated, updated)` timestamp, identity.md + context.md at end (see Open Questions)
6. **Topics** — `.md` files with code blocks, topic list in context.md, `topic validate/list` CLI
7. **CI pipeline** — GitHub Actions (lint, test, build on every PR)
8. **Trigger evaluation** — server-side cron/file_change/webhook → enqueue message
9. **PTY shell** — interactive commands, not just batch
10. **Provider abstraction** — Anthropic, local models
11. **CLI as separate crate** — TUI with graceful degradation

## Open Questions

**Instruction positioning: top vs bottom vs split.** The current architecture places system instructions (constitution + operator) at position 0 — before all history. Models are typically trained with system prompts at the end (closer to generation). Three variants to test empirically:
- **A: Top** — all instructions before history (current design, maximizes KV cache stability)
- **B: Bottom** — all instructions after history, before current message (matches training, recency bias helps compliance)
- **C: Split** — constitution + operator at top (stable laws, cached), identity + context at bottom (persona refreshed by recency)

Hypothesis: C wins — laws anchored at top for stability, persona refreshed at bottom for recency. But this needs a structured eval (20+ prompts testing boundary compliance, persona consistency, instruction recall after 10+ turns), not vibes. Until tested, the architecture diagram marks the layout as provisional.

**Persona dimensions: do they actually work?** The assumption that structured traits like `verbosity: terse` or `autonomy: high` in the system prompt measurably change model behavior is untested. Possible outcomes:
- They work well → keep as structured config
- They work weakly → merge into freeform prose ("be terse, act without asking")
- They don't work → drop the concept, persona is just freeform identity.md prose

Needs the same kind of eval: baseline vs dimensions-in-prompt, measuring actual output length, decision patterns, tone consistency across turns.

## Principles

1. **One tool.** If you're adding a tool, you're probably wrong. Make the prompt smarter.
2. **One queue.** All messages enter the same way. Source doesn't matter.
3. **Agent controls its context.** No opaque truncation. Agent sees what exists, decides what to load.
4. **Topics are indexes, not containers.** They point to content. Content lives elsewhere.
5. **Subscriptions are instant and optional.** The forcing mechanism is the shell cap, not auto-loading. Warn on overflow, never block.
6. **Files for storage, CLI for control plane.** Raw content = files. Validated state changes = CLI.
7. **Small surface.** Every line of code is a liability. Fewer lines, fewer bugs.
8. **Crash and resume.** SQLite queue means nothing is lost. Restart and continue.
9. **Composition over accretion.** The system scales by composing small primitives (context, subscriptions, topics, queues, files), not by adding features. Richer behavior emerges from how the agent uses the primitives, not from new runtime surface area.
10. **SSE over WebSocket for transport.** Streamable HTTP/SSE is the preferred transport — stateless, resumable, mobile-friendly. WebSocket is supported but SSE should be the primary path for reliability across unreliable networks.
