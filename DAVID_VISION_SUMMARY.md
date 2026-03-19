# David Vision Summary

Autopoiesis-rs is a lightweight Rust runtime for long-lived autonomous agents built around a small set of hard primitives: one binary, one universal shell tool, one source-agnostic inbox per session, and one agent loop that turns messages into actions. The system is meant to scale by composition rather than feature accretion: richer behavior comes from context management, subscriptions, topics, tiered agents, and approval gates, not from adding bespoke tools or server-side product logic.

## Core Philosophy

- Shell is the universal tool. File I/O, web access, process management, self-management, and agent-to-agent interaction should all happen through shell, with the prompt teaching usage instead of the runtime adding more tools.
- The inbox is source-agnostic. Cron events, webhooks, user input, and other agents all become the same queued message type feeding the same session inbox.
- Simple primitives should compound into emergent capability. Context pressure, subscriptions, topics, queues, and files are the infrastructure; higher-level behavior should emerge from how the agent uses them.
- T1 and T2 are one brain at two speeds; T3 workers are disposable executors. Long-lived cognition stays in the main identity and workspace, while delegated execution is cheap and replaceable.
- The agent should be able to manage itself through its own CLI. Operational control belongs in validated commands, not ad hoc file edits or special admin surfaces.
- Long-term autonomy is the north star. The intended limit is the approval system, not artificial product constraints or narrow interaction models.
- UI is agent-driven but transport-agnostic. The server should return JSON only; clients render it. Transport should favor streamable HTTP/SSE for reliability, especially on mobile.
- Skills are thought patterns, not vendor-specific connectors. When a new integration is needed, the agent should be able to read docs, generate the connector, test it, and deploy it.
- Safety is multi-dimensional. Gates are not a single yes/no check; they combine budget, permissions, taint, and approval.

## Architecture Overview

The intended architecture is a queue-driven agent runtime backed by SQLite. Each session has one inbox. Sources enqueue messages, the agent loop dequeues the next message, assembles context, calls the model, runs guarded shell commands when needed, persists results, and marks the message complete. The server is therefore a message broker with an agent loop attached, not a chat server.

Context is designed as a layered system. The constitution defines laws of thought, operator policy defines externally imposed boundaries, identity defines the agent's persona and self-adjustable stance, and `context.md` provides short-horizon steering. Topics are the agent's working-set index: each topic is a markdown document that carries plans, state, and structured references to relevant files. Subscriptions are the mechanism that materializes those files into context. They are explicit, topic-scoped, and placed inside history by file recency so that updated material bubbles forward while stable prefixes remain cache-friendly.

Shell execution is supposed to be file-first. Every shell result is written to disk. Small output can also be stored inline in history, but substantial output must be hard-capped and referenced by metadata only, forcing the agent to subscribe to the saved file if it wants to inspect the full result. This turns context selection into an explicit cognitive act rather than an accidental byproduct of large tool outputs.

Multi-agent behavior is intentionally minimal at the protocol level. Agent-to-agent communication should reduce to either a CLI call or an HTTP POST that writes a message into another session's inbox. Different agent tiers come from different `agents.toml` configurations and workspace setups, not from separate orchestration frameworks.

## What Exists Today vs What's Next

Today, the codebase already has the core runtime skeleton: one binary, one shell tool, a shared turn builder for CLI and server, SQLite-backed session/message storage, JSONL persistence, an OpenAI streaming integration, OAuth auth flow, and a guard pipeline. Identity v1 exists as constitution plus identity plus context. The current code therefore reflects the general direction, but not the full design.

Several important parts of David's intended architecture are still roadmap items or only partially realized. The latest review shows the server is not yet fully queue-driven in practice, WebSocket execution currently diverges from the one-inbox model, approval handling is weaker than intended on that path, shell output is not yet hard-capped into file-backed subscriptions, topics and subscription-based context assembly are still ahead, operator.md and identity v2 are not implemented, message metadata injection is still pending, and the security story is materially weaker than the long-term design. In short: the foundation exists, but the system has not yet reached the architectural bar described in the vision.

Near-term work is therefore concentrated around closing those gaps: file-backed shell result storage with enforced subscriptions, a real subscription system in SQLite plus CLI, context assembly that treats subscriptions as first-class timeline entries, topic files as the unit of active work, identity v2 with operator policy and persona controls, message metadata for principal-aware reasoning, and transport/runtime alignment around streamable HTTP/SSE and a truly queue-driven server loop.

## Key Design Decisions and Rationale

- One tool only: keeping shell as the sole tool minimizes runtime surface area, keeps the architecture legible, and pushes capability into prompting and composition rather than proliferating APIs.
- One inbox per session: normalizing all event sources into one queue avoids special cases and makes autonomous behavior schedulable, auditable, and recoverable.
- File-backed shell results with subscriptions: hard output caps prevent context pollution, force deliberate loading of large artifacts, and make long-running work tractable under finite context windows.
- Subscriptions inside history by mtime: representing subscribed content as timeline entries preserves recency semantics and maximizes KV-cache reuse by keeping unchanged prefixes stable.
- Topics as working memory indexes: a topic gives the agent a portable unit of plan, state, and relevant material so it can suspend, resume, delegate, and scale across many concurrent concerns.
- CLI as self-management interface: using the agent's own binary for subscription, topic, message, and context operations keeps administration visible in shell history and subject to the same guard/audit model as any other action.
- T1/T2 as persistent cognition, T3 as disposable execution: this separates long-lived judgment and memory from cheap task-specific labor without inventing a separate conceptual model for delegation.
- JSON-only server responses: keeping the server presentation-free makes clients replaceable and keeps UI decisions outside the runtime core.
- Four-dimensional gates: approval alone is too narrow; budget, permissions, taint, and approval together define the real control plane for safe autonomy.
