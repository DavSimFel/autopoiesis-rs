# VISION.md — autopoiesis-rs

## What is this

A lightweight agent runtime. One binary. One tool (shell). Messages in, actions out.

## Core ideas

**One inbox, any source.** Cron, webhook, user, agent — all feed the same SQLite queue. The agent loop reads the next message, thinks, acts, responds. Never knows or cares where the message came from.

**Shell is the universal tool.** File I/O, web requests, process management, agent-to-agent calls, self-configuration — all through shell. The prompt teaches the agent what to do. The tool surface stays at one.

**Shell output is capped. Full results live in files.** Every shell execution saves full output to `sessions/{id}/results/{call_id}.txt`. Output below threshold (configurable, e.g. 4KB) is also inline in history. Output above threshold: only metadata in history — the agent sees what exists, how big it is, and where it is. To read the content, the agent **subscribes**. This is the forcing mechanism: the agent cannot avoid subscriptions for substantial content.

**Subscriptions are explicit context management.** A subscription injects file content into the context pipeline. The agent subscribes via CLI (`./autopoiesis sub add <path>`). Subscriptions are:
- **Instant** — content appears on the very next turn
- **Optional** — the agent decides what to load, nothing is auto-loaded above threshold
- **Positional** — content is placed in the history timeline by `max(activated, updated)` timestamp
- **Reactive** — when a subscribed file changes, it gets a new timestamp and bubbles forward in the timeline
- **Budget-constrained** — total subscribed bytes have a ceiling. Agent must manage its context budget.

Cache optimization: everything before the earliest moved subscription stays cached. Stable subscriptions = stable cache prefix.

**Topics are indexes.** A topic is a single `.md` file that maps everything the agent needs for one concern — plans, state, questions, relevant files. The content lives elsewhere; the topic **points** to it. Topics are the agent's cognitive architecture for managing hundreds of tasks, projects, and goals with a finite context window.

**CLI is the self-management interface.** The agent manages itself through its own binary via shell: `sub add/remove/list`, `msg send`, `session list`. Files for storage, CLI for validated control plane. Every management action is a shell call, visible in history, auditable by guards.

**Identity is prompt, not code.** Constitution, personality — stable markdown files at the system level. `context.md` is the agent's working memory: active topics, subscription index, current focus, notes. Positioned right before the latest message for maximum attention.

**Agent-to-agent = message.** T1 spawns T3 by posting a message to a new session. One agent can subscribe files for another's session — that's delegation with context. Same inbox, same queue, same processing. Multi-agent tiers are just different model configs.

**SQLite is the backbone.** Session state, message queue, subscription records, history — one database file. ACID, concurrent-safe, crash-recoverable, shell-accessible (`sqlite3`).

## Architecture

```
sources ──→ SQLite queue ──→ agent loop ──→ responses

agent loop:
  1. dequeue next message
  2. assemble context:
     [system: constitution.md + identity.md]          ← stable, cached forever
     [history: turns + materialized sub content]       ← sorted by timestamp
       ├─ user msg (13:00)
       ├─ assistant msg (13:01)
       ├─ tool result inline (13:02, <4KB)
       ├─ sub: src/auth.rs (max(act=13:10, upd=13:45) = 13:45)
       ├─ user msg (14:00)
       ├─ sub: results/call_abc.txt (max(act=14:02, upd=14:02) = 14:02)
       └─ assistant msg (14:05)
     [context.md: active topics, sub index, focus]     ← right before message
     [current message]                                 ← new
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

**Topics at scale:**
- Layer 0: Topic list — "I have 200 topics" (~2KB, always visible in context.md)
- Layer 1: Topic file — "fix-auth has 5 items, here's the plan" (~500B–2KB, loaded when agent opens it via subscription)
- Layer 2: Item content — actual file content (loaded when agent subscribes to specific items)

200 topics, one context window. Agent loads 1-3 at a time. Everything else is on disk, indexed, resumable.

**Topic as portable context:**
- T1 sends scout T3 to explore a codebase
- Scout reads code, subscribes relevant files to the topic, writes plan
- T1 sends coder T3 with same topic
- Coder loads topic → all subs ready, plan written. Zero exploration. Codes immediately.

**Triggers** are evaluated server-side. The server watches trigger conditions (cron ticks, file mtime changes, incoming webhooks) and enqueues a message to the appropriate session when a trigger fires. The agent just sees a new inbox message.

## CLI Self-Management

```bash
# Subscriptions
./autopoiesis sub add [--session <id>] [--topic <name>] <path>
./autopoiesis sub remove [--session <id>] [--topic <name>] <path>
./autopoiesis sub list [--session <id>]

# Messaging
./autopoiesis msg send --session <id> "content"

# Sessions
./autopoiesis session list

# Topics (structural changes only — prose is edited via shell)
./autopoiesis topic validate <name>
./autopoiesis topic list
```

CLI validates: path safety (no traversal), file existence, subscription limits, total context budget. Every action is a shell call in history, auditable by guards.

Topics themselves are just `.md` files — create, edit, delete via shell directly. No CLI needed for content. CLI only validates structured data (code blocks) on demand.

## Done

- [x] Agent loop (async, streaming)
- [x] Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- [x] Guard pipeline (secret redactor, shell safety, exfil detector)
- [x] Session persistence (JSONL history)
- [x] Identity system (constitution + identity + context, template vars)
- [x] OAuth device flow auth
- [x] Token estimation + context trimming
- [x] SQLite message queue + session store
- [x] axum HTTP server + WebSocket
- [x] API key auth middleware (header + WS query param)
- [x] Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler callbacks)
- [x] Kill child process on shell timeout (process-group aware)

## Next

1. **Shell output cap + file storage** — save all results to files, cap inline output, force subscription pattern
2. **Subscription system** — SQLite table, CLI commands (`sub add/remove/list`), budget enforcement
3. **Context assembly rework** — materialize sub content in history by `max(activated, updated)` timestamp, context.md at end
4. **Topics** — `.md` files with code blocks, topic list in context.md, `topic validate/list` CLI
5. **CI pipeline** — GitHub Actions (lint, test, build on every PR)
6. **Trigger evaluation** — server-side cron/file_change/webhook → enqueue message
7. **PTY shell** — interactive commands, not just batch
8. **Provider abstraction** — Anthropic, local models
9. **CLI as separate crate** — TUI with graceful degradation

## Principles

1. **One tool.** If you're adding a tool, you're probably wrong. Make the prompt smarter.
2. **One queue.** All messages enter the same way. Source doesn't matter.
3. **Agent controls its context.** No opaque truncation. Agent sees what exists, decides what to load.
4. **Topics are indexes, not containers.** They point to content. Content lives elsewhere.
5. **Subscriptions are instant and optional.** The forcing mechanism is the shell cap, not auto-loading.
6. **Files for storage, CLI for control plane.** Raw content = files. Validated state changes = CLI.
7. **Small surface.** Every line of code is a liability. Fewer lines, fewer bugs.
8. **Crash and resume.** SQLite queue means nothing is lost. Restart and continue.
