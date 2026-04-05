# Identity System v2

> Status: implemented.
> Purpose: describe the live three-layer identity stack and how config assembles prompts.

## Summary

The runtime uses one brain with three execution tiers:

- T1: fast operator-facing mode
- T2: reasoning and planning mode
- T3: disposable execution mode

T1 and T2 are the same identity expressed at different speeds. T3 is a task worker spawned for a specific job.

## Identity Stack

The current stack has three layers:

- `constitution.md` - policy layer, loaded for every tier
- `agent.md` - character layer, loaded for T1 only
- `context.md` - session context layer, loaded for every tier

The shipped source of truth is `src/shipped/identity-templates/`.

### T1

T1 loads:

- `src/shipped/identity-templates/constitution.md`
- `src/shipped/identity-templates/agents/<name>/agent.md`
- `src/shipped/identity-templates/context.md`

### T2 and T3

T2 and T3 load:

- `src/shipped/identity-templates/constitution.md`
- `src/shipped/identity-templates/context.md`

### Domains

Selected domains append `context_extend` files during prompt assembly. Domain context is an extension layer, not a separate identity layer.

## `agents.toml`

The live configuration shape is:

- `[agents.<name>]`
- `[agents.<name>.t1]`
- `[agents.<name>.t2]`
- `[models]`
- `[models.catalog.*]`
- `[models.routes.*]`
- `[shell]`
- `[read]`
- `[queue]`
- `[domains]`
- `[domains.<name>]`

The code loads the selected agent identity, the tier-specific config, model catalog entries, shell policy, read policy, queue settings, and domain extensions from this file.

## Prompt Assembly

Prompt assembly is tier-aware:

- T1 gets constitution, agent, context, optional domain extensions, and conversation history.
- T2 gets constitution, context, optional domain extensions, and its task or history.
- T3 gets constitution, context, optional domain extensions, and the spawned task prompt.

Template variables are resolved at runtime. The context file is not a static prompt blob.

## Tier Behavior

### T1

T1 is operator-facing. It uses shell, can browse skill summaries, and can delegate work when the configured threshold says to do so.

### T2

T2 is reasoning-first. It uses `read_file` only, not shell. It produces structured plans, delegates external work to T3, and returns conclusions through the queue.

### T3

T3 is ephemeral execution. It receives the skills and model selected for the task, then runs the shell-backed work and exits.

## Model Routing

Model selection is config-driven and fail-closed:

1. explicit override, if enabled
2. route match from `models.routes`
3. default model from `models.default`
4. reject the spawn if nothing matches

The model catalog is a runtime policy layer, not an identity layer.

## Skills

Skills are local TOML files discovered from the configured skills directory.

- T1 and T2 receive summaries.
- Spawned T3 workers receive full skill instructions.
- Unknown skills fail closed.

## Handoff

- T2 writes structured conclusions back to the queue.
- T1 reads the conclusion as a normal inbound message.
- T1 can then communicate the result to the operator.
