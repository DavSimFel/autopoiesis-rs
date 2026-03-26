# Vision

> For current state, see [architecture/overview.md](architecture/overview.md).
> For known hazards, see [risks.md](risks.md).
> For build order, see [roadmap.md](roadmap.md).

## What This Is

A personal agent runtime. One brain runs operational work through a small tiered system instead of a grab-bag of one-off integrations.

It is not a framework, a multi-tenant platform, or a generic chatbot. It is a runtime with a fixed execution model, a guarded shell surface, a structured read path, and durable queue-backed handoff.

## What Exists Today

- T1 is the operator-facing fast mode.
- T2 is the reasoning mode.
- T3 is disposable execution.
- The plan engine lets T2 emit structured work instead of free-form prose.
- The skill system lets T1 and T2 discover summaries while spawned T3 workers receive full instructions.
- The guard pipeline enforces budget, redaction, shell safety, and exfil checks.
- SQLite stores sessions, queue items, subscriptions, and plan state.
- The server exposes HTTP and WebSocket control surfaces.

## Tiered Runtime

### T1

T1 is the fast operator-facing brain. It uses shell, can talk directly to the operator, and can delegate harder work.

### T2

T2 is the reasoning layer. It uses `read_file` only. It plans, decomposes, and emits structured plan actions or structured conclusions.

### T3

T3 is disposable execution. It is spawned for a specific task, gets the appropriate model and skills, then runs the shell-backed work and exits.

## Identity

Identity is three layers:

- `constitution.md` - policy for every tier
- `agent.md` - T1 character only
- `context.md` - runtime context for every tier

Domain packs are context extensions. They are appended when selected, not treated as a separate identity layer.

The live identity files are under `identity-templates/`.

## Skills

Skills are local TOML definitions. They are not plugins or libraries.

- T1 and T2 see summaries.
- Spawned T3 workers receive the full skill instructions.
- Skills compose with subscriptions, context, and standing approvals.

## Model Routing

Model selection is fail-closed and config-driven. The runtime resolves explicit overrides first, then route preferences, then the default model, and rejects the spawn if nothing matches.

## Context and Memory

- The queue is the source of truth for inbound work.
- JSONL stores durable session history.
- Subscriptions are durable records with filters and token estimates.
- Shell output above the cap is stored on disk and referenced from history.

## Surfaces

- CLI is the primary operator surface.
- HTTP and WebSocket are built and share the same queue and store.
- All surfaces feed the same runtime; none gets a special execution path.

## What Still Remains

- Subscriptions v2 and context wiring.
- Topic export/import.
- Provider abstraction beyond OpenAI.
- PTY shell.
- Real sandboxing or permissions enforcement.
