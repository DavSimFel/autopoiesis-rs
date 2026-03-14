# Autopoiesis-rs Critical Review

## 1) Architecture Assessment

### Module structure and dependency health
- `main.rs` and `lib.rs` both declare separate module trees (`main.rs`: `mod agent; mod auth; ...`, `lib.rs`: `pub mod agent; pub mod auth; ...`), which creates two independently-owned copies of the same code shape.
  - This is brittle and violates a single-source-module layout. The crate has duplicated declarations and potential drift risk between binary and library builds.
  - There is no circular module dependency loop visible in the source graph.

### Provider abstraction (`LlmProvider`)
- The `LlmProvider` trait is minimal and extension-friendly conceptually:
  - `stream_completion(messages, tools, on_token)` captures the two critical runtime requirements.
- But abstraction leaks OpenAI semantics in places:
  - The agent loop assumes only `Stop` vs `ToolCalls` semantics.
  - OpenAI-specific content mapping and tool-call format are hidden in provider, but all higher layers assume the assistant emits `ToolCall` exactly this way.
- Good: adding a second provider is possible, but you will still need to force it into the same callback + tool-stop contract.

### Session/persistence model
- Data model is coherent at a glance: in-memory `messages`, append-on-write JSONL, daily file path.
- Main correctness risk: session persistence is not a faithful reification of the live message model. Tool call stubs are dropped during serialization, so resumed sessions lose full turn structure (more in Top 10).
- Daily file naming is practical, but there is no integrity policy, lock, or tamper checks.

### Responsibility separation
- Reasonable separation:
  - auth/config/session/tool/llm/agent are separated modules.
  - I/O-heavy concerns (tool exec, token persistence, JSONL persistence) are centralized.
- Weaknesses:
  - Provider/client concerns and CLI startup concerns bleed together (hard-coded paths and defaults in `main.rs`, no dependency inversion on config/env loaders).
  - Security-sensitive token handling is mixed with network/auth logic without an explicit secret store abstraction.

## 2) Code Quality (per file)

### `src/lib.rs`
- Good: concise module exports.
- No per-file correctness issues beyond maintainability duplication noted in architecture.

### `src/main.rs`
- Potential correctness/safety issue (non-UTF-8 cwd): `cwd = env::current_dir().ok().and_then(|path| path.to_str().map(ToString::to_string)).unwrap_or_else(String::new);` silently discards cwd on non-UTF-8 paths (`main.rs:91-94`).
- Architectural smell: binary defines its own `mod` tree instead of reusing `lib.rs`, causing duplicated module graphs (`main.rs:7-15`).
- Missing config/path flexibility: hard-coded `agents.toml` and `identity` directory (`main.rs:88`, `main.rs:106`) reduce testability/deployability.
- Comment clarity: no explicit validation for interactive/no prompt command mode transitions before entering loop.

### `src/config.rs`
- Error behavior is okay for loading and fallback.
- Missing validation: no schema validation for empty model/base_url/invalid format before runtime (`config.rs:23-29` defaults + `from_str` path `39-40`).
- Defaults are hardcoded in code; future config changes require recompilation (`config.rs:24-28`).

### `src/identity.rs`
- Correctness: all-or-nothing load of three files; missing any file aborts prompt assembly (`identity.rs:15-24`). In strict mode this is surprising and likely brittle when directories are partially deployed.
- Safety: no canonicalization/path checks; accepts any `identity_dir` string passed by caller (`identity.rs:9-10`).
- Missing comments: `load_system_prompt` implies a directory lookup but does not document fail-closed vs fallback behavior.

### `src/auth.rs`
- Security/comment mismatch: comment on “safe fallback” is incorrect if file permissions/atomicity are insecure (`auth.rs:33-37`).
- Missing hardening: tokens are serialized in cleartext with default perms; no explicit `0o600` permissioning, no atomic rename, no backup strategy (`auth.rs:225-236`, `auth.rs:238-249`).
- Error handling: `token_file_path` relies on env `HOME` with `/root` fallback and no validation (`auth.rs:34-37`).
- Polling semantics: only `403/404` treated as “retry”; other transient status codes are terminal (`auth.rs:135-158`) with potentially recoverable conditions.

### `src/template.rs`
- Correctness is simple and clear.
- Missing escaping/encoding policy: template values are raw-substituted into prompt instructions (`template.rs:3-9`), allowing prompt-confusion if values include template-like markers or untrusted content.
- `HashMap` iteration order is non-deterministic; overlapping keys can produce non-obvious replacements (`template.rs:6-8`).

### `src/tools.rs`
- Critical correctness/security issue in execution model (discussed in security/top-10): `Command::new("sh").arg("-lc").arg(&command)` executes arbitrary shell text (`tools.rs:52-55`).
- Missing guardrails: no output size cap and no stderr/stdout stream truncation, so huge outputs can consume memory (`tools.rs:60-75`).
- API contract issue: `timeout_ms` accepts any `u64`; effectively unbounded waits are possible (`tools.rs:46-50`).
- `execute_tool_call` only validates presence of a string; it does not enforce non-empty/allowlist/sandbox policy (`tools.rs:39-45`).

### `src/session.rs`
- Serialization gap: `to_entry` drops assistant `ToolCall` blocks; it serializes only text/tool results (`session.rs:81-90`, `session.rs:73-90`).
- Deserialization asymmetry: tool messages with missing metadata fall back to empty call IDs and names, losing causality (`session.rs:154-161`).
- Load-time context control missing: `load_today` accumulates old context but does not trim to limits on startup (`session.rs:198-229`).
- `trim_context` removes from fixed index `1` in pairs, assuming a strict two-message cadence and risking loss of valid cross-type context structure (`session.rs:237-260`).
- File I/O: appends without file locking or atomic writes.

### `src/util.rs`
- Timestamp logic is hand-rolled; tests catch current shape but this is non-trivial date arithmetic that is easy to get wrong across edge cases.
- On system clock anomalies it silently degrades to epoch using `unwrap_or_default()` (`util.rs:9-12`) instead of explicit handling.

### `src/llm/mod.rs`
- The message model is sensible and the trait is clean.
- `ChatMessage`/`MessageContent` tagging strategy is rigid; adding richer multimodal content requires enum churn.
- No additional safety issues in this layer.

### `src/llm/openai.rs`
- Stream parsing/state is brittle and assumes mostly-serialized, single-call argument stream.
- `parse_sse_line` returns `None` on parse errors without surfacing protocol corruption (`openai.rs:160`), which silently drops unexpected events.
- `stream_completion` reads the whole response body before parsing (`openai.rs:500-503`), so it is not truly streaming and can amplify memory use on long responses.
- Tool-call state assembly assumes one active call stream and collapses into a global accumulator (`openai.rs:505-550`), plus uses `HashMap` for final ordering (`openai.rs:570-577`).
- `build_input` keeps only the latest system text and ignores other system content blocks that may be present, which is an implicit behavior decision not called out in docs (`openai.rs:42-54`, especially lines 48-54).

### `src/agent.rs`
- Error-handling style is okay for resilience.
- JSON produced on tool errors is malformed JSON-like string due escaped quotes in raw string literal (`agent.rs:49`).
- No cap for recursive tool loop; model can force many tool turns before termination.

### `tests/integration.rs`
- Not a production file, but test quality is weak for critical paths:
  - network-dependent and auth-dependent; not runnable offline;
  - behavioral assumptions about external model compliance are brittle (`integration.rs:12-47`, `integration.rs:81-119`).

## 3) Security

### Auth token handling
- Token file is plaintext and path-based only; no secure permissions or atomic replacement (`auth.rs:225-236`).
- `load_tokens` + `save_tokens` are susceptible to TOCTOU and symlink-style replacement attacks in shared filesystem environments (`auth.rs:238-249`, `auth.rs:233-235`).
- Refresh and login flows do not guard token exfiltration logging. Current code avoids printing tokens, which is good.

### Shell execution
- Tool execution path is high-risk:
  - `execute_tool_call` executes `command` through `sh -lc`, giving model-supplied input full shell execution powers (`tools.rs:52-55`).
  - This is the highest-priority security issue and can run arbitrary commands with user privileges.

### File I/O integrity
- Session and auth files are appended without locking/atomicity guarantees.
- Session writes can be interleaved if multiple processes run simultaneously.
- No truncation/size limit on command output means file size can grow with attacker-controlled output (`tools.rs:60-75`).

### Session file exposure
- Sessions are plain JSONL text with full user/tool output and model-visible prompts; no redaction/ACL controls.
- Tool output may include secrets or tokens from user commands; these are persisted by design with no masking (`session.rs:170-179`, `session.rs:134-136`).

## 4) Testing Gaps

### Well-covered
- Unit tests exist for core models/parsing/schemas for config, identity, template, and some OpenAI parsing cases.
- OpenAI SSE parse tests cover basic text and completed events (`llm/openai.rs` test module).

### Missing or weak coverage
- Auth flow untested:
  - no unit/integration tests for token refresh failures, malformed token files, clock-skew handling, poll retry paths (`auth.rs`).
- Session persistence coverage misses adversarial/edge persistence:
  - malformed JSONL lines,
  - tool-call roundtrip persistence,
  - startup load-time trimming behavior (`session.rs`).
- Tools module lacks negative tests:
  - timeout edge values,
  - very large outputs,
  - malformed JSON args,
  - command injection boundaries.
- OpenAI stream robustness weakly tested:
  - interleaved tool-call deltas,
  - multiple in-flight calls,
  - malformed stream lines,
  - output item ordering.

### False-green risks
- `integration` tests are behind feature flag and rely on live OpenAI + valid credentials; they are likely skipped in most CI, so critical paths can pass with zero real coverage.
- `parse_sse_line` tests validate some parse variants but not failure modes, so silent dropping (`None`) could hide bugs without failing tests.

## 5) Scalability Concerns

- Context growth at startup is unbounded until first post-load append because trimming is load-bypassed (`session.rs:198-229`). This can lead to large memory and API payload usage on long-running days.
- Command outputs and token persistence are unbounded; large outputs will bloat session files and slow JSONL scans.
- Daily session file strategy requires full replay on startup; with heavy usage this becomes O(size of day) before first response.
- Provider parsing is not streaming despite SSE interface; large responses are buffered fully in memory (`openai.rs:500-503`) which will scale poorly.
- Single global toolset and hard-coded command tool means future extension will require intrusive changes (`tools.rs`, `main.rs`, `llm/openai.rs`, `agent.rs`) rather than adding providers/plugins.

## 6) Top 10 Issues (ranked)

1. P0 — Arbitrary shell execution through model-controlled input
   - `src/tools.rs:52-55` (`execute_tool_call`)
   - Risk: direct RCE with model output, can read/write files, run network tools, exfiltrate secrets.
   - Fix: execute direct binaries without shell, validate/allowlist commands, add sandboxing (policy, cgroup/container), and user confirmation in interactive mode.

2. P1 — Tool-call serialization loss breaks resumability
   - `src/session.rs:73-90` (`to_entry`) and `src/session.rs:139-168` (`message_from_entry`)
   - Risk: resumed session loses prior assistant tool-call stubs, producing invalid/misaligned conversation state.
   - Fix: persist full `ChatMessage` (or at least explicit `ToolCall` fields) and reconstruct message variants verbatim.

3. P1 — Tool-call ordering is non-deterministic after stream parse
   - `src/llm/openai.rs:570-577` (`tool_calls` from `HashMap`)
   - Risk: tool execution order can flip, breaking model expectations and causing wrong side effects.
   - Fix: collect tool calls in an ordered `Vec` keyed by stream sequence, and dedupe by id only when necessary.

4. P1 — SSE argument parsing is single-stream and can mix concurrent function-call streams
   - `src/llm/openai.rs:505-550`
   - Risk: interleaved `function_call_arguments.delta` events for multiple calls produce corrupted arguments and wrong `call_id/name` assignments.
   - Fix: track parser state per `call_id` in a map of accumulators; only finalize one call when done for that id.

5. P1 — Session startup can exceed context constraints immediately
   - `src/session.rs:198-229` (`load_today`)
   - Risk: very large historical day file loads uncapped into `messages`, then first API call sends huge context before `trim_context` runs.
   - Fix: call trimming (or lazy trimming) right after load and persist pruned replay offset.

6. P1 — Tokens stored with insecure defaults and TOCTOU exposure
   - `src/auth.rs:225-236`, `src/auth.rs:238-249`
   - Risk: token file readable by wrong user; race window on `exists` then `read/write` allows swap attacks.
   - Fix: create file with restricted mode (e.g., 0600), open atomically, reject symlinks, write to temp + fsync + rename.

7. P1 — Provider parser drops malformed stream events silently
   - `src/llm/openai.rs:160`
   - Risk: protocol drift or model API anomalies are silently ignored, creating silent degradations and invalid reasoning state.
   - Fix: emit structured diagnostics and fail-fast on malformed critical events in strict mode.

8. P2 — `main.rs` and `lib.rs` define parallel module trees
   - `src/main.rs:7-15` and `src/lib.rs:1-8`
   - Risk: maintenance drift and inconsistent behavior between binary and library builds.
   - Fix: make binary depend on library modules (`use autopoiesis::...`) and delete duplicate module declarations.

9. P2 — No bounded output limits for tool execution
   - `src/tools.rs:60-75`
   - Risk: malicious command can emit very large output, inflating memory and session file sizes.
   - Fix: enforce max output bytes and stream/crop outputs before serializing to session.

10. P2 — Context contamination from malformed/untrusted template values
   - `src/template.rs:3-9`
   - Risk: raw insertion allows prompt-manipulation side effects; user-provided vars can mutate instruction semantics.
   - Fix: escape template values for plain-text insertion and enforce a strict templating format/type for variables.
