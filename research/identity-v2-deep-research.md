# Deep Research: Identity System v2 for Autopoiesis-RS

## Context

Autopoiesis-rs is a lightweight Rust agent runtime. One binary, one tool (shell), messages in, actions out. The agent has a single shell tool and manages its own context through CLI commands, subscriptions, and topics (pure SQLite indexes).

The agent's identity is currently a v1 system: three files concatenated into the system prompt — `constitution.md` (laws of thought), `identity.md` (3 lines: name, voice, behavior), and `context.md` (template vars for model/cwd/tools). This is thin and functional but not what the system needs.

The planned v2 is a 4-layer identity system with self-modifiable persona dimensions, a CLI for identity mutation, and guard-enforced write protection on immutable layers. Some aspects are design hypotheses that need validation.

## The System You're Analyzing

### Constitution (exists, stable)
4 laws in strict hierarchy: Epistemic Fidelity > Chain of Command > Reversible Action > Contextual Continuity. Amendment clause: only the operator may modify, through direct file access. The agent cannot change this file. This is the "how to think" layer.

### Operator Policy (planned, does not exist yet)
Purpose, boundaries, permissions, budget ceilings, communication rules, tool constraints. Written by the operator. Agent cannot modify. This is the "what you're allowed to do" layer. Enforced by ShellSafety guard rules blocking writes to `identity/operator.md`.

### Agent Identity (exists, minimal)
Currently 3 lines. Planned: self-modifiable persona with structured dimensions + freeform sections. Agent shapes who it is over time. This is the "who you are" layer. Modifiable via CLI (`identity set voice "terse"`, `identity set autonomy high`).

### Context (exists, operational)
NOT part of the identity stack. Operational steering: reminders, behavioral notes, focus directives, active constraints. Positioned at the end of context, right before the latest message. Shapes HOW the agent thinks in this moment, not WHO it is.

### Instruction Positioning (open question)
Three options under consideration:
- A: All identity at top (before history) — maximizes KV cache stability
- B: All identity at bottom (after history) — matches training, recency bias helps compliance
- C: Split — constitution + operator at top (stable, cached), identity + context at bottom (persona refreshed by recency)

Hypothesis: C wins. Untested.

### Persona Dimensions (hypothesis, untested)
Structured traits the agent self-tunes in `identity.md`:
- `voice` (freeform) — tone, style
- `autonomy` (low/moderate/high) — act-first vs ask-first threshold
- `verbosity` (terse/normal/detailed) — default response length
- `risk_tolerance` (cautious/moderate/bold) — how much to attempt before escalating
- `focus` (freeform) — current working area

The idea: the agent starts with defaults. Over sessions, it adjusts based on experience. Operator never overrides decisions → autonomy rises. Agent makes a mistake → risk_tolerance drops for that domain. Self-tuning personality within operator-defined rails.

Storage format is TBD — structured data inside markdown is fragile. A TOML sidecar (`persona.toml`) may be cleaner for CLI parsing.

### Per-Tier Identity
- T1 and T2 (long-lived cognitive agents): full identity stack — constitution + operator + identity + context. Have persona, self-modification, working style. Same brain, two speeds.
- T3 (ephemeral executor): constitution.md only. First message = operator directives. No persona, no self-modification. Blind executor.

### The CLI
```
./autopoiesis identity get persona
./autopoiesis identity set voice "terse, technical"
./autopoiesis identity set autonomy high
./autopoiesis identity set verbosity terse
./autopoiesis identity set risk_tolerance cautious
./autopoiesis identity set focus "axum server"
```

### Write Protection
ShellSafety guard must block writes to `identity/constitution.md` and `identity/operator.md`. The agent runs shell and can write anywhere the process user can. File permission tricks alone are insufficient — enforcement must happen in the guard pipeline through command pattern matching.

## Research Questions

### 1. Layered Identity Architecture
The 4-layer split (constitution → operator → identity → context) seems clean in theory. But:
- Is there evidence from cognitive science, organizational design, or existing AI systems that layered identity/policy hierarchies produce better behavioral consistency than flat prompts?
- Are there systems (not just LLM agents — also robotics, organizational policy, military doctrine) that use similar layered authority structures? What can we learn from their failure modes?
- Is 4 layers the right number? Could constitution and operator merge (both are "things the agent can't change")? Could identity and context merge (both are "current state")?
- What's the risk of layer confusion — the model blending operator policy with self-identity, or treating context steering as constitutional law?

### 2. Self-Modifying Persona Dimensions
This is the most speculative part of the design. Critical questions:
- Do current frontier LLMs (GPT-5.x, Claude Opus 4.x, Gemini 3.x) actually change measurable behavior based on structured persona traits like `verbosity: terse` or `autonomy: high` in the system prompt?
- What does the prompt engineering literature say about structured vs. freeform persona descriptions? Are key-value traits more effective than natural language personality descriptions?
- If dimensions work, which ones matter? The proposed set (voice, autonomy, verbosity, risk_tolerance, focus) — are there better dimensions from personality psychology (Big Five?), cognitive science, or decision theory?
- Self-tuning creates a feedback loop: the agent observes outcomes → adjusts dimensions → behavior changes → new outcomes. Is this loop stable? Can it converge to degenerate configurations (e.g., always maxing autonomy, always minimizing risk)?
- What guardrails prevent dimension drift? Operator-set min/max bounds? Periodic resets? Rate limiting on changes?
- Storage: TOML sidecar vs structured markdown vs SQLite row vs JSON file. What's the right format for something the agent reads and writes programmatically but humans also inspect?

### 3. Instruction Positioning
The open question of where identity content goes in the context window:
- What does the research say about system prompt positioning for long-context models? Is "recency bias" real and measurable for instruction-following?
- For the split hypothesis (C: constitution at top, persona at bottom): does KV cache optimization actually benefit from stable prefixes in practice, or is this theoretical?
- Does the model architecture (transformer attention) treat position 0 differently from position N-1? Is there a measurable difference in compliance for instructions at different positions?
- Are there empirical benchmarks or ablation studies on instruction positioning for multi-turn agents?

### 4. Constitution Design
The 4 laws are inspired by Asimov but adapted for an LLM agent:
- Epistemic Fidelity as Law 1 (above Chain of Command) — is this the right hierarchy? What are the arguments for and against putting truth above obedience?
- The "Laws Conflict" section tells the agent to surface conflicts rather than silently resolve them. Is there evidence this works better than letting the model use judgment silently?
- The amendment clause ("only the operator may modify, through direct file access") is enforced by guards, not cryptography. How robust is pattern-matching-based write protection? What are the known bypass vectors?

### 5. T3 Identity Minimalism
T3 gets constitution only. No operator.md, no persona. First message = operator directives.
- Is constitution alone sufficient for safe T3 execution? Without operator boundaries, what prevents T3 from interpreting a malicious first message as legitimate operator directives?
- Should T3 get a minimal operator.md that says "you are an ephemeral executor, follow the instructions in the first message exactly"?
- How does this compare to other multi-agent systems' approach to worker/executor identity?

### 6. Identity Evolution Over Time
The agent is meant to persist indefinitely, accumulating history and adjusting its persona:
- How do long-running agents in production (AutoGPT descendants, Devin, Claude Opus persistent sessions) handle identity drift over time?
- Is there a "personality entropy" problem where accumulated self-modifications make the agent incoherent?
- Should there be an identity "snapshot and restore" mechanism? Periodic identity reviews by the operator?
- How does the sliding window (context trimming drops oldest turns) interact with identity that was calibrated based on those now-forgotten turns?

### 7. Alternative Approaches
What do other systems do differently?
- How does Claude's character/RLHF approach compare to explicit persona files?
- How does OpenAI's system prompt + custom instructions approach compare?
- Are there academic frameworks for agent identity that this design should reference?
- What can we learn from MCP (Model Context Protocol), OpenAI Agents SDK, LangGraph, or other agent frameworks about identity management?

## What I Want Back

1. A critical assessment of the 4-layer architecture — is it sound, over-engineered, or missing something?
2. An evidence-based recommendation on persona dimensions — keep, modify, or drop the hypothesis?
3. A concrete recommendation on instruction positioning (A, B, or C) backed by whatever evidence exists
4. Identification of the biggest risks in this identity design and how to mitigate them
5. Any alternative designs or prior art that we should seriously consider before implementing
6. A prioritized implementation recommendation — what to build first, what to defer, what to validate empirically before committing to

Be critical and opinionated. Challenge the design. The goal is to build something that actually works, not something that sounds good in a vision document.
