# VISION.md — autopoiesis-rs

## What is this

A lightweight agent runtime. One binary. One tool (shell). Messages in, actions out.

## Core ideas

**One inbox, any source.** Cron, webhook, user, agent — all feed the same SQLite queue. The agent loop reads the next message, thinks, acts, responds. Never knows or cares where the message came from.

**Shell is the universal tool.** File I/O, web requests, process management, agent-to-agent calls, self-configuration — all through shell. The prompt teaches the agent what to do. The tool surface stays at one.

**Identity is prompt, not code.** Constitution, personality, context — all markdown files. The agent can edit its own context.md via shell to manage topics and subscriptions. Behavior changes without recompilation.

**Agent-to-agent = message.** T1 spawns T3 by posting a message to a new session. T3 responds when done. Same inbox, same queue, same processing. Multi-agent tiers are just different model configs.

**SQLite is the backbone.** Session state, message queue, history — one database file. ACID, concurrent-safe, crash-recoverable, shell-accessible (`sqlite3`). No external services.

**Shell results are files.** Tool output is saved to dedicated files and linked in message history. The pipeline uses summaries or truncated results for context, but the agent can read the full result at any time. This keeps context lean while preserving full fidelity.

## Architecture

```
            ┌─ CLI stdin
            ├─ HTTP POST /api/message
sources ────┼─ WebSocket frame          ──→  SQLite queue  ──→  agent loop  ──→  responses
            ├─ cron / webhook                (ordered,          (turn by         (WS stream,
            └─ agent CLI call                 persistent)        turn)            HTTP, file)

agent loop:
  1. read next message from queue
  2. assemble context (identity + history + topic files)
  3. call LLM (stream tokens to connected clients)
  4. if tool call → guard check → shell execute → loop
  5. if done → persist turn, mark message processed

shell results:
  - full output → sessions/{id}/results/{call_id}.txt
  - summary/truncated → inline in message history
  - agent reads full file on demand via shell
```

## Done

- [x] Agent loop (async, streaming)
- [x] Shell tool (async, RLIMIT-sandboxed, process-group kill on timeout)
- [x] Guard pipeline (secret redactor, shell safety, exfil detector)
- [x] Session persistence (JSONL history + SQLite queue)
- [x] Identity system (constitution + identity + context, template vars)
- [x] OAuth device flow auth
- [x] Token estimation + context trimming
- [x] SQLite message queue + session store
- [x] axum HTTP server + WebSocket
- [x] API key auth middleware (header + WS query param)
- [x] Decouple agent loop from stdin/stdout (TokenSink + ApprovalHandler callbacks)
- [x] Kill child process on shell timeout (process-group aware)

## Next

- [ ] CI pipeline (GitHub Actions)
- [ ] PTY shell (interactive commands, not just batch)
- [ ] Shell results → file storage (full output in files, summaries in history)
- [ ] Provider abstraction (Anthropic, local models)
- [ ] CLI as separate crate — full TUI with graceful degradation to plain terminal
- [ ] Multiplatform GUI client (web, Windows, Linux, Android)

## Principles

1. **One tool.** If you're adding a tool, you're probably wrong. Make the prompt smarter.
2. **One queue.** All messages enter the same way. Source doesn't matter.
3. **Files over protocols.** Topics, subscriptions, config — editable files, not API calls.
4. **Small surface.** Every line of code is a liability. Fewer lines, fewer bugs.
5. **Crash and resume.** SQLite queue means nothing is lost. Restart and continue.
6. **Full fidelity, lean context.** Shell results saved to files. Summaries in the pipeline. Full data always accessible.
