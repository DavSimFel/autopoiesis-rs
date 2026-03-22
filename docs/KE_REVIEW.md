# KE Review for aprs

The Python KE specs contain one strong core and a large amount of speculative architecture around it. The strong core is: immutable artifacts, append-only claims, explicit provenance, conflict sets, abstention, and gated promotion of high-impact facts. That core fits `aprs`.

Most of the rest does not. The v2 specs drifted from a practical knowledge layer into a graph analytics platform with learned trust embeddings, provenance DAG math, specialized storage adapters, ontology auto-discovery, link prediction, community detection, anomaly detection, prediction markets, and 100M-scale benchmarks. That is not "simple primitives + emergence." It is a second product.

The requirements document is narrower and better than the expanded 00-08 design. `aprs` should implement to the durable requirements, not to the speculative v2 machinery.

## 1. What translates directly

These KE ideas map cleanly to `aprs` as-is or with only mechanical changes:

- **Artifacts as immutable evidence.** `aprs` should store every fetched file, API payload, shell output, and user-provided document as an immutable artifact with content hash, source metadata, fetch time, and taint labels.
- **Claims as the canonical unit of knowledge.** The core move from "documents/snippets" to "atomic claims with provenance" is correct and should carry over directly.
- **Conflict sets plus active values.** The `(entity, property[, scope]) -> multiple claims -> one current best value or abstain` model is the right abstraction for a runtime that needs to answer "what do we currently believe?" without deleting contradictory evidence.
- **Bitemporal history.** `observed_at`, `valid_from/valid_to`, and `system_from/system_to` fit naturally in SQLite and are worth keeping. They matter for replay, audits, and "what changed?" queries.
- **Derived state is rebuildable.** Active values, search indexes, summaries, and subscriptions should all be projections from the artifact/claim ledger, not primary mutable state.
- **Mandatory provenance.** The requirement that no fact exists without source evidence or signed human attestation should be kept exactly.
- **Deduplication across sources.** Hash-based and metadata-based dedupe fit `aprs` well, especially once subscriptions exist and the same URL/file may arrive from multiple feeds.
- **Resumable jobs and checkpoints.** This maps directly onto the existing SQLite queue and planned subscription/checkpoint model.
- **High-impact mutations go through approvals.** This is already culturally aligned with `aprs` because the runtime already has a guard pipeline and approval model.
- **Knowledge-aware subscriptions.** "Notify when confidence changed / contradiction created / source degraded" is a strong fit for the planned subscription system.
- **Fact vs forecast vs scenario separation.** This is a sound boundary even if forecasting stays out of scope for a while.

## 2. What needs adaptation

These ideas are useful, but the Python form does not fit the Rust runtime and should be recast around `aprs` primitives.

- **Topics as sources.** In Python, topics were the ingestion control plane. In `aprs`, subscriptions should be the control plane and topics should remain optional grouping. The KE equivalent of a source declaration should be a subscription row with `fetch_cmd`, `schedule`, `checkpoint`, `parse_mode`, `trust_class`, and `taint_policy`, not YAML frontmatter.
- **Graph-first retrieval.** The concept is fine; the implementation assumption is wrong. `aprs` should start with a SQLite claim ledger plus bounded joins and relation tables, not a graph database and not a full graph query planner. If graph-style traversal becomes necessary later, build it on top of the ledger.
- **Source model.** Python assumed connectors and source objects. `aprs` has shell as the universal tool. The right abstraction is "subscription + shell pipeline + provenance envelope," not a connector class hierarchy.
- **Ingest/adapters.** The useful part is "different data types need different extraction logic." The wrong part is a fleet of independently deployed adapters and specialized stores. In `aprs`, use shell-normalization pipelines first and add specialized tables only when a real workload demands them.
- **Entity resolution.** Keep aliasing, canonical IDs, and merge history, but make it conservative and review-heavy. The spec's aggressive auto-resolution and weighted neighborhood scoring assume much richer data and more operator tooling than `aprs` currently has.
- **Ontology.** Keep a small typed property registry and version it manually. Drop automatic ontology discovery for now. `aprs` does not yet need a self-evolving schema engine.
- **SDK-centric integration.** Python assumed a broad internal SDK shared across T1/T2/T3. `aprs` should expose a smaller CLI/runtime surface first: inspect artifact, inspect claim, lookup entity, list conflicts, list subscriptions, replay provenance.
- **Event model.** Use the existing SQLite message queue and session persistence. Do not create a parallel event bus for KE.
- **Scale targets.** The Wikipedia-scale and 100M-entity goals distort the design. `aprs` should optimize for local operational knowledge, subscription feeds, and bounded workspace-scale corpora first.

## 3. What to drop

These features should not be carried into `aprs` v1. Some are over-engineered, some are speculative, and some are Python-specific residue.

- **Graph database ADR and graph-native canonical store.** `aprs` already uses SQLite successfully. Adding Neo4j/Arango/Postgres before a working KE exists is upside-down.
- **The "graph analytics platform" framing.** Leiden, link prediction, anomaly detection, PageRank confidence propagation, and missing-relation inference are not prerequisites for a useful knowledge layer.
- **Learned 128-dimensional trust embeddings.** This is the clearest example of speculative complexity. It is expensive, opaque, hard to calibrate, and unnecessary for a first useful system.
- **Post-resolution embedding refinement loops.** This creates hidden mutable trust state that will be difficult to explain, test, or debug.
- **Full provenance DAGs plus LCA-based corroboration math.** Provenance is essential; full DAG reasoning is not. Start with linear provenance plus source-lineage grouping. Revisit only if syndicated-source problems become common and costly.
- **Pretending the current truth algorithm is Bayesian.** The v2.1 algorithm is mostly a heuristic weighted scorer wrapped in Bayesian language. `aprs` should call a heuristic scorer a heuristic scorer until it has real calibration data.
- **Automatic ontology discovery, merge, and split workflows.** This is a later optimization, not a starting point.
- **Ten specialized data-type adapters and external stores.** Time series, geospatial, financial, procedural, probabilistic, conversational, and streaming stores should not be predesigned before the actual data mix forces them.
- **Cross-type query orchestration as a first-class subsystem.** Shell plus SQLite can already orchestrate multiple data forms. A formal adapter/query framework is premature.
- **Forecast/signal/backtest machinery inside the KE core.** Useful eventually, but not part of the smallest viable knowledge engine.
- **1000-page report templates and broad output machinery.** Valuable presentation work, but orthogonal to getting knowledge storage and belief updates right.
- **Markdown migration compatibility.** This is Python-product baggage, not a Rust-runtime requirement.

## 4. Truth Engine Assessment

### Verdict

The truth model from `02-truth-engine.md` is **not viable for `aprs` as written**, but a reduced version is worth building.

The main issue is not just complexity. The spec couples too many speculative mechanisms:

- trust embeddings
- provenance DAG aggregation
- LCA-based independence
- PageRank priors
- community detection
- anomaly feedback
- recalibration loops

That is too much state, too much hidden behavior, and too much operator ambiguity for a first Rust implementation.

There is also a conceptual mismatch: the spec claims to produce Bayesian posteriors, but the actual resolution logic is a heuristic ranking formula. That is not fatal, but it should be described honestly.

### What is worth keeping

Keep this reduced truth model:

- `artifact -> claim -> conflict_set -> active_value`
- status outcomes: `accepted`, `contested`, `abstain`, `retracted`, `needs_review`
- explicit reasons for score changes
- source/lineage-aware corroboration
- recency decay by property class
- anchor handling for approved or authoritative sources
- contradiction retention
- recomputation when source trust or evidence changes

### What `aprs` should implement instead

Use a **deterministic evidence scorer** backed by SQLite, not learned embeddings:

- `source_prior`
- `anchor_bonus`
- `recency_factor`
- `extraction_confidence`
- `corroboration_bonus` from independent source groups
- `contradiction_penalty`
- `taint_penalty`
- optional `human_approved_bonus`

Then project:

- `confidence_score` as a bounded scalar
- `resolution_status`
- `top_alternative_claim_ids`
- `reason_codes`

Do not emit fake confidence intervals until there is enough evaluation data to support them. A scalar confidence plus explanation is more honest than invented statistical precision.

### Provenance model for `aprs`

Keep provenance mandatory, but simplify the shape:

- origin artifact ID and hash
- source subscription ID
- fetch command or shell pipeline used
- original URL/path
- parser/extractor version
- timestamps
- optional `derived_from_claim_id`
- optional human attestation / approval ID

That is enough to support audit, replay, taint propagation, and operator review. It is also easy to store and inspect.

### How it should integrate with taint tracking

Taint and truth should be **orthogonal**:

- **Truth/confidence** answers: "How likely is this claim to be correct?"
- **Taint** answers: "How dangerous is it to let this claim influence agent behavior?"

That separation matters. A claim can be true and still operationally unsafe to act on automatically.

Recommended integration:

- Every artifact inherits taint from its origin: `user_input`, `external_web`, `external_api`, `local_file`, `manual_approved`, `generated_summary`, etc.
- Every claim inherits the strongest taint from its supporting artifacts and transformations.
- Summaries, reports, and extracted entities inherit taint; taint never disappears because of summarization.
- Taint should cap actionability, not just confidence. Example: an `external_web` claim may be viewable and queryable, but cannot auto-trigger shell actions, high-impact state changes, or trust promotion without corroboration or approval.
- Corroboration should discount shared taint lineage. Ten claims copied from the same site family or API vendor count as one source group.
- Human approval can add an approval flag; it should not erase provenance or original taint.

In practice, `aprs` should use taint as the runtime-side equivalent of "operational trust" and the truth engine as epistemic scoring. Do not collapse them into one number.

## 5. Data Poisoning Defense

Bruce Schneier's February 25, 2026 post, ["Poisoning AI Training Data"](https://www.schneier.com/blog/archives/2026/02/poisoning-ai-training-data.html), is the right warning for `aprs`: low-effort false content can be made to look authoritative enough that AI systems repeat it. For agent systems, the equivalent threat is **runtime data poisoning**.

In `aprs`, the attacker does not need to poison model weights. They can poison:

- web pages the agent scrapes
- API responses
- user-provided URLs
- subscription feeds
- cached summaries
- persistent memory
- any retrieved artifact that later re-enters context

This is exactly the direction described in the February 10, 2026 paper by Brodt, Feldman, Schneier, and Nassi, ["The Promptware Kill Chain"](https://www.schneier.com/wp-content/uploads/2026/01/The-Promptware-Kill-Chain.pdf): persistence in agent systems comes from **memory and retrieval poisoning**, and shell-integrated agent runtimes make action-on-objective materially more dangerous.

`aprs` should defend against runtime poisoning with the following rules:

- **Treat all external data as untrusted by default.** No webpage, feed, API, or user URL is trusted merely because it was fetched successfully.
- **Separate observation from belief from action.** A fetched artifact becomes an observation first, then maybe a claim, then maybe an active value, and only then maybe an input to action. Never skip layers.
- **Store raw artifacts immutably.** Keep the bytes, hash, headers/metadata, fetch command, and timestamps so poisoned content can be replayed and audited later.
- **Never treat retrieved content as instructions.** Retrieved text may inform claims; it must never modify system prompts, policy, tool permissions, or shell command templates.
- **Sanitize before model exposure.** Strip active content, hidden text, metadata tricks, prompt-injection patterns, and unsupported MIME types before extraction.
- **Prefer structured sources over scraping.** If both exist, prefer a signed API or export over HTML scraping. If scraping is necessary, isolate it and mark the result with stronger taint.
- **Cluster source lineage aggressively.** Same domain family, same publisher network, same mirrored payload, or same upstream API vendor should count as one corroboration group.
- **Cap confidence from tainted sources.** Claims sourced only from untrusted external content should have a confidence ceiling until independently corroborated.
- **Block auto-action from tainted claims.** Untrusted claims may appear in answers but must not directly drive shell execution, approvals, state mutation, or outgoing messages.
- **Do not let summaries become primary evidence.** Model-generated summaries are convenience views, not authoritative artifacts. They should always point back to original evidence.
- **Add poisoning-specific monitoring.** Flag sudden claim convergence from one origin, bursts of novel entities from one domain, hidden-text artifacts, repeated conflicts from one source, or claims that only ever cite generated content.
- **Support quarantine.** A poisoned subscription or source should be pausable immediately without deleting prior evidence.

The mental model should be: **training data poisoning corrupts model weights; runtime data poisoning corrupts the agent's live belief state and tool-driving context.** For `aprs`, the second threat is the practical one.

## 6. Concrete Recommendations

Build in this order. Effort assumes one engineer familiar with the current Rust runtime.

1. **Finish generic subscriptions first, then reuse them as KE sources.** Knowledge ingestion should ride on the planned SQLite-backed subscription/checkpoint system, not invent a parallel source framework. Topics should stay optional grouping. **Effort: 3-5 days.**
2. **Add an immutable artifact ledger.** Store fetched files, API payloads, shell outputs, hashes, origin metadata, taint labels, retention class, and checkpoint linkage. This is the foundation for provenance and poisoning defense. **Effort: 4-7 days.**
3. **Add a minimal claim schema in SQLite.** Start with `entities`, `properties`, `claims`, `conflict_sets`, `active_values`, `evidence_links`, and `source_lineage_groups`. Keep it small and inspectable. **Effort: 5-8 days.**
4. **Build the shell-based ingest pipeline.** Subscriptions should run shell fetch/normalize/extract commands and record the full provenance envelope. Do not build connector/adaptor class hierarchies first. **Effort: 5-10 days.**
5. **Implement a simple deterministic resolver.** Score claims from source class, recency, corroboration group count, extraction confidence, contradiction penalties, anchors, and taint caps. Support `accepted`, `contested`, and `abstain`. **Effort: 4-6 days.**
6. **Integrate taint with claim use, not just shell I/O.** Propagate taint artifact -> claim -> active value -> answer -> tool input, and require corroboration or approval before tainted knowledge can drive actions. **Effort: 3-5 days.**
7. **Expose inspection-first CLI surfaces.** `ke artifact`, `ke claim`, `ke entity`, `ke conflicts`, `ke source`, and `ke replay` matter more than natural-language query planning at this stage. Operator trust comes from inspectability. **Effort: 4-6 days.**
8. **Emit subscription events from KE state changes.** Start with `entity.property_changed`, `claim.contested`, `confidence.dropped`, `source.quarantined`, and `artifact.ingested`. This is where KE becomes useful to the agent runtime. **Effort: 3-4 days.**
9. **Add a small evaluation harness before adding complexity.** Build gold tests for provenance completeness, dedupe, conflict resolution, taint propagation, and poisoning scenarios. Do not add embeddings or graph analytics before these are stable. **Effort: 4-6 days.**
10. **Defer everything else.** Specifically defer graph analytics, ontology auto-discovery, special stores, link prediction, PageRank, anomaly detection, and forecasting until the simple ledger proves insufficient. **Effort now: zero.**

## Bottom line

`aprs` should build a **SQLite-backed claim and provenance ledger integrated with subscriptions, shell ingestion, taint tracking, and approvals**.

It should **not** import the Python KE's graph-database ambitions, learned trust model, or analytics-platform sprawl.

The good idea in the old specs is not "graph science." It is "store evidence, make belief explicit, keep contradictions, and never let the agent forget where a fact came from."
