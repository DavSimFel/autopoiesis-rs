# Architecture and Security Review: autopoiesis-rs (Gate System)

## 1. Architecture — does the gate-centric design hold up?

Short answer: conceptually solid, but not complete enough for high-assurance execution.

`Pipeline` is a good separation point for pre/post-provider transformation + controls, but this implementation has a few structural weaknesses:

- Prompt assembly and security are coupled too tightly and run with different lifecycles, which creates observable inconsistency.
- The gate graph is hard-coded (`assemble -> context -> sanitize -> validate`), but `HistoryGate` is still treated as a special case instead of first-class banded gate.
- `HistoryGate` mutates inbound prompt-building behavior in a way that can silently discard required turns when budgets are low.
- `Agent` loop integration is leaky: it trusts the provider output and only consults gates later, but never feeds outbound outputs back through any gate for sanitization.

For the architecture questions:

### Is `Pipeline` the right abstraction for prompt assembly + security?

Mostly yes for this codebase. The directionality model (`In`, `Out`, `Both`) is useful, but `run_inbound` mutates message list only while `check_tool_call/check_tool_batch` are separate. This makes it hard to reason about a single end-to-end pipeline contract.

`run_inbound` currently:
- assembles prompt material (identity/context),
- redacts/sanitizes text blocks,
- and returns either `Allow`, `Edit`, or hard stop.

There is no symmetric output pipeline for text responses/tool results. That asymmetry is the largest architectural gap.

### Does `HistoryGate` belong as a special-cased field or should it be a regular gate?

It should be a regular gate.

Current special-casing (`Pipeline { history: Option<HistoryGate> }`) means:
- no ordering beyond fixed insertion position,
- no way to have multiple history-like gates,
- no easy composition with `recording_gate` tests and banded ordering,
- inconsistent behavior versus other gate types in API surface.

This is a maintainability issue now and will be brittle once additional prompt-shaping gates are added.

### Is band model (`Assemble → Sanitize → Validate`) correct? Missing bands?

The three-band flow is directionally correct for this runtime, but the event model is incomplete:
- there is no explicit **Transform/Normalize** band for canonicalization (e.g., system-message normalization),
- no **Output** band for post-response redaction, approval stamping, or safety policy enforcement.

Adding an explicit output band would prevent the current “unchecked assistant output” gap.

### Are `GateEvent` variants sufficient?

No. Current variants:
- `Messages`, `ToolCallComplete`, `ToolCallBatch`, `TextDelta`.

Missing variants create blind spots:
- no session/context snapshot event for auditing,
- no post-assembly event that carries tool-call results,
- no execution error event,
- no outbound assistant/tool output stream event object model.

`TextDelta` is currently defined but not used anywhere, suggesting either dead functionality or incomplete streaming design.

### Does the agent loop integrate cleanly or fight the gates?

Partially, but not cleanly.
- It calls `pipeline.update_context(session.history())` and `run_inbound()` each turn.
- It correctly short-circuits on `Block`/`Request` from validation.
- It does **not** surface `TurnVerdict` to `main.rs`, so user approvals/blocks are not externally enforced.
- It does **not** apply gate output path to model text/tool output before append/echo.

This is the biggest integration mismatch: gates are partially enforced but their outcomes are not consistently surfaced or completed through output safety.

## 2. Gate System Deep Dive (`src/gate.rs`)

### IdentityGate

`IdentityGate` always loads all identity docs, renders template tokens, and rewrites the first system message.

- Correctness:
  - Loads fallback if identity dir missing.
  - Replaces missing system message when absent.
  - Rewrites existing first system block if it differs from rendered prompt.
- Edge cases:
  - A session may contain tool/system side effects, but only first system position is controlled.
  - It rewrites any first message with non-text system content, effectively treating malformed system blocks as replaceable rather than invalid.
  - `load_prompt()` composes file I/O on every check; repeated reads per request can become noticeable.

### HistoryGate

- Algorithm:
  - Uses tokenized size of each message text-only content.
  - Walks `history.iter().rev()` and selects newest turns that fit in budget, then reverses selected messages and appends to prompt.
- Correctness:
  - `newest-first` logic works.
  - System messages are skipped in history selection.
- Missing/fragile behavior:
  - It uses a simple text-only estimator; tool-call arguments and structured overhead are ignored.
  - `current_tokens` includes tokens from already-provided inbound messages, so budget accounting may be inflated and drop good context unnecessarily.
  - If budget is too low for latest user turn, turn can be dropped entirely, producing prompt without user request.

### SecretRedactor

- It compiles regexes with `filter_map(... ok())`, silently dropping invalid patterns.
- `check` can only redact `Text` and `ToolResult.content`.
- Edge cases:
  - Redaction happens on plain text blocks only.
  - No redaction of structured fields (`tool` args, JSON keys, names, non-text blocks, or hidden metadata).
  - No canonicalization/normalization pass before matching means escaped variants and separators can bypass patterns.

### ShellHeuristic

- What it catches:
  - Explicit `rm -rf /`, `mkfs`, `shutdown/reboot`, `dd if=...`.
  - Simple multi-command chains via `&&`, `;`, and `|` splitting.
- What it misses:
  - Shell-level indirections: command substitution, arithmetic expansion, `\` escapes, newlines, `$(...)`, backticks.
  - Redirect and process-substitution paths that can execute binaries.
  - Case where separator is inside quoted context (split regex is not quote-aware).
  - `r\m` style escaping remains a known bypass pattern in the tests themselves.
- False negatives are materially dangerous because execution path is `sh -lc` with full shell power.

### ExfiltrationDetector

- The detector is intentionally coarse:
  - flags if any command in batch reads sensitive targets and any command in batch sends outbound.
  - blocks regardless of order, but only via substring checks.
- Coverage gaps:
  - misses many read sources (`/etc/shadow`, `/proc/*/cmdline`, config dirs, cloud creds variants),
  - misses many send channels (`python` sockets, `openssl s_client`, `ssh`, `socat`, `curl` with obfuscated args),
  - misses obfuscated command separators and multiline staging.
- It is easy to evade with in-line command substitutions or encoded separators where read+send happen in one command without exact substrings.

### Pipeline execution in `src/gate.rs`

`run_inbound` order and short-circuit are deterministic:
- Assemble gates run first, then optional history, then sanitize.
- Validation runs separately on outbound tool-call events.
- On `Block`/`Request`, it returns immediately.

Weaknesses:
- No output safety pass is applied for assistant text/tool results in this same `Pipeline`.
- History gate is special-cased rather than generic banded gate.
- No error handling for ambiguous regex split / parser failures beyond `Allow`; failing open in several paths.

## 3. Per-File Code Quality

[src/lib.rs](/tmp/aprs-review2/src/lib.rs) — clean module surface. No blocking issues found.

[src/main.rs](/tmp/aprs-review2/src/main.rs)
- Panics/missing handling: minimal.
- Bug: `run_agent_loop` return value is ignored, so `RequestApproval` and `Blocked` verdicts are never surfaced to user (control-flow bypass).
- UI/flow: interactive mode always prints a blank line after run regardless of verdict.
- Security integration: block/request reasons are appended to session but not printed back.

[src/config.rs](/tmp/aprs-review2/src/config.rs)
- Mostly solid loader code.
- Minor naming: default model string hardcoded; tests rely on stable semantics of that default.
- Missing: no validation on `base_url` format or allowed model values.
- Dead code: none.

[src/auth.rs](/tmp/aprs-review2/src/auth.rs)
- Polling statuses are narrow (`403`/`404`); token endpoint may emit other retryable states.
- `read_tokens()`/`load_tokens()` have straightforward unwrap flows with context.
- No panics, but long-lived token refresh logic assumes system clock validity.

[src/identity.rs](/tmp/aprs-review2/src/identity.rs)
- Correct-by-default file loading is clear.
- Fail-fast on missing identity files is intentional.
- Potential robustness issue: any malformed or partial identity bundle makes the entire load fail (even if partial content would be acceptable).

[src/template.rs](/tmp/aprs-review2/src/template.rs)
- Straightforward replacement.
- No escaping of braces or recursive expansion; deterministic behavior.
- No panics; complexity low.

[src/tools.rs](/tmp/aprs-review2/src/tools.rs)
- Executes commands through `sh -lc`; this is inherently high-risk and must depend entirely on gate correctness.
- Error output is stringified without JSON escaping, which can produce malformed payload strings in structured downstream consumers.
- No hard timeout upper bound validation beyond numeric conversion.

[src/util.rs](/tmp/aprs-review2/src/util.rs)
- Date-time math is manual; tests cover basic format and drift.
- `unwrap_or_default` on time means invalid system clocks collapse to epoch epoch-time style behavior silently.

[src/session.rs](/tmp/aprs-review2/src/session.rs)
- Core persistence logic is understandable.
- Major correctness concern: tool-call messages are not round-trippable because `message_from_entry()` reconstructs assistant messages as plain text-only, dropping `ToolCall` blocks.
- `append` writes to file then trims memory only; on restart file keeps historical messages that are considered trimmed.
- `trim_context()` uses alternating behavior tied to `total_tokens == 0`; context policy can be inconsistent between runs.

[src/llm/mod.rs](/tmp/aprs-review2/src/llm/mod.rs)
- Data model is coherent.
- No obvious panics.
- Naming is clear.

[src/llm/openai.rs](/tmp/aprs-review2/src/llm/openai.rs)
- SSE parser is simple and intentionally narrow.
- Potential quality gap: parser assumes event framing by line and discards malformed frames silently.
- `response.completed` metadata extraction is permissive but non-fatal, which may hide provider format drift.

[src/gate.rs](/tmp/aprs-review2/src/gate.rs)
- Main hotspot. `TextDelta` exists but unused.
- Directional logic is clear.
- Regex/parse errors are mostly silent-allow, which is safer for liveness but poor for security.
- Some checks rely on lowercase token assumptions and naive substring matching.

[src/agent.rs](/tmp/aprs-review2/src/agent.rs)
- `TurnVerdict` exists but not consumed by caller.
- No post-append redaction on assistant/tool output paths.
- On tool-call batches, checks are performed before execution (good), but execution error formatting is raw.
- Return type suggests interactive approval, but main loop never acts on it.

## 4. Security — adversarial

### What shell commands bypass `ShellHeuristic` right now?

Commands likely to pass despite intent:
- `r\m -rf /` style obfuscation (already in tests as `allow|block` ambiguity).
- `$(command)` / backticks producing destructive effects.
- `command=$(echo rm); $command -rf /`
- `\n`/line-break-separated shell segments and here-doc style payloads.
- `sh -lc` side effects via `$IFS`, `eval`, `xargs sh -c`, `printf '...' | sh`.
- quoted/escaped separators where split regex still sees a safe token boundary but shell resolves differently.

### What secrets bypass `SecretRedactor`?

- Provider tokens outside the three regexes:
  - AWS session keys, PAT variants (`gho_`, `ghu_`, etc.), API keys with separators.
- Secrets split by whitespace/newline/JSON escapes across blocks.
- Non-text containers (e.g., tool arg JSON, command outputs with escaped fields) if they are not in redacted `Text`/`ToolResult.content`.

### Can the gate system be manipulated via prompt injection?

Yes.
- The gate is invoked on inbound assembled messages, but the model can still influence behavior inside tool arguments unless command parsing catches semantic expansions.
- Because output is not sanitized, a poisoned assistant response can plant future prompt text before user sees anything.
- Identity/system replacement only touches first position, so prompt-only injection via other channels (tool output context corruption, session artifacts) remains.

### TOCTOU issues between gating and execution

- High-risk seam: `run_agent_loop` validates tool call and then executes command via the same `ToolCall` object. If an attacker can force parse ambiguity at shell execution time (`shell_words` vs shell parser), checks and execution do not align.
- Another seam: policy is applied on model output before tool execution, but not on output of executed tool when persisting/feeding next turn.

### Token estimation attacks

- `Session` and `HistoryGate` rely on text-only cl100k estimates in multiple places.
- Attackers can force low-entropy inputs with large structural overhead (tool calls, control frames, long system-like strings), causing under/over-trim mismatches.
- If `total_tokens == 0` mode engages, estimation is still used and can significantly drift from actual provider accounting, allowing context behavior changes across identical turns.

## 5. Test Quality

- Several tests are not adversarial enough:
  - `fuzz_adversarial_shell_commands` in [`src/gate.rs`](/tmp/aprs-review2/src/gate.rs) uses only seven examples and includes intentionally ambiguous cases.
  - `tests` emphasize path-positive assertions and rarely assert that blocked behavior has side effects prevented.
- Some tests can pass incorrectly:
  - `backslash_escape_binary` accepts both block and allow, so it does not pin security posture.
  - `catches_piped_exfiltration` and related tests rely on helper assumptions that exact strings are present.
- Uncovered gaps:
  - No structured property tests for shell AST bypasses (`$(...)`, `$IFS`, unicode whitespace, brace expansion).
  - No adversarial tests for prompt injection across rounds (tool-call + tool-result + re-inclusion in prompt).
  - No integration tests asserting `main.rs` handles `TurnVerdict` outcomes.
  - No tests for stale history file replay behavior after `trim_context`.
- Risk: a large portion of tests are format/roundtrip checks, not red-team behavioral guarantees.

## 6. Top 10 Issues (ranked P0-P2)

1. [P0] Outbound secret leakage is not blocked or redacted
- File/range: [src/agent.rs](/tmp/aprs-review2/src/agent.rs):73-70 and 143-147.
- What fails: assistant text and all tool output are appended to session without redaction.
- Fix: run `SecretRedactor` on outbound `Messages` before `session.append`, and consider separate outbound output band in `Pipeline`.

2. [P0] Shell command parsing bypass allows dangerous execution despite validator
- File/range: [src/gate.rs](/tmp/aprs-review2/src/gate.rs):519-526, 542-585, 601-625.
- What fails: command splitting relies on a simplistic regex and tokenization; shell expression features are not normalized before policy checks.
- Fix: replace with AST-like shell parse or safer command execution model (explicitly disallow `sh -lc`, use allow-listed binary + args).

3. [P0] Exfiltration detection has low coverage and can be bypassed inside command language
- File/range: [src/gate.rs](/tmp/aprs-review2/src/gate.rs):660-680, 704-726.
- What fails: broad substring matching catches only a few read/send forms; many common exfil vectors are unrecognized.
- Fix: explicit denylist/allowlist for network primitives plus shell AST + argument normalization, not raw substring search.

4. [P1] `main.rs` never consumes `TurnVerdict`; approval/block decisions become invisible
- File/range: [src/main.rs](/tmp/aprs-review2/src/main.rs):115-132.
- What fails: `run_agent_loop` returns `RequestApproval`/`Blocked`, but caller ignores it.
- Fix: handle verdicts in REPL and non-interactive flows (print decision + request confirmation + exit code mapping).

5. [P1] Assistant tool-call context is not faithfully persisted/restored
- File/range: [src/session.rs](/tmp/aprs-review2/src/session.rs):65-106 and 131-160.
- What fails: persisted assistant messages discard `ToolCall` blocks, so reload loses exact function-call context.
- Fix: persist and restore assistant tool-call blocks via serialized typed `ChatMessage`, not text-only collapse.

6. [P1] Trim logic diverges from persisted file
- File/range: [src/session.rs](/tmp/aprs-review2/src/session.rs):185-200, 242-247.
- What fails: `trim_context` mutates in-memory state only after write; on restart removed turns are reloaded from disk.
- Fix: either replay from canonical file rewrite or keep persistent index of active context window.

7. [P1] `HistoryGate` can drop the entire user turn under tight budget
- File/range: [src/gate.rs](/tmp/aprs-review2/src/gate.rs):177-200.
- What fails: if latest message alone exceeds budget, no context is appended beyond system prompt.
- Fix: enforce minimum guarantee for latest user turn (e.g., reserve budget floor for most recent turn, then trim earlier history).

8. [P2] Case handling and pattern gaps in `is_root_recursive_rm`
- File/range: [src/gate.rs](/tmp/aprs-review2/src/gate.rs):452-466.
- What fails: `args` are lowercased then compared against `"-R"` making that branch dead; inconsistent coverage in recursive flag forms.
- Fix: standardize lowercase handling and normalize all flag variants once (`-r`, `-rf`, `-fr`, `-R`, `-Rf`, `-rF`).

9. [P2] `SecretRedactor` silently drops invalid pattern definitions
- File/range: [src/gate.rs](/tmp/aprs-review2/src/gate.rs):350-356.
- What fails: invalid regex silently ignored, reducing sanitization coverage without alert.
- Fix: fail fast on invalid redaction patterns in config/constructor.

10. [P2] Tool output/error formatting can create malformed structured payloads
- File/range: [src/tools.rs](/tmp/aprs-review2/src/tools.rs):134-135, 63-75.
- What fails: tool errors are interpolated into pseudo-JSON with no escaping; large unbounded outputs are stored verbatim.
- Fix: serialize tool output as typed JSON/object and cap/sanitize payload size before persistence.
