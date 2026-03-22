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

**Topics are pure indexes in SQLite. No special topic files.**

A topic is a name, an activation state, and a set of subscriptions, triggers, and relations. All in SQLite, all managed through CLI. There is no `topics/` directory, no magic `.md` convention.

If the agent wants working notes for a topic, it creates any file anywhere and subscribes it:
```bash
./autopoiesis topic create fix-auth
echo "## Plan\n1. Fix middleware\n2. Add tests" > notes/auth-plan.md
./autopoiesis sub add --topic fix-auth notes/auth-plan.md
./autopoiesis sub add --topic fix-auth src/server.rs --lines 174-230
./autopoiesis sub add --topic fix-auth src/guard.rs --regex "fn check"
./autopoiesis topic activate fix-auth
```

The "prose" is just another subscription. Code files, notes, result files, configs — all subscriptions, all equal. The topic doesn't care what it points to.

**A topic with zero subscriptions is valid.** Just a name + triggers. A topic with one notes file is a scratchpad. A topic with ten code files is a working set. Same mechanism throughout.

**Lifecycle and structure** — all through CLI, all in SQLite:
- `topic create/delete/activate/deactivate` — lifecycle
- `sub add/remove/list` — what files to load
- `trigger add/remove/list` — when to wake the agent
- `relation add/remove/list` — dependencies between topics
- `topic export/import` — portable bundles for cross-session transfer

**Enforcement:** ShellSafety guard rule blocks direct `sqlite3` writes to the topic database. CLI is the only validated entry point.

**Default topic.** A catch-all topic (e.g. `topics/_default.md`) holds subscriptions that don't belong to any specific project — skills, global references, workspace config. Always active. Anything that isn't a dedicated topic's concern goes here.

**CLI is the self-management interface.** The agent manages itself through its own binary via shell: `sub add/remove/list`, `msg send`, `session list`. Files for storage, CLI for validated control plane. Every management action is a shell call, visible in history, auditable by guards.

**Safety is multi-dimensional.** Guards are not a single approval check. The control plane combines four dimensions: **budget** (resource/cost ceilings), **permissions** (what the agent is allowed to touch), **taint** (tracking provenance of untrusted input), and **approval** (human-in-the-loop escalation). A command can pass approval but fail budget. A command can be within permissions but tainted. The guard pipeline evaluates all dimensions; approval alone is insufficient for safe autonomy.

**Skills are two-tiered: shipped and custom.**

*Shipped skills* are core cognitive capabilities that come with the binary: web search, deep research, coding, planning. These are thought patterns — reusable reasoning strategies injected into context, not runtime plugins. They teach the agent HOW to think about a class of problem.

*Custom skills* are built by the agent or the operator. Connectors, procedures, workflows, integrations. When the agent needs a new integration, it reads the docs, generates the connector, tests it, and deploys it — all through shell. Autopoiesis ships zero vendor connectors. The meta-skill of building connectors is a shipped skill; the connectors themselves are custom.

Both tiers live as context (subscriptions, topic indexes) rather than as code in the binary. The tool surface stays at one (shell). Skills are context, not tools.

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

T3 gets its own workspace — either ephemeral (spun up, destroyed after) or static (persistent). Workspace setup copies in the files T3 needs: constitution.md (always), plus whatever T2 delegates via `topic export` (subscribed files, relevant sources). For coding tasks, T3 gets a git worktree. T2's instructions arrive as the first message — they ARE T3's operator directives. No operator.md file, no persona dimensions, no self-modification. T3 is a blind executor — receives instructions, executes exactly, reports results. When done, it's done.

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
     [active topic subscriptions]                      ← materialized file content
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

A topic is a pure SQLite entry: name, active state, subscriptions, triggers, relations. No special files.

**Loading a topic** activates its subscriptions — file content materializes in the history timeline by timestamp. That's it. No separate "prose loading" step.

**Example:**
```bash
./autopoiesis topic create fix-auth
./autopoiesis sub add --topic fix-auth src/server.rs --lines 174-230
./autopoiesis sub add --topic fix-auth src/guard.rs --regex "fn check"
./autopoiesis sub add --topic fix-auth notes/auth-plan.md           # agent's working notes
./autopoiesis trigger add --topic fix-auth --type cron --schedule "*/5 * * * *"
./autopoiesis relation add --topic fix-auth --blocked-by db-migration
./autopoiesis topic activate fix-auth
# → all three files now in context. Trigger checks every 5 min.
```

**Topics at scale:**
- Layer 0: Topic list — "I have 200 topics" (~2KB, always visible in context.md)
- Layer 1: Subscribed content — actual file content (materialized in history timeline when topic is active)

200 topics, one context window. Agent loads 1-3 at a time. Everything else is on disk, indexed, resumable.

**Topic as portable context:**
- `topic export fix-auth` bundles DB metadata (subs, triggers, relations) into a portable artifact
- T1 sends scout T3 with a topic — scout reads code, adds subscriptions, creates working notes files
- T1 sends coder T3 with same topic — coder loads topic, all subs ready. Zero exploration.
- `topic import <bundle>` restores the topic in a new session

**Triggers** are two types, both server-side:
- **Cron** — schedule expression evaluated by server tick loop (1/min). Enqueues message when matched.
- **Webhook** — HTTP POST to a registered path. Server receives request, enqueues payload as message.

No file-watching triggers. If the agent needs to detect file changes, a cron trigger with a shell command that checks `stat`/`git diff`/`find -newer` does the same job with zero infrastructure. The agent already has shell.

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

# Topics (pure SQLite — no special files)
./autopoiesis topic create <name>                 # register topic in DB
./autopoiesis topic delete <name>                 # unregister + CASCADE subs/triggers/relations
./autopoiesis topic list                          # all topics with status
./autopoiesis topic activate <name>               # load topic into current session
./autopoiesis topic deactivate <name>             # unload topic
./autopoiesis topic export <name>                 # bundle DB metadata for transfer
./autopoiesis topic import <bundle>               # restore in new session

# Triggers (two types: cron + webhook)
./autopoiesis trigger add --topic <name> --type cron --schedule "*/5 * * * *"
./autopoiesis trigger add --topic <name> --type webhook --path "/hooks/deploy"
./autopoiesis trigger remove --topic <name> <trigger-id>
./autopoiesis trigger list [--topic <name>]

# Relations
./autopoiesis relation add --topic <name> --blocked-by <other>
./autopoiesis relation add --topic <name> --related <other>
./autopoiesis relation remove --topic <name> <relation-id>
./autopoiesis relation list [--topic <name>]

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

Topics are pure SQLite entries — no special files. All topic operations (lifecycle, subscriptions, triggers, relations) go through CLI for validation. Working notes are just files the agent creates and subscribes. ShellSafety must block direct `sqlite3` writes to the topic database.

## Done

- [x] Agent loop (async, streaming, real SSE parsing with incremental byte boundaries)
- [x] Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- [x] Guard pipeline (SecretRedactor, ShellSafety, ExfilDetector — deny>approve>allow)
- [x] Turn architecture (ContextSource + Tool + Guard trait composition)
- [x] Approval system with severity levels + REPL prompt flow
- [x] Session persistence (daily JSONL, tool_call round-trip, replay-safe)
- [x] Identity system v1 (constitution + identity + context, template vars)
- [x] Constitution v1 (4 laws, 1st person, research-backed)
- [x] OAuth device flow auth (token storage, refresh, expiry)
- [x] Token estimation via tiktoken-rs + context trimming
- [x] SQLite message queue + session store (source-agnostic inbox)
- [x] Unified drain_queue() for CLI and server execution
- [x] axum HTTP server + WebSocket
- [x] API key auth middleware (header + WS query param)
- [x] Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler callbacks)
- [x] Kill child process on shell timeout (process-group aware, setpgid + killpg)
- [x] CI pipeline — GitHub Actions (fmt + clippy + test on every PR)
- [x] Shell output cap + file-backed result storage (4KB threshold, forces subscription pattern)
- [x] Persistent named sessions with default session (`--session <name>`)
- [x] Server path sanitization (session_id validation on HTTP routes)
- [x] Stale message recovery on startup

## Next

### Security stack (priority — build in this order)

1. **P0 fixes** — HTTP role injection (force user-only from external callers), approval denial terminates the turn (wire up TurnVerdict::Denied), shell default-approve with allowlist/denylist config
2. **Standing approvals** — `[shell.standing_approvals]` in agents.toml. Pattern-based pre-approval for known-safe commands. Operator-configured, not agent-modifiable.
3. **Taint tracking** — `<meta ts="..." principal="operator|user|agent:id|system" />` tags on every message. Tainted input escalates tool calls to manual approval even if they match standing approvals.
4. **Budget enforcement** — per-turn token ceiling, per-session ceiling, per-day ceiling. Structural prevention of runaway loops.

### Context management (after security)

5. **Subscription system** — SQLite table, CLI commands (`sub add/remove/list`), filter support (lines/regex/head/tail/jq), budget reporting. Standalone — no topic dependency.
6. **Context assembly rework** — materialize subscribed content in history by `max(activated, updated)` timestamp, identity.md + context.md at end
7. **Topics** — optional grouping layer on subscriptions. Pure SQLite indexes, `topic/trigger/relation` CLI, `export/import` for portability

### Identity + infrastructure (parallel track)

8. **Identity v2** — operator.md file, persona dimensions in identity.md, `identity set/get` CLI, guard rules blocking writes to constitution.md + operator.md
9. **Trigger evaluation** — server-side cron + webhook → enqueue message
10. **Provider abstraction** — Anthropic, local models
11. **PTY shell** — interactive commands, not just batch
12. **Permissions** — filesystem/network sandboxing (seccomp/landlock/uid-drop) for multi-tenant

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

## Architectural reality check (2026-03-22)

This section exists because the vision above describes a mature system. The codebase is not there yet. Read this before assuming any invariant holds.

**The shell is not contained.** The vision says "one tool" and describes safety as "multi-dimensional gates." The reality is that `sh -lc` runs with the current user's full privileges. ShellSafety is regex pattern matching — trivially bypassed via `python -c`, string concatenation, shell builtins, or any unflagged binary. The guard pipeline is risk reduction, not a security boundary. Until standing approvals + taint tracking + real sandboxing are built, any feature that depends on "the agent can't do X" is built on sand.

**The identity hierarchy has no enforcement.** The vision describes constitution.md as immutable and operator.md as operator-only. The guard pipeline does not yet block writes to these files. `echo "new rules" > identity/constitution.md` works. File permissions alone are insufficient because the agent runs shell as the process user. Guard rules for identity protection are planned, not built.

**CLI self-management is a privilege escalation surface.** The vision says the agent manages subscriptions/topics via its own CLI through shell. That means the agent's context management goes through the same uncontained shell that can write arbitrary files. Without taint tracking, a prompt injection can instruct the agent to `sub add` a malicious file, and the subscription system will faithfully load it into context on every turn.

**The server is single-threaded in practice.** `worker_lock` serializes all sessions behind one global mutex held for the full agent turn (LLM call + tool execution + persistence). The vision of concurrent trigger-driven topic updates clashes with this reality. The SQLite queue claim is also non-atomic across processes.

**HTTP prompt integrity is broken.** The enqueue endpoint accepts arbitrary `role` from callers. `system` and `assistant` messages can be injected into persistent history by anyone with the API key. The 4-layer identity hierarchy (constitution → operator → identity → context) is meaningless when the "operator" layer can be spoofed via HTTP. Fix this before building identity v2.

**These are not future concerns.** They are current blockers for every feature in the Next section that depends on safety, identity, or concurrency. The build order in Next reflects this: security stack first, then context management, then identity.

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
