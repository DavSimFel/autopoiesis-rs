# Identity System v2 — Spec

> **Status:** Design complete. Replaces the current 3-file flat model.
> **Date:** 2026-03-24
> **Origin:** Adversarial debate (Silas × Codex, 4 rounds). Files: `/tmp/identity-debate/`

---

## Summary

One brain, two speeds, ephemeral hands.

- **T1** (fast model, low latency): talks to the operator. Has personality.
- **T2** (reasoning model, max depth): deep analysis. No personality. Produces structured artifacts.
- **T3** (task-appropriate model): executes. No personality. Model chosen from catalog.

T1 and T2 are the **same identity** — System 1 and System 2 of one mind. Multiple T2 instances run in parallel as independent deep-thought threads. T3s are tools, not the brain.

---

## Identity Stack

Three files. Constitution and context always load. agent.md loads for T1 only.

| Layer | File | Owner | Mutability | Loads for |
|-------|------|-------|-----------|-----------|
| **Policy** | `constitution.md` | Operator | Immutable at runtime | All tiers |
| **Character** | `agent.md` | Operator | Refreshed on start | T1 only |
| **Context** | `context.md` | System | Rendered per session | All tiers |

### constitution.md

The laws. What every agent instance MUST and MUST NOT do.

- **One file, one location.** Not copied per-agent.
- **Read at assembly time** from `identity-templates/constitution.md`.
- **Protected paths guard** hard-denies any modification attempt via shell.
- **Content:** Laws, hierarchy, conflict resolution, amendment rules.
- **Read-only enforcement** at process level, not just prompt-level policy.

### agent.md

The character. Operator's blueprint for how T1 communicates.

- **T1 only.** T2 and T3 do not load this file.
- **Per brain, not per tier.** One agent.md per named brain (e.g., Silas).
- **Refreshed from template every start.** Operator can change character; next restart applies.
- **Agent cannot modify.** Protected paths guard denies writes.
- **Constrained section format.** Not free-form prose, not rigid schema.

Required sections:
```markdown
# Character

## Voice
[How the agent communicates — sentence patterns, vocabulary, what it avoids]

## Worldview
[Opinions, instincts, how it thinks about its domain]

## Defaults
[How it approaches work — initiative, when to ask vs act, error handling]

## Edges
[Where it pushes back, discomfort zones, internal tensions]
```

### context.md

Operational reality. Template-rendered, never persisted.

- **Always present** for all tiers.
- **Rendered in memory** with `{{model}}`, `{{cwd}}`, `{{tools}}`, etc.
- **Template inputs validated** — variables cannot smuggle instructions.
- **Includes domain pack** when a domain is specified at spawn time.

---

## Prompt Assembly

### T1 (personality mode)
```
constitution.md → agent.md → context.md → [domain pack if active] → [message history] → [current message]
```

### T2 (reasoning mode)
```
constitution.md → context.md → [domain pack if active] → [task] → [message history]
```

### T3 (execution mode)
```
constitution.md → context.md → [task prompt]
```

No XML tags. No special delimiters. Files concatenated with `\n\n` separators (current behavior). The model distinguishes sections by content, not markup.

---

## Domain Context

Domain knowledge is a **context extension**, not an identity layer.

- Lives in `identity-templates/domains/{name}.md`
- Loaded alongside context.md when a domain is specified
- Examples: `fitness.md` (ExcitingFit + VIVO), `immobilien.md` (real estate), `autoshine.md`
- Same brain + different domain = same personality, different working knowledge
- T3s can also load domain packs when spawned for domain-specific tasks

---

## One Brain, Two Speeds

T1 and T2 share the same agent identity. The model is the mode switch:
- Fast model → naturally concise, conversational (T1 behavior)
- Reasoning model → naturally deeper, structured (T2 behavior)

### Isolation
- Each T2 instance gets its own session and reasoning context.
- Parallel T2s are **isolated by default** — no shared scratchpad, no groupthink.
- T2 instances do not see each other's work unless explicitly coordinated.

### Handoff
- T2 finishes → writes a structured conclusion to the message queue.
- T1 reads the conclusion as a normal inbound message.
- Handoff is explicit, not implicit memory transfer.
- Conclusion format: decision, evidence, confidence, open risks, next action.

---

## T3 Model Repository

Lives in `agents.toml` under `[models]`. Runtime policy, not identity.

### Catalog
Each model entry:
```toml
[models.catalog.gpt5_mini]
provider = "openai"
model = "gpt-5.4-mini"
caps = ["fast", "cheap", "reasoning", "multilingual"]
context_window = 128000
cost_tier = "cheap"
cost_unit = 1
enabled = true
```

### Routes
Task kinds mapped to route requirements + preference order:
```toml
[models.routes.code_review]
requires = ["code_review"]
prefer = ["gpt5_codex", "gpt5_mini"]
```

### Selection (fail-closed)
1. Explicit model override → use if enabled and present in the catalog
2. Route match by `requires` containing the task kind → first valid in preference list
3. Default model → only if enabled and present in the catalog
4. Nothing → reject spawn (`model_unavailable` or `budget_exceeded`)

### Rules
- **One model per T3 lifetime.** No mid-task switching.
- **Budget checked before spawn.** Session/day ceilings are enforced before spawn.
- **Fallback degrades, never upgrades** cost class without explicit allowance.

---

## agents.toml

```toml
# === Brain ===
[agents.silas]
identity = "silas"                    # → identity-templates/agents/silas/agent.md

[agents.silas.t1]
model = "gpt-5.4-mini"               # fast, cheap
reasoning = "medium"

[agents.silas.t2]
model = "o3"                          # or heavy reasoner
reasoning = "xhigh"

# === T3 defaults ===
[agents.default]
tier = "t3"

# === Model catalog ===
[models]
default = "gpt5_mini"

[models.catalog.gpt5_mini]
provider = "openai"
model = "gpt-5.4-mini"
caps = ["fast", "cheap", "reasoning", "multilingual"]
context_window = 128000
cost_tier = "cheap"
cost_unit = 1
enabled = true

[models.catalog.gpt5_codex]
provider = "openai"
model = "gpt-5-codex"
caps = ["code", "reasoning"]
context_window = 200000
cost_tier = "medium"
cost_unit = 3
enabled = true

# ... more models ...

[models.routes.code_review]
requires = ["code"]
prefer = ["gpt5_codex", "gpt5_mini"]

[models.routes.vision]
requires = ["vision"]
prefer = ["gpt4o_mini", "gpt4_1"]

# === Domain packs ===
[domains.fitness]
context_extend = "identity-templates/domains/fitness.md"

[domains.immobilien]
context_extend = "identity-templates/domains/immobilien.md"
```

---

## File Layout

```
identity-templates/                   # git-tracked, operator-authored
├── constitution.md                   # global — one for all agents
├── agents/
│   └── silas/
│       └── agent.md                  # T1 personality
├── domains/
│   ├── fitness.md                    # ExcitingFit + VIVO knowledge
│   ├── immobilien.md                 # real estate knowledge
│   └── autoshine.md                  # detailing business knowledge
└── context.md                        # template with {{vars}}
```

No per-agent runtime copies of constitution or agent.md. Read directly from templates at assembly time.

---

## Security

| File | Agent can read? | Agent can modify? | Enforcement |
|------|----------------|-------------------|-------------|
| `constitution.md` | Yes | **No** | ProtectedPaths guard + read-only mount |
| `agent.md` | Yes (T1) | **No** | ProtectedPaths guard + read-only mount |
| `context.md` | Yes | No | System-generated, no file on disk |
| `domains/*.md` | Yes | **No** | ProtectedPaths guard |

All identity-templates/ paths added to the `ProtectedPaths` catalog in `gate/secret_patterns.rs`.

---

## What Changes in Code

### `src/identity.rs`
Current: configurable file list based on tier. T1 loads `[constitution, agent, context]`. T2/T3 load `[constitution, context]`.

### `src/config.rs`
Current: `[agents.{name}]` with tier-specific subtables, `[models]` catalog/routes, `[domains]`.

### `src/context.rs`
Current: `Identity` takes a file list, not a directory. T1 uses `[constitution, agent, context]`. T2/T3 use `[constitution, context]`. Selected domain packs are appended to the list when configured.

### `agents.toml`
Current: `[agents.{name}]` with `.t1`/`.t2` subtables, `[models]`, and `[domains] selected=[...]` with per-pack `context_extend` entries. No legacy `[agent]` fallback.

---

## What Is Explicitly Out of Scope

- **Memory / learning** — separate system, future design
- **Self-modifiable identity** — not in v1 (security risk without provenance infrastructure)
- **Multi-provider routing** — models catalog handles it, but only OpenAI provider exists today
- **Agent spawning** — T2/T3 spawn mechanism is multi-agent workspace design, not identity

---

## Implementation Order

1. **Add identity-templates/ to ProtectedPaths** — guard enforcement for constitution + agent.md
2. **Refactor identity.rs** — configurable file list instead of hardcoded triple
3. **Write Silas agent.md** — first real T1 character file
4. **Extend config.rs** — parse `[agents.{name}]` with tier subtables, strict no-legacy mode
5. **Add models catalog/routes to config** — `[models]` section parsing
6. **Domain pack loading** — context extension mechanism
7. **Persona stability tests** — fixed scenario battery in existing test infrastructure
8. **Update docs** — architecture.md, vision.md, roadmap.md

---

## Acceptance Criteria

- [ ] Constitution + agent.md modification denied by guard pipeline (test)
- [ ] T1 loads constitution + agent.md + context; T2/T3 load constitution + context; selected domain packs are appended (test)
- [ ] Same agent.md produces consistent persona across fresh sessions (eval)
- [ ] Adversarial injection in history does not override persona (eval)
- [ ] Legacy `[agent]` config is rejected when `[agents]` is present or absent (test)
- [ ] Model catalog resolves correct model for task kind (test)
- [ ] Budget check prevents spawn when cost exceeds limit (test)
- [ ] Domain pack extends context without affecting identity (test)
