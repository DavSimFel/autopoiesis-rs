# Documentation Sync Plan

## Measured Baseline

The repo does not match the stale numbers in the task prompt. The documentation update should use the measured values from the current tree, not the requested placeholders:

- `src/` Rust source files: `52`
- `src/` Rust source lines: `34,821`
- Rust tests in `src/` + `tests/` (`#[test]` and `#[tokio::test]`): `564`
- Git commits on `HEAD`: `159`
- Current `HEAD`: `53da0bb`

## 1. Files Read

### Config and top-level docs

```text
AGENTS.md
Cargo.toml
agents.toml
README.md
docs/index.md
docs/roadmap.md
docs/vision.md
docs/risks.md
docs/architecture/overview.md
docs/architecture/guarded-shell-executor.md
docs/specs/plan-engine.md
docs/specs/identity-v2.md
docs/audits/doc-review-2026-03-24.md
```

### Source files in `src/`

```text
src/agent/loop_impl.rs
src/agent/loop_impl/tests.rs
src/agent/mod.rs
src/agent/queue.rs
src/agent/queue/tests.rs
src/agent/shell_execute.rs
src/agent/spawn.rs
src/agent/spawn/tests.rs
src/agent/tests.rs
src/agent/tests/common.rs
src/agent/tests/regression_tests.rs
src/auth.rs
src/cli.rs
src/config.rs
src/context.rs
src/delegation.rs
src/gate/budget.rs
src/gate/exfil_detector.rs
src/gate/mod.rs
src/gate/output_cap.rs
src/gate/secret_patterns.rs
src/gate/secret_redactor.rs
src/gate/shell_safety.rs
src/gate/streaming_redact.rs
src/identity.rs
src/lib.rs
src/llm/mod.rs
src/llm/openai.rs
src/main.rs
src/model_selection.rs
src/plan.rs
src/plan/executor.rs
src/plan/notify.rs
src/plan/patch.rs
src/plan/recovery.rs
src/plan/runner.rs
src/principal.rs
src/read_tool.rs
src/server/auth.rs
src/server/http.rs
src/server/mod.rs
src/server/queue.rs
src/server/ws.rs
src/session.rs
src/skills.rs
src/spawn.rs
src/store.rs
src/subscription.rs
src/template.rs
src/tool.rs
src/turn.rs
src/util.rs
```

### Supporting runtime assets and tests consulted for doc accuracy

```text
identity-templates/constitution.md
identity-templates/agents/silas/agent.md
identity-templates/context.md
skills/code-review.toml
tests/integration.rs
tests/plan_engine.rs
tests/shipped_shell_policy.rs
tests/tier_integration.rs
```

## 2. Exact Changes Per File

### `docs/roadmap.md`

- Rewrite the phase structure so Phase `1` through Phase `5` each remain explicit completed sections, plus a separate completed `Plan Engine` section.
- Fold the old `1a` and `1b` material into a single completed `Phase 1` section instead of leaving the roadmap in split-subphase form.
- Within those completed phase sections, describe the shipped work that matches the code:
  - observability and `tracing`
  - identity/config foundation
  - model routing and delegation
  - T2 read tool and T3 spawn path
  - skills
  - hardening and module splits
  - plan engine
- Replace speculative task tables with:
  - a short "Completed" section per phase naming the concrete modules that landed
  - a short "What remains" section naming only the actual backlog
- Restrict the remaining backlog to what is still genuinely unbuilt:
  - subscriptions v2 or subscription context wiring
  - topics beyond the current subscription label field
  - provider abstraction beyond the existing trait plus OpenAI backend
  - PTY shell support
  - real permissions or sandboxing
  - topic export/import
- Remove obsolete estimates and "not building yet" items that already shipped.
- Add one note that the roadmap is driven by measured implementation state, not the stale design assumptions in older docs.

### `docs/vision.md`

- Rewrite the document in present tense for shipped features and reserve future tense only for true backlog items.
- Replace the "one tool: shell" framing with the actual tier model:
  - T1 and T3 use shell
  - T2 uses `read_file`
  - spawned T3 workers can receive fully loaded skills
- Update the architecture narrative to include what is live today:
  - T1/T2/T3 split
  - plan engine as T2's structured execution model
  - skill discovery versus full skill loading
  - guard pipeline
  - SQLite queue and session store
  - subscription store
  - model catalog plus route selection
- Remove aspirational language for features that already exist:
  - identity v2
  - model routing
  - delegation
  - skills
  - plan engine
  - split `agent/` and `server/`
- Keep the future section narrow and honest:
  - provider expansion
  - real sandboxing
  - PTY shell
  - richer topic or subscription system
- Correct identity details to the actual file layout under `identity-templates/`.

### `docs/architecture/overview.md`

- Replace the stale stats with the measured baseline:
  - `52` Rust source files
  - `34,821` lines in `src/`
  - `564` Rust tests
  - `159` commits
- Replace the old module map with the current layout:
  - `agent/`: `loop_impl`, `queue`, `spawn`, `shell_execute`
  - `server/`: `mod`, `http`, `ws`, `auth`, `queue`
  - `gate/`: `budget`, `shell_safety`, `secret_redactor`, `exfil_detector`, `output_cap`, `streaming_redact`, `secret_patterns`
  - `plan/`: `runner`, `executor`, `notify`, `patch`, `recovery`
  - plus `config`, `context`, `session`, `store`, `turn`, `tool`, `spawn`, `skills`, `subscription`, `delegation`, `model_selection`, `read_tool`, `principal`, `identity`, `template`, `auth`, `cli`, `util`, `llm/*`, `main`, `lib`
- Update the execution flow to show the real data path:
  - CLI or HTTP or WS enqueue into SQLite
  - queue drain claims rows atomically
  - `build_turn_for_config()` assembles context, tools, and guards
  - LLM streaming and tool calls run through the guarded executor
  - results persist to JSONL and SQLite-backed metadata
  - WS streams tokens and approval prompts
  - plan runner claims plan rows and reuses spawn plus guarded shell execution
- Document current identity assembly and domain extension behavior:
  - constitution + agent + context for T1
  - constitution + context for T2 or T3
  - `[domains] selected` appends `context_extend` files
- Document current skill behavior:
  - T1 or T2 receive summaries
  - spawned T3 receives full skill instructions
- Document the real subscription state:
  - stored in SQLite with filters and token estimation
  - not yet injected into turn context
- Replace obsolete references to ignored plan placeholders with the implemented plan system.

### `docs/risks.md`

- Refresh the update date and rework the file so "open bugs" only contains issues still supported by the current code.
- Move the completed audit fixes into the resolved section and state them as resolved history, not active hazards.
- Add the resolved audit section in a way that keeps the count and the item list consistent. For the user-requested "14-bug audit fixes," enumerate the 14-item set explicitly:
  - `P0-1` through `P0-5`
  - `P1-1` through `P1-5`
  - `P1-7`
  - `P1-8`
  - `P1-10`
  - `P1-11`
- Treat `P1-9` separately from that 14-item audit-fix list. If final verification confirms it is fixed, move it into the broader resolved-history section without folding it into the "14 audit fixes" count.
- Re-check `P1-10` and `P1-11` before editing. Based on the current code and regression tests, both appear fixed and should move to resolved unless a final verification contradicts that reading.
- Remove or rewrite stale open items that no longer describe the code:
  - `P1-9` if `Session::append` is now correctly disk-first
  - `P1-10` if same-turn budget enforcement is confirmed
  - `P1-11` if approval prompt content is confirmed
  - `P1-6` because the current trimming behavior is pair-aware and covered by tests; if any residual concern remains, restate it precisely instead of repeating the older invariant break
- Keep or add only real current structural risks:
  - heuristic shell guards are not a sandbox
  - no PTY support
  - no filesystem or network sandboxing
  - server principal model is still operator-versus-user, not per-caller multi-tenancy
  - subscriptions are durable but not part of context assembly yet
- Make the distinction explicit between:
  - fixed bugs
  - still-open implementation gaps
  - longer-horizon architectural risks

### `docs/specs/plan-engine.md`

- Rewrite the spec from "converged design" into an implementation-backed spec.
- Update the schema section to match the real tables:
  - `plan_runs`
  - `plan_step_attempts`
- Describe the real `PlanAction` parsing path:
  - `plan-json` fenced block extraction from T2 assistant text
  - serde validation into `PlanAction`
  - `Plan`, `Done`, and `Escalate` handling
- Document patch semantics from `src/plan/patch.rs` explicitly:
  - `plan_run_id`
  - `replace_from_step`
  - revision increments
  - replacement of the remaining suffix only
  - how a patched `waiting_t2` run becomes executable again
- Document the actual supported step types and structures:
  - `spawn`
  - `shell`
  - shell checks with typed expectations
- Document the real executor behavior:
  - shell steps and shell checks go through `src/agent/shell_execute.rs`
  - spawn steps go through the child-session machinery
  - failed checks notify T2
  - crash recovery resets stale claims and resumes work
- Call out one important current implementation boundary:
  - `max_attempts` is parsed and validated, but the runner does not currently perform automatic retries from that field alone
- Document the CLI and startup integration that already exist:
  - `plan status`
  - `plan list`
  - `plan resume`
  - `plan cancel`
  - server startup crash recovery
- Update the T2-notification section to the actual message shape and queue behavior.

### `docs/specs/identity-v2.md`

- Rewrite the status from "design complete" to "implemented" or equivalent implementation-backed wording.
- Update the stack description to the current live three-layer model:
  - `constitution.md`
  - `agents/<name>/agent.md`
  - `context.md`
- Show the real `agents.toml` structure now in use:
  - `[agents.silas]`
  - `[agents.silas.t1]`
  - `[agents.silas.t2]`
  - `[models]`
  - `[models.catalog.*]`
  - `[models.routes.*]`
  - `[domains]`
  - `[domains.<name>]`
- Correct the tier loading rules to match the code:
  - T1 loads constitution + agent + context
  - T2 and T3 load constitution + context
  - selected domains append `context_extend`
- Document domain context as an implemented extension path, not just a design direction.
- Remove or rewrite concrete domain-pack examples that point at files not currently checked into the repo. Keep the spec on the mechanism unless the example files actually exist in-tree.
- Update the T3 description so it matches reality:
  - spawned T3 receives T2 or T3 identity files plus full skill instructions through the spawn path
  - model selection resolves through the catalog and routes
- Remove stale references to `identity/identity.md`, `operator.md`, or other superseded v1 names.

### `docs/index.md`

- Rewrite the reading order so it reflects the current doc set and current implementation state.
- Replace vague audience names with explicit roles or add a legend so the index is understandable without project lore.
- Move `identity-v2` and `plan-engine` out of "pre-implementation" framing.
- Update the index rules so they do not contradict the decision to keep implementation-backed specs live. Either:
  - allow shipped specs to remain live when they are still the most precise normative docs, or
  - explicitly state when they should later be folded into architecture and archived
- Update the live-doc list to include the real architecture and spec documents that remain authoritative after shipping.
- Fix any stale claims about research documents being cited if they are not actually linked by the live docs.
- Keep the rule that architecture docs describe the current code and future-looking docs stay clearly labeled.

### `README.md`

- Rewrite the feature list to the current product surface:
  - CLI, HTTP, and WS
  - tiered runtime with T1 or T2 or T3
  - `read_file` for T2
  - shell for T1 or T3
  - guard pipeline
  - plan engine
  - skills
  - SQLite queue and store
  - subscriptions
  - identity templates
- Replace the old v1 configuration example with the current `agents.toml` shape or a minimal but real excerpt from the current file.
- Update the architecture section to the split module tree.
- Add accurate stats from the measured baseline.
- Update usage examples to cover current user-visible commands:
  - prompt and REPL
  - `serve`
  - `auth`
  - `sub add/remove/list`
  - `plan status/list/resume/cancel`
- Keep the safety section explicit that RLIMIT is not sandboxing and that shell guards remain heuristic.
- Remove claims that identity v2, plan engine, or split modules are still future work.

### `AGENTS.md`

- Update the project structure block to the current measured size and current module families.
- Update the top warning banner so it matches the post-sync `docs/risks.md` state. If the queue, approval, and budget bugs are no longer open, the banner should warn about current structural risks without claiming those specific invariants are still broken.
- Add a key-files table that points contributors at the real entry points:
  - `src/agent/loop_impl.rs`
  - `src/agent/shell_execute.rs`
  - `src/turn.rs`
  - `src/store.rs`
  - `src/plan/*.rs`
  - `src/config.rs`
  - `src/server/*.rs`
  - `src/subscription.rs`
  - `src/session.rs`
- Update the module structure section so it no longer talks about future splits that already happened.
- Add or refresh the architecture diagram to show:
  - surfaces
  - SQLite queue and store
  - turn builder
  - guard pipeline
  - plan runner
  - spawned child sessions
- Refresh the pitfalls and invariants so they match the post-audit code:
  - queue claim semantics
  - tool call replay requirements
  - disk-backed shell output pointers
  - tier-specific tool surfaces
  - selected-domain identity extensions
- Replace stale dependency notes that still describe `tracing` and `thiserror` as future additions.

### `docs/architecture/guarded-shell-executor.md`

- Expand the file instead of removing it, because the code now has a central guarded shell execution module used in more than one path.
- Document the sequence in `src/agent/shell_execute.rs`:
  - guard tool call
  - request approval when required
  - execute tool
  - redact or guard output
  - cap output and write artifacts
  - parse exit code and return a normalized result
- Show both call sites:
  - agent loop tool execution
  - plan executor shell steps and checks
- Document the non-goals:
  - this is policy reuse, not a sandbox
  - shell still runs via `sh -lc`
  - no PTY yet

## 3. What Tests To Write

These doc updates are documentation-only, so no new runtime tests are required to make the docs buildable. That said, a few implementation tests would strengthen the invariants the updated docs would claim:

- Add a failure-injection test for `Session::append` that proves file append happens before in-memory mutation, so the `P1-9` resolution is backed by an explicit regression test instead of code inspection alone.
- Add a plan-runner test that documents current `max_attempts` semantics. The code parses and validates `max_attempts`, but the runner does not automatically retry based on that field alone. The docs should either state that clearly or the code should change.
- Add a plan patch-flow test that proves `replace_from_step` only replaces the remaining suffix, increments the revision, and allows a `waiting_t2` run to become executable again after patching.
- Add an identity-assembly test that verifies selected-domain `context_extend` files are appended for the correct tiers and do not leak the T1-only `agent.md` layer into T2 or T3.
- Add a spawned-worker test that verifies T1 or T2 receives only skill summaries while spawned T3 receives the full skill instructions expected by the updated docs.
- Add an integration test for subscription context assembly once subscriptions are actually wired into turn construction. That test does not belong in this doc-only change, but the updated roadmap and risks should call it out as missing coverage for the remaining backlog.
- If stats drift has been a repeated problem, add a lightweight check or generator for the architecture stats block so future docs cannot regress silently.

Existing tests already support several key doc corrections and should be cited during the doc rewrite:

- `src/agent/tests/regression_tests.rs` covers same-turn budget enforcement and inbound approval prompt content.
- `tests/plan_engine.rs` covers `plan-json` parsing and validation behavior.
- `tests/tier_integration.rs` covers tiered runtime behavior end to end.
- `tests/shipped_shell_policy.rs` covers the committed shell policy in `agents.toml`.

## 4. Order of Operations

1. Re-measure the repo stats immediately before editing docs so the final numbers are current at commit time.
2. Update the implementation-reference docs first:
   - `docs/architecture/overview.md`
   - `docs/architecture/guarded-shell-executor.md`
   - `docs/specs/plan-engine.md`
   - `docs/specs/identity-v2.md`
3. Update `docs/risks.md` next, because the overview and specs establish the implementation baseline needed to separate open risks from fixed bugs.
4. Update the narrative and navigation docs after the implementation docs are correct:
   - `docs/vision.md`
   - `docs/roadmap.md`
   - `docs/index.md`
   - `README.md`
   - `AGENTS.md`
5. Re-read the edited docs once for cross-links, naming consistency, and stat consistency.
6. Run the repo-mandated checks even though the diff is docs-only:
   - `cargo build --release`
   - `cargo test`
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `cargo test --features integration` only if auth is available

This order keeps the docs internally consistent while minimizing churn from later terminology changes.

## 5. Risk Assessment

- The highest risk is copying the stale task-prompt numbers into the docs. The measured repository state is `52 / 34,821 / 564 / 159`, not `52 / 35.5K / 558 / 160`.
- The second highest risk is overstating unfinished features. The code does not yet provide:
  - subscription-backed context assembly
  - a full topic system beyond the current `topic` field and subscription grouping
  - non-OpenAI provider implementations
  - PTY shell execution
  - real filesystem or network sandboxing
  - topic export/import
- Another risk is understating shipped functionality. Several docs still talk as if identity v2, plan engine, model routing, skills, and module splits are future work, which is now false.
- `docs/risks.md` needs the most careful pass. It currently mixes fixed issues, stale issues, and structural warnings. That file should be rewritten only after each claimed open bug has been checked against the current tests and code paths.
- The plan-engine spec needs one explicit caveat to avoid a new mismatch: `max_attempts` exists in the schema, but automatic retries are not implemented by the runner.
- The guarded-shell-executor architecture doc should be expanded, not deleted. Removing it would hide a real centralization seam that now matters to both the agent loop and the plan runner.
