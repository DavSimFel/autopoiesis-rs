# Roadmap

> Updated: 2026-03-26
> Status: Phase 1 through Phase 5 and the Plan Engine are complete.

## Completed Phases

### Phase 1 - Observability and Foundation

Completed:

- Replaced ad hoc printing with `tracing`.
- Added span-aware turn and server instrumentation.
- Loaded tier config from `agents.toml`.
- Introduced the current identity stack and template-driven prompt assembly.
- Added model catalog, shell policy, read policy, and queue config loading.

### Phase 2 - Model Routing and Delegation

Completed:

- Added fail-closed model selection from the catalog.
- Added delegation thresholds.
- Added spawn-time budget checks for child sessions.
- Stored resolved model and tier metadata for children.

### Phase 3 - T2 Capability Layer

Completed:

- Added the structured `read_file` path for T2.
- Kept T2 shell-free.
- Added domain context extension loading.
- Added T2-to-T3 spawning and handoff through the queue.

### Phase 4 - Skills

Completed:

- Added local TOML skill discovery.
- Added skill summaries for T1 and T2.
- Added full skill loading for spawned T3 workers.
- Added skill budget checks and duplicate/unknown skill validation.

### Phase 5 - Hardening

Completed:

- Split the runtime by responsibility into `agent/`, `server/`, `gate/`, and `plan/`.
- Added queue claim recovery and session locking.
- Added shell policy hardening for protected paths and metacharacters.
- Added disk-backed shell output capping.
- Added durable session, subscription, and plan storage.
- Added live HTTP and WebSocket server paths.

### Plan Engine

Completed:

- Added structured `plan-json` parsing from T2.
- Added durable plan runs and step attempts in SQLite.
- Added guarded shell execution reuse for plan steps and checks.
- Added crash recovery and T2 failure notifications.
- Added CLI commands for plan inspection and lifecycle management.

## What Remains

The remaining backlog is narrow:

- Subscriptions v2 and context wiring.
- Topic model work beyond the current subscription topic field.
- Provider abstraction beyond the current OpenAI backend.
- PTY shell support.
- Real permissions or sandboxing.
- Topic export/import.
Everything else in the older roadmap is now shipped and should stay out of the "next" list.
