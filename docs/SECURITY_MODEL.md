# Security Model for `autopoiesis-rs` Knowledge Engine

This document reviews the planned Knowledge Engine (KE) security model against the current `aprs` runtime and the KE specs in `specs/knowledge-engine/00` through `08`, `knowledge-engine-requirements.md`, `architecture.md`, `roadmap.md`, `risks.md`, `principal.rs`, and the current guard pipeline (`mod.rs`, `budget.rs`, `shell_safety.rs`, `exfil_detector.rs`, `secret_redactor.rs`).

The core conclusion is simple:

The current runtime security model is still mostly a turn-local shell safety model. A persistent Knowledge Engine changes the problem into a state integrity problem. Once the agent can remember, summarize, subscribe, re-ingest, and reuse prior outputs across turns and sessions, prompt injection and exfiltration stop being only "bad input" problems. They become durable corruption, delayed-action, and cross-scope data flow problems.

The KE specs are already strong on epistemic provenance, bitemporality, and auditability. They are not yet sufficient for adversarial security. Confidence, source tracking, and truth scoring help answer "why do we believe this fact?" They do not answer "should this content be allowed to influence future agent behavior, cross a tenant boundary, or leave the machine?"

## 1. Threat model for agent knowledge systems

### What changes when the agent has persistent knowledge

A stateless chat agent can be compromised in one turn. A knowledge-bearing agent can be compromised for many future turns, many future users, and many future workflows.

Persistent knowledge creates six new attack classes:

1. **Delayed-action prompt injection**
   Untrusted content is ingested in turn `N`, stored as a note, summary, report section, subscription output, or claim explanation, and only influences actions in turn `N+K`.

2. **Trust laundering**
   Malicious or low-integrity content is transformed into a higher-status object:
   raw artifact -> chunk -> summary -> note -> subscription context -> "known fact".
   The more derived the object becomes, the easier it is for the runtime to forget that it began as attacker-controlled data.

3. **Cross-session contamination**
   A poisoned summary or claim can affect future sessions, future plans, future reports, and future topics. This is qualitatively different from a one-turn jailbreak.

4. **Cross-turn secret exfiltration**
   Sensitive data read in one turn can be persisted into memory, summaries, reports, or session history, then exfiltrated later when the immediate read is no longer visible to the guard that made the original decision.

5. **Self-poisoning feedback loops**
   The agent can write its own derived summaries back into the knowledge base. If those summaries are later trusted as evidence, the agent becomes able to reinforce its own mistakes or attacker-planted distortions.

6. **Analytic amplification**
   In the KE specs, truth scoring, community detection, link prediction, anomaly detection, PageRank confidence propagation, subscriptions, and context injection all fan knowledge forward. Poison in one source can become weight, ranking, community scope, alerts, and future context everywhere else.

### Primary assets

The assets that need protection are broader than "the prompt" or "the shell":

- Canonical artifacts, chunks, claims, and provenance chains
- Active values and relationship projections
- Trust embeddings, source priors, PageRank scores, community assignments, inferred links
- Session history, tool outputs, reports, and subscription-delivered context
- Source configs, checkpoints, approvals, and policy state
- Secrets and confidential claims
- Tenant, organization, and sensitivity boundaries
- Operator-authored control material: constitution, identity, standing approvals, gate policy

### Threat actors

- External content publishers: websites, feeds, email senders, PDFs, scraped pages, webhook senders
- Authenticated but low-trust users
- Compromised connectors or poisoned upstream APIs
- Malicious artifacts inside otherwise trusted sources
- A compromised or overly-permissive tool surface
- The agent itself, by storing unreviewed derived content as future truth
- Cross-tenant or cross-session users when identity and scope are weak

### Real attack paths relevant to KE

The relevant attack paths are not hypothetical:

- **Indirect prompt injection from retrieved content** was demonstrated by Greshake et al. against application-integrated LLMs, where malicious instructions embedded in retrieved data steer tool use and downstream actions.
- **Tool-output prompt injection** is now a benchmarked agent problem. InjecAgent showed meaningful attack success against tool-integrated agents, including data exfiltration cases.
- **Persistent memory poisoning** is now an explicit research category. MemoryGraft, A-MemGuard, and later memory-poisoning papers show that poisoned records in long-term memory can survive across sessions and reshape future behavior.
- **Real-world exfiltration through indirect prompt injection** has already happened. EchoLeak describes a production Microsoft 365 Copilot exploit that crossed trust boundaries and exfiltrated data from a crafted email.

### Why the KE design increases the blast radius

The KE specs intentionally add:

- typed ingest sources
- subscriptions
- reports
- context injection
- graph analytics
- trust embedding refinement
- source inheritance
- standing approvals for trusted ingest
- migration from markdown memory into canonical claims

All of that is useful. It also means a poisoned object can now:

- survive longer
- be retrieved more often
- gain higher apparent authority
- trigger more workflows
- contaminate more downstream derived objects

The security model therefore has to protect not only the original ingest path, but also every transformation and reuse path.

## 2. Content-level taint

### Current state

Current `aprs` taint is effectively message-level and boolean:

- `Principal` in `principal.rs` is a coarse label: `Operator`, `User`, `System`, `Agent`
- guard context in `mod.rs` carries `tainted: bool`
- `ShellSafety` only uses this bit to disable standing approvals on tainted turns

That is not enough for a persistent knowledge system.

It cannot represent:

- mixed provenance
- partial contamination
- secrecy vs integrity vs execution risk
- content derived from multiple sources
- model-generated summaries
- cross-turn persistence of sensitive content
- per-object scope or tenant restrictions
- the difference between "likely true as a fact" and "safe to trust as control input"

### Taint must become object-level, not message-level

Every persisted object needs taint metadata:

- artifacts
- chunks
- claim drafts
- committed claims
- evidence chains
- summaries
- reports
- subscription payloads
- query results cached for reuse
- tool outputs stored on disk
- model-generated memories or notes

### Taint needs multiple dimensions

A single `tainted: bool` should be replaced by at least four orthogonal dimensions.

#### 1. Integrity taint

Who originated this content and how directly?

- `operator_attested`
- `approved_human_attestation`
- `anchored_external_source`
- `external_untrusted`
- `user_untrusted`
- `tool_untrusted`
- `model_derived`
- `mixed`
- `quarantined`

#### 2. Sensitivity taint

What confidentiality obligations attach to it?

- `public`
- `internal`
- `confidential`
- `restricted`
- `secret_derived`

#### 3. Instructionality taint

Could this content act as control input if naively replayed?

- `inert_data`
- `prompt_like`
- `imperative_text`
- `code_or_shell`
- `tool_recipe`
- `policy_like`

This matters because a Reuters article and a shell transcript are both "text", but they are not the same security object.

#### 4. Scope taint

Where may this object flow?

- tenant
- organization
- session
- topic
- source
- report audience
- approval requirement

### Taint propagation rules

The core rule is monotonicity:

Derived objects inherit the union of upstream taints. Summarization never clears taint. Confidence never clears taint. Truth resolution never clears taint. Only an explicit review or declassification action may reduce taint.

Required propagation rules:

1. **Artifact -> chunk**
   Chunks inherit source integrity, sensitivity, and instructionality labels from the artifact and parser output.

2. **Chunk -> claim draft**
   Claim drafts inherit the chunk taint plus `model_derived` if an LLM extractor participated.

3. **Claim draft -> claim**
   A committed claim keeps source/integrity taint even if truth scoring later marks it highly probable.

4. **Many-to-one summarization**
   A summary inherits the union of all contributing taints and adds `model_derived`.

5. **Query answer / report / subscription output**
   These inherit the max sensitivity and union integrity taint of all supporting claims and evidence. They are not "safe" just because they were formatted by the platform.

6. **Secret flow**
   If a turn reads a secret, every object derived from that read gets `secret_derived` until reviewed or deleted. This has to survive across turns.

7. **Cross-session injection**
   Context pulled from memory, reports, subscriptions, or saved tool results must keep its taint when reintroduced into future prompts.

### Special rule for model-generated summaries

For KE, model-written summaries are the highest-risk object class.

They should never directly become trusted claims.

They should instead enter as:

- `DerivedSummary`
- `CandidateClaimBatch`
- `ReviewRequiredMemory`

and remain clearly separated from:

- raw artifacts
- human attestations
- anchored claims
- approved operator notes

If the agent writes "summary" objects back into knowledge without this separation, memory poisoning becomes guaranteed over time.

### Cross-session context rule

Any future context assembly should intersect permissions with taint:

- `external_untrusted` or `prompt_like` content may be quoted as evidence, but not injected as freeform high-authority context
- `secret_derived` content may not be exposed to network tools, report export, or other tenants without declassification
- `model_derived + mixed` summaries should not unlock standing approvals

In other words:

Persistent knowledge is not just retrieval data. It is part of the control surface unless the runtime explicitly prevents that.

## 3. Provenance tracking

### What the KE specs already get right

The KE specs are strong on epistemic provenance:

- immutable artifacts
- evidence chains
- provenance-chain DAGs
- source entities
- bitemporal claim versions
- confidence scores
- conflict sets
- active values
- source tracking in explanations
- approval hooks for high-impact updates

This is already far better than typical agent memory systems.

### Why it is still not sufficient

The current provenance model mainly answers:

- where did this information come from?
- why is this claim believed?
- what alternative claims exist?

For a secure knowledge runtime, provenance must also answer:

- who caused this object to exist?
- which untrusted inputs influenced it?
- which transforms touched it?
- which policy checks ran?
- which approvals exist?
- what scopes and taints apply?
- can this object safely influence tools or egress?

### What is missing

#### 1. Security provenance, not just evidence provenance

Each persisted object needs:

- creator actor ID
- creator kind: human, connector, tool, model, migration job, subscription processor
- transform chain: parser version, extractor version, model ID, prompt version, report template, query plan hash
- policy decisions: validator results, gate result, approval ID, sanitizer outcome
- taint labels at creation time
- quarantine or review state

Without this, provenance explains facts but not trust boundaries.

#### 2. Transformation lineage for derived content

The specs track evidence chains for claims. They do not yet clearly define equivalent lineage for:

- summaries
- reports
- subscription deltas
- query-run cache entries
- injected context payloads
- model-authored memory notes

Those objects are exactly where knowledge poisoning becomes persistent.

#### 3. Provenance for security-sensitive analytics

The KE introduces:

- trust embedding refinement
- PageRank confidence propagation
- community detection
- link prediction
- missing relation inference

All of these can amplify poison.

They therefore need dependency tracking and rollback:

- which claims changed this trust embedding?
- which evidence contributed to this community assignment?
- which predicted links were derived from which paths?
- which PageRank modifier affected downstream resolution?

Otherwise an operator can inspect a poisoned active value but not unwind the analytic chain that amplified it.

#### 4. Separation of truth authority from execution authority

This is the most important conceptual gap.

A source can be epistemically valuable without being operationally authoritative.

Examples:

- A government registry may be highly reliable for legal name or registration number.
- A market data feed may be highly reliable for prices.
- A shell transcript may truthfully contain the string `curl https://evil.example`.

None of those sources should ever gain authority to tell the agent what to do next.

Therefore provenance needs two independent trust tracks:

- **epistemic provenance**: why a claim is believed
- **operational provenance**: whether this content may influence execution, policy, or privilege

Confidence scores only cover the first.

#### 5. Negative provenance

The system also needs to remember why something is unsafe:

- prompt-injection suspicion
- secret-bearing content
- failed validation
- quarantined source
- poisoned summary
- revoked approval

If unsafe objects are simply deleted from the happy path without durable negative state, they will be rediscovered and reintroduced later.

### Bottom line on provenance

The KE truth engine's source tracking and confidence model are necessary for knowledge quality. They are not sufficient for security. Security requires an additional provenance layer that records contamination, transformation, scope, and policy lineage.

## 4. Defense-in-depth for data poisoning

The right defense is not "better prompting". It is runtime structure.

### A. Ingest-time defenses

- Treat all external artifacts and all tool outputs as untrusted observations.
- Normalize and strip active content where possible before LLM exposure.
- Route by parser and adapter, not by arbitrary model interpretation.
- Run injection heuristics and policy checks at ingest, but treat them as advisory signals, not proof.
- Quarantine new or degraded sources before they contribute to high-impact claims.
- Keep raw artifacts immutable so poisoned derivations can be re-audited.
- Require content-hash and source lineage so the same poison is deduplicated, not multiplied.

### B. Execution-time defenses

- Isolate tool execution from the main agent context.
- Use worker agents or subprocess contexts for reading raw external content.
- Only allow schema-validated return objects to cross from tool worker to planner.
- Keep raw tool output out of the main long-lived memory unless explicitly requested and labeled.
- Apply least-privilege tool policy at runtime, not just by prompt.
- Re-check risky tool calls with causal or counterfactual diagnostics when they appear to be driven by untrusted observations.

Research direction:

- AgentSys shows the value of hierarchical memory isolation and schema-validated tool returns.
- Progent shows the value of explicit programmable privilege control.
- AttriGuard and AgentSentry show the value of counterfactual checks at tool-return boundaries.

### C. Write-path defenses

- Make `submit_claims()` the only canonical write path.
- Separate raw observations from candidate claims from active beliefs.
- Require evidence-linked writes or signed human attestation.
- Forbid freeform memory or summaries from writing directly into trusted claim tables.
- Reject or quarantine any derived summary containing imperative, policy-like, or tool-like language.
- Require multi-source or anchor support before low-trust content can affect high-impact properties.
- Rate-limit trust embedding refinement and support rollback.

### D. Retrieval and context-injection defenses

- Inject structured answer objects, not raw snippets, by default.
- Quote untrusted evidence as data, not instructions.
- Preserve taint and sensitivity metadata in every explanation bundle, report section, and subscription payload.
- Exclude `prompt_like`, `policy_like`, and `secret_derived` content from autonomous execution contexts unless explicitly approved.
- Rank or filter retrieval results by both epistemic confidence and security posture.
- Treat link predictions, anomalies, and inferred relations as hypothesis-only outputs that never silently upgrade to fact.

### E. Egress defenses

- Run egress checks on the full dependency set of the outbound object, not only the current turn text.
- Deny or require approval when outbound payloads depend on:
  `secret_derived`, `restricted`, `tenant_isolated`, or `quarantined` inputs.
- Disable standing approvals for mixed-taint or model-derived outputs.
- Log destination, payload class, approval state, and upstream object IDs for every external call.
- Bind declassification to explicit human approval for sensitive KE outputs.

### F. Operational defenses

- Maintain quarantine, retraction, and rollback paths for sources, claims, summaries, and analytic outputs.
- Keep tamper-evident audit logs for approvals, manual overrides, trust updates, and ontology changes.
- Monitor for poisoning indicators:
  shallow provenance, sudden source convergence, trust-embedding drift, anomalous contradiction rates, repeated imperative language in summaries, and unusual cross-tenant references.
- Test with agent-specific attack suites, not generic LLM evals only.

### What not to rely on

Do not rely on any of the following as primary controls:

- prompt wording alone
- source trust alone
- confidence scores alone
- redaction alone
- one-shot prompt-injection classifiers alone
- batch-local exfil detectors alone

All of these are useful. None is a sufficient boundary for persistent knowledge.

## 5. Gaps in current `aprs` security beyond `CRITICAL_REVIEW.md`

`CRITICAL_REVIEW.md` already found several blocking issues. For KE, additional gaps matter even if the current shell agent were otherwise fixed.

### 1. Taint is too coarse for knowledge persistence

Current taint is effectively `Principal` plus a boolean. That is not enough to secure:

- mixed-source summaries
- knowledge writes
- reports
- subscriptions
- context injection
- future-session reuse
- secret-derived memory

### 2. Guard coverage stops before the planned KE surfaces

Current guards protect:

- inbound messages
- tool calls
- outbound text deltas

They do not yet protect:

- knowledge writes
- summary generation
- report generation
- subscription materialization
- context injection
- query cache reuse
- migration from markdown memory into claims

For KE, those are first-class attack surfaces.

### 3. No persistent secret-flow tracking

`CRITICAL_REVIEW.md` already notes cross-turn exfil risk. KE makes this worse.

There is still no object-level mechanism that marks a report, summary, or claim explanation as derived from a secret read in an earlier turn. Without that, later turns can leak past secrets while appearing clean.

### 4. No security boundary between observations and control

Today the system is still one-agent, one-context, one-shell-tool. KE adds more derived context sources, but the design does not yet define a hard separation between:

- operator control material
- user requests
- tool observations
- retrieved knowledge
- model-written summaries

Without that separation, any stored knowledge can eventually become control input.

### 5. No security model for model-authored memory

The KE migration plan explicitly says markdown memory writes can be mirrored into the claim ledger. There is not yet a policy saying when an agent-authored note is:

- merely a note
- a candidate claim
- a trusted manual attestation
- disallowed from becoming canonical knowledge

This is the central memory-poisoning gap.

### 6. No poisoning controls on analytic feedback loops

The truth engine and KE specs allow:

- trust embedding refinement
- PageRank confidence propagation
- community-aware scoping
- link prediction enrichment
- anomaly-driven workflows

Those are all amplification channels.

There is no current design for:

- rollback of poisoned trust updates
- "hypothesis only" containment of predictions
- preventing low-trust content from shaping future retrieval scopes
- limiting analytic influence from quarantined or newly ingested sources

### 7. Provenance is strong for claims, weak for derived operational objects

The specs define claim/evidence provenance well. They are weaker on provenance for:

- summaries
- reports
- subscription deltas
- injected context packages
- model-generated lessons or memories

These are the objects most likely to carry prompt injection across turns.

### 8. No per-caller identity model strong enough for shared knowledge

Current runtime auth is essentially operator key vs user key. KE requirements mention tenant and sensitivity scopes, but the present runtime does not provide the actor identity model needed to enforce them safely across shared memory, subscriptions, and exports.

### 9. No tamper-evident audit path for security-critical knowledge mutations

KE requires operator overrides, source trust changes, ontology changes, and high-impact claim approvals. There is not yet a concrete tamper-evident audit design tying these to later retrieval, export, and rollback.

### 10. Shell safety is still heuristic even before KE adds more routes to it

Current `shell_safety.rs` uses raw string globbing, and `exfil_detector.rs` is a simple batch heuristic. Even if those improve, shell-based self-management remains a dangerous control path for subscriptions, topics, identity, and future knowledge operations unless KE-native typed tools replace shell mediation for those tasks.

## 6. Recommended security architecture

The right architecture is a four-plane model with explicit object metadata and monotonic taint.

### Plane 1: Control plane

Contains:

- constitution / operator policy
- standing approvals
- gate rules
- tool privilege policy
- identity and tenant policy

Rules:

- only operator or approved system components may write here
- external or model-derived content never flows into this plane
- this plane can influence other planes, but not vice versa

### Plane 2: Observation plane

Contains:

- raw artifacts
- raw tool outputs
- parsed chunks
- external feeds
- webhook bodies
- scraped pages
- transcripts

Rules:

- everything here is untrusted by default
- immutable storage
- taint attached at object creation
- safe to inspect, not safe to treat as instructions

### Plane 3: Knowledge plane

Contains:

- candidate claims
- evidence chains
- provenance DAGs
- active values
- reports
- subscription outputs
- analytic outputs

Rules:

- all writes go through typed, validated APIs
- object lineage is mandatory
- model-derived objects are distinct from anchored or attested objects
- predictions and inferences are never silently promoted into facts

### Plane 4: Execution plane

Contains:

- planner context
- tool workers
- egress requests
- approval interactions

Rules:

- worker isolation for raw tool interactions
- only schema-validated payloads return to the main planner
- outbound permissions are computed from object dependencies and taint, not only current text

## Required object schema additions

Every persisted knowledge-adjacent object should carry at least:

- object ID and object type
- creator actor ID and creator kind
- upstream object IDs
- integrity taint set
- sensitivity label
- instructionality label
- tenant / org / session scope
- transform metadata: parser, extractor, model, prompt, template, query hash
- validation results
- approval ID, if any
- quarantine / review / retraction status
- created-at and superseded-at timestamps

## Required write-state machine

Recommended write lifecycle:

1. `observed`
   Raw object exists, immutable, untrusted.

2. `extracted`
   Candidate structured objects exist with inherited taint.

3. `candidate`
   May participate in truth scoring, but not in privileged execution context.

4. `validated`
   Passed structural, provenance, secrecy, and anti-instruction checks.

5. `approved`
   Human or policy approval exists for high-impact use.

6. `active`
   Eligible for normal retrieval and low-risk context injection.

7. `quarantined` / `retracted`
   Removed from normal retrieval, kept for audit and rollback.

This state machine should apply not only to claims, but also to summaries, reports, and subscription payloads.

## Non-negotiable security invariants

1. **No untrusted content may directly increase tool privilege.**
   Source reliability can affect fact resolution. It must never grant execution authority.

2. **No model-derived summary may become trusted knowledge without evidence-linked validation.**

3. **No secret-derived object may leave the machine or cross tenant boundaries without declassification.**

4. **No hypothesis output may silently become fact.**
   Link predictions, inferences, anomaly narratives, and report summaries remain derived objects unless separately validated.

5. **No context injection may erase provenance.**
   Every injected object keeps source, taint, and scope metadata.

6. **No approval may apply to mixed or contaminated objects by inheritance.**
   Standing approvals must be bound to object classes and taint predicates, not just tool names.

## Concrete design changes for `aprs`

### 1. Replace `Principal`-only taint with a taint record

`Principal` can remain as caller identity metadata. It should not be the knowledge taint model.

Add a persistent taint record used across session, knowledge, and output objects.

### 2. Introduce a knowledge-safe return channel

For shell and future KE tools:

- raw output stays in observation storage
- the main agent receives only typed fields or explicitly quoted evidence
- untrusted text never re-enters main context as implicit instructions

This follows the same direction as AgentSys and related runtime defenses.

### 3. Add a write firewall in front of KE persistence

Before anything enters canonical knowledge:

- enforce provenance presence
- classify taint and sensitivity
- detect imperative or policy-like content
- run secret scans
- attach transform lineage
- require approval where policy says so

### 4. Split epistemic trust from operational trust

Keep the KE truth engine for deciding what is likely true.

Add a separate operational trust policy for deciding:

- whether content may influence action planning
- whether it may unlock standing approvals
- whether it may be injected into future execution contexts
- whether it may leave the machine

No external source, however accurate, should gain operational trust by default.

### 5. Treat analytics as tainted derived views

Community assignments, predicted links, anomaly narratives, and PageRank modifiers should carry:

- derivation lineage
- contributing object IDs
- influence caps
- rollback support
- "derived, non-authoritative" status

They should help retrieval and review, not silently rewrite reality.

### 6. Add quarantine and rollback as first-class KE operations

Operators need to be able to quarantine:

- a source
- an artifact family
- a summary class
- a connector
- a trust-embedding update
- a predicted-link batch

and then recompute downstream affected objects.

### 7. Make context injection taint-aware

The current spec is already moving in the right direction by preferring structured answer data over raw snippets.

That should become strict policy:

- inject facts as typed envelopes with citations
- inject contradictions as structured review items
- inject raw evidence only on explicit request and as quoted data
- never inject secret-derived or prompt-like memory into autonomous execution context

## Final assessment

The KE specs are already unusually mature on provenance and truth modeling. The missing work is the adversarial side:

- content-level taint
- operational provenance
- memory poisoning resistance
- cross-turn secret-flow control
- separation of knowledge authority from execution authority
- rollback for poisoned analytic state

If `aprs` adds persistent knowledge without those controls, the system will become better at remembering attacks than at defending against them.

If it adds them, the Knowledge Engine can be made substantially safer than the current shell-centric runtime because the KE already has the right structural foundations: immutable artifacts, typed claims, provenance DAGs, conflict sets, approvals, and explicit write APIs.

## References

- Kai Greshake et al., "Not what you've signed up for: Compromising Real-World LLM-Integrated Applications with Indirect Prompt Injection" (2023), https://arxiv.org/abs/2302.12173
- Jingwei Yi et al., "Benchmarking and Defending Against Indirect Prompt Injection Attacks on Large Language Models" (2023), https://arxiv.org/abs/2312.14197
- Qiusi Zhan et al., "InjecAgent: Benchmarking Indirect Prompt Injections in Tool-Integrated Large Language Model Agents" (2024), https://arxiv.org/abs/2403.02691
- Jason Wei et al., "The Instruction Hierarchy: Training LLMs to Prioritize Privileged Instructions" (2024), https://arxiv.org/abs/2404.13208
- Tianneng Shi et al., "Progent: Programmable Privilege Control for LLM Agents" (2025), https://arxiv.org/abs/2504.11703
- Pavan Reddy and Aditya Sanjay Gujral, "EchoLeak: The First Real-World Zero-Click Prompt Injection Exploit in a Production LLM System" (2025), https://arxiv.org/abs/2509.10540
- Saksham Sahai Srivastava and Haoyu He, "MemoryGraft: Persistent Compromise of LLM Agents via Poisoned Experience Retrieval" (2025), https://arxiv.org/abs/2512.16962
- Qianshan Wei et al., "A-MemGuard: A Proactive Defense Framework for LLM-Based Agent Memory" (2025), https://arxiv.org/abs/2510.02373
- Balachandra Devarangadi Sunil et al., "Memory Poisoning Attack and Defense on Memory Based LLM-Agents" (2026), https://arxiv.org/abs/2601.05504
- Ruoyao Wen et al., "AgentSys: Secure and Dynamic LLM Agents Through Explicit Hierarchical Memory Management" (2026), https://arxiv.org/abs/2602.07398
- Yu He et al., "AttriGuard: Defeating Indirect Prompt Injection in LLM Agents via Causal Attribution of Tool Invocations" (2026), https://arxiv.org/abs/2603.10749
- Tian Zhang et al., "AgentSentry: Mitigating Indirect Prompt Injection in LLM Agents via Temporal Causal Diagnostics and Context Purification" (2026), https://arxiv.org/abs/2602.22724
- OWASP GenAI Security Project, "LLM01:2025 Prompt Injection", https://genai.owasp.org/llmrisk/llm01-prompt-injection/
- OWASP GenAI Security Project, "Agentic AI - Threats and Mitigations", https://genai.owasp.org/resource/agentic-ai-threats-and-mitigations/
