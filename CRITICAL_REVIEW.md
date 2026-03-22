# Critical Review

`cargo build --release`, `cargo test`, `cargo fmt --check`, and `cargo clippy -- -D warnings` all pass on this revision. The findings below are from full source review.

## Code Quality

- P2: [src/context.rs:7] `ContextSource::name()` is never used, and [src/gate/mod.rs:67] `Guard::name()` is also unused. These dead interface methods increase trait surface without providing behavior — improvement, not urgent.
- P2: [src/session.rs:297] `load_today()` is a misleading name and comment: it replays every `*.jsonl` file in the session directory, not just today's file. The API name hides cross-day behavior and makes call sites easy to misread — improvement, not urgent.
- P2: [src/lib.rs:1] The crate re-exports nearly every internal module as public API. `server`, `session`, `store`, `turn`, and guard internals are all externally reachable even though they are implementation detail-heavy — improvement, not urgent.

## Correctness

- P1: [src/session.rs:132] `token_total()` stores provider usage (`input_tokens + output_tokens`), but [src/session.rs:409] `trim_context()` compares the sum of those usage counters against `max_context_tokens`. Because every turn's `input_tokens` already includes prior history, normal multi-turn sessions are over-counted and trimmed far more aggressively than the real context window — should fix before next feature.
- P1: [src/session.rs:264] `append()` mutates in-memory history and token counters before writing JSONL. If the file write fails at [src/session.rs:273], the live session diverges from disk and later reloads cannot reconstruct the state the process continued with — should fix before next feature.
- P1: [src/llm/openai.rs:506] Trailing SSE events are only handled for `TextDelta`; trailing `function_call_arguments.done`, `output_item.done`, `response.completed`, and `[DONE]` are dropped. Tool calls and usage metadata can vanish silently when the stream ends without a newline — should fix before next feature.
- P1: [src/llm/openai.rs:408] `stream_completion()` accepts EOF as success even if no `response.completed` or `[DONE]` arrived. A truncated network stream can be committed as a valid partial assistant turn instead of surfacing an error — should fix before next feature.
- P1: [src/session.rs:155] Session replay silently drops unknown roles, and malformed tool rows are only warned and discarded at [src/session.rs:188]. Corrupt JSONL mutates the reconstructed conversation instead of failing fast — should fix before next feature.
- P1: [src/store.rs:102] Queue claiming is `SELECT ... LIMIT 1` followed by unconditional `UPDATE`, so two processes using the same SQLite file can claim the same pending row. The transaction does not make the claim atomic across processes — should fix before next feature.
- P1: [src/store.rs:173] `recover_stale_messages()` resets every `processing` row with no lease, owner, or age check, and it is invoked on server startup at [src/server.rs:103]. A second server instance can steal work that another live process is still executing — should fix before next feature.
- P1: [src/context.rs:38] `Identity::strict()` panics on load failure, and production enables it at [src/turn.rs:199]. Missing or corrupt identity files crash CLI/server startup instead of degrading to the configured fallback prompt — should fix before next feature.
- P1: [src/main.rs:105] CLI mode never calls `recover_stale_messages()`. If a CLI process crashes after claiming a queue row, that row remains stuck in `processing` forever unless a server process later repairs it — should fix before next feature.
- P2: [src/agent.rs:295] Tool execution failures are formatted as JSON-looking strings without escaping the error payload. Quotes or newlines in the error message produce malformed pseudo-JSON that downstream prompts can mis-handle — improvement, not urgent.
- P2: [src/auth.rs:252] Token persistence truncates `auth.json` in place instead of writing a temp file and renaming it. A crash or full disk during write can corrupt the only token store — improvement, not urgent.

## Security

- P0: [src/gate/mod.rs:82] `guard_message_output()` only redacts `Text` blocks. Assistant `ToolCall.arguments` are serialized verbatim at [src/session.rs:107] and replayed to the provider at [src/llm/openai.rs:110], so any secret embedded in a tool call is written to disk and resent upstream unredacted — blocks further development.
- P0: [src/agent.rs:64] Approval/denial audit messages are persisted as `system` messages, and later system messages are replayed as system-role inputs at [src/llm/openai.rs:83]. Model- or user-controlled command text can therefore be promoted into highest-priority prompt space on the next turn — blocks further development.
- P0: [src/agent.rs:220] Tool outputs from untainted sessions are marked `Principal::Operator`, while taint is derived only from message principals at [src/turn.rs:67]. Combined with batch-only exfil detection at [src/gate/exfil_detector.rs:60], the agent can read sensitive data in one turn and exfiltrate it in a later turn without taint-based safeguards — blocks further development.
- P1: [src/server.rs:392] WebSocket authentication accepts API keys in the query string. Those secrets are routinely exposed through access logs, browser history, and reverse-proxy telemetry — should fix before next feature.
- P1: [src/gate/shell_safety.rs:81] Shell policy matching is a raw-string glob, not shell parsing. Equivalent commands using extra whitespace, newlines, comments, or wrapper shells can bypass allow/deny intent while still executing the same dangerous operation — should fix before next feature.
- P1: [src/tool.rs:132] `timeout_ms` is entirely model-controlled and unbounded. A tool call can request arbitrarily long execution and tie up shared runtime resources far beyond the intended 30s default — should fix before next feature.
- P2: [src/main.rs:99] CLI session names are not validated before being embedded in `sessions/{session_id}`. `--session ../../tmp/x` escapes the session root and redirects history/result writes to arbitrary filesystem locations — improvement, not urgent.

## Architecture

- P0: [src/server.rs:302] WebSocket turns hold the global `worker_lock`, and [src/server.rs:323] they also hold the shared store mutex across `agent::drain_queue()`. HTTP workers do the same at [src/server.rs:446] and [src/server.rs:460], while websocket approvals block indefinitely at [src/server.rs:585]. One slow command or ignored approval can freeze all sessions and block new enqueues/listing — blocks further development.
- P1: [src/context.rs:86] `History` is heavily tested but not used in the production turn builder at [src/turn.rs:181]; real replay/trimming happens separately through `Session` and `agent` at [src/agent.rs:129]. The codebase now has two divergent context-management designs to maintain — should fix before next feature.
- P2: [src/session.rs:460] `today_token_total()` rescans the entire day file every time `budget_snapshot()` runs. In tool-heavy sessions this adds repeated disk I/O and turns budget checking into an O(n²)-ish path over session length — improvement, not urgent.
- P2: [src/turn.rs:14] `Turn` carries mutable taint state in an `AtomicBool`. It is only safe because current callers avoid concurrent reuse; the type itself is non-reentrant and easy to misuse in future refactors — improvement, not urgent.
- P2: [src/server.rs:421] Every HTTP enqueue spawns a worker task even though the global `worker_lock` allows only one active turn. Bursty traffic creates avoidable task churn and contention without increasing throughput — improvement, not urgent.

## Tests

- P1: [tests/integration.rs:14] Integration tests do not self-skip when auth is absent, even though repo instructions say live tests should be skipped without credentials. `cargo test --features integration` hard-fails in a clean environment — should fix before next feature.
- P2: [src/agent.rs:769] The only automated test for pre-call context trimming is `#[ignore]`, leaving a fragile memory-management path unexecuted in normal CI — improvement, not urgent.
- P2: [tests/MANUAL.md:60] The manual checklist says missing `identity/` should fall back to the default prompt, but production code panics in strict mode. The documented acceptance criteria are stale — improvement, not urgent.
- P2: [tests/MANUAL.md:31] The manual checklist still expects the agent to admit it cannot fetch live data, even though the shell tool can access the network. That test plan no longer reflects actual capabilities — improvement, not urgent.
- P2: [src/gate/mod.rs:82] There is no coverage for redaction of `ToolCall.arguments`, and [src/llm/openai.rs:408] there is no test for truncated EOF without `response.completed`/`[DONE]`. Two of the highest-risk leak/corruption paths are currently untested — improvement, not urgent.
