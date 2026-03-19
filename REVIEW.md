# Critical Review

`VISION.md` sets a clear bar: one queue, shared turn construction, auditable shell execution, crash/resume, and a guard pipeline that materially reduces risk. The codebase does not meet that bar in several important places. `cargo test` passes, but one agent trimming test is ignored, and multiple severe problems survive because the tests mostly assert local behavior rather than end-to-end invariants.

## Severity-Ordered Findings

### Critical

1. `src/server.rs:273-289` bypasses the SQLite queue completely for WebSocket traffic. It enqueues the message, then immediately calls `run_agent_loop()` on the raw content instead of dequeuing and marking the row processed. This breaks the VISION "one inbox, one queue" design, leaves WS messages stuck in `pending`, and makes queue state lie about what was actually executed.

2. `src/server.rs:279-382` hardcodes `WsAutoApprove`, so every approval-required command is silently approved over WebSocket. That nullifies the entire `Approve { severity }` path for the server execution path. The guard pipeline exists, but the server throws away its most important outcome.

3. `src/llm/openai.rs:64-71` keeps only the last `system` message as `instructions`. `src/agent.rs:111-120`, `src/agent.rs:168-171`, and `src/agent.rs:231-232` append operational system messages like approval denials and hard denials. After the first such message, the provider can send that denial string as the only system prompt, replacing constitution/identity/context. This is a severe correctness bug: the runtime can lose its governing prompt mid-session.

4. `src/session.rs:274-314` trims from index `1` under the assumption that index `0` is always a preserved system prompt. In actual persisted session state, there often is no system prompt at index `0`. The result is that the oldest user message gets pinned forever while newer history is discarded. The same logic removes two messages at a time and can split tool-call round trips, leaving orphaned tool results or assistant tool-call stubs in history.

5. `src/tool.rs:136-160` does not reliably kill the child process group on timeout. It sends `SIGTERM` to the process group, then only sends `SIGKILL` to the group if `child.kill().await` fails. If the parent dies but descendants ignore `SIGTERM`, they survive. That directly contradicts the "process-group kill" claim in VISION and the source comments.

6. The "sandbox" is mostly branding. `src/tool.rs:20-39` sets a few RLIMITs, but there is no filesystem isolation, no network isolation, no seccomp, no chroot, no uid drop, and no memory limit. Combined with `src/guard.rs`, which is only heuristic command inspection, this means arbitrary shell code still runs with full user privileges. The security story is materially weaker than the code/comments imply.

7. Outbound redaction is not wired into execution. `src/guard.rs:34` defines `TextDelta`, but nothing in `src/agent.rs` or `src/llm/openai.rs` runs guard checks on streamed model tokens. `src/agent.rs:263-269` also persists raw tool output before any redaction pass. Secrets can therefore stream to the client and hit disk unredacted even though the codebase has tests implying "both directions" redaction.

8. `src/llm/openai.rs:220-236` still falls back to `"unknown"` for function call IDs and names on `response.output_item.done`. VISION explicitly says not to do this because it creates ghost tool entries. The parser test suite enforces the rule for one event type, then violates it in the actual streaming path for another.

### High

9. `src/server.rs` is not actually queue-driven server execution. `POST /api/sessions/:id/messages` only enqueues; nothing consumes those rows. The only live execution path is the WebSocket bypass above. That means the HTTP API is half-built and the comment `queue-driven agent execution` at `src/server.rs:1` is false.

10. `src/context.rs:67-142` defines a `History` context source, but `src/turn.rs:124-127` stores it immutably inside `Turn`, and no production code ever calls `History::set_history`. The real history path is `src/agent.rs:148`, which copies `session.history()` directly. `History` is effectively dead production code and a misleading abstraction.

11. `src/session.rs:232-264` only reloads today's JSONL file. Long-lived sessions forget every prior day on restart, which is a poor fit for the stated crash/resume model. The data is on disk, but the runtime does not actually resume the session history it claims to persist.

12. `src/session.rs:147-177` drops all persisted `system` entries on load. That means approval denials, hard denials, and any future system-side annotations vanish after restart. Even if that is intentional for prompt hygiene, it is not documented, and it makes persistence semantically lossy.

13. `src/main.rs:190-194` silently marks any non-`user` queued message as processed and throws it away. For a system that claims "one inbox, any source", that is brittle. The queue schema allows arbitrary roles; the worker path only meaningfully supports one.

14. `src/context.rs:28-31` and `src/identity.rs:9-27` will silently fall back to a generic config prompt if layered identity files are missing or unreadable. Given the design emphasis on constitution/identity/context, swallowing identity failures and continuing with a stub prompt is architecturally suspect.

15. `src/auth.rs:232-242` writes bearer and refresh tokens with default filesystem permissions. There is no explicit `0600` mode or permission check. On a permissive umask, auth secrets land more broadly readable than they should.

### Medium

16. `src/agent.rs:164-196` has broken inbound `Deny`/`Approve` control flow. A denied inbound message is appended, then the loop `continue`s with the same persisted user message still in history. If an inbound guard ever actually denies or requests approval, this can loop forever or repeatedly re-prompt. It happens not to fire today because the shipped inbound guards only redact.

17. `src/agent.rs:179-190` chooses the first text block in the whole assembled message list when asking for inbound approval. If that path ever executes, it is likely to show the system prompt, not the offending user input.

18. `src/tool.rs:101-105` claims the `command` argument must be non-empty, but it only checks for presence and string type. `""` is accepted.

19. `src/server.rs:273-275` ignores the result of `enqueue_message()` for WebSocket messages and proceeds to execute anyway. If the store write fails, execution and persistence diverge immediately.

20. `src/llm/openai.rs:703-793` parses SSE line-by-line and ignores non-`TextDelta` residual buffered events at stream end. That is probably fine for normal termination, but it is another place where partially delivered tool-call events can be silently lost.

21. `src/llm/mod.rs:23-25` and `src/session.rs` carry `reasoning_trace`, but `src/llm/openai.rs` never populates it. The field is effectively a stub.

22. `src/server.rs:339-349` accepts API keys via query string for WebSocket upgrades. That was apparently deliberate, but it still means secrets may leak into logs and intermediaries. At minimum it should be treated as a compromise path, not a peer to header auth.

## File-by-File Review

### `src/main.rs`

- Match to VISION: Partial. CLI and server entrypoints share `build_default_turn()`, which is good, but the queue model is inconsistent across execution paths.
- Architecture: `process_queue()` is the only real queue consumer, and it only exists in the CLI path. The server does not use it.
- Correctness: `src/main.rs:86-99` creates a fresh session ID on every CLI run, so `history.load_today()` is effectively dead for CLI restart/resume.
- Correctness: `src/main.rs:190-194` drops non-user messages on the floor.
- Built vs incomplete: REPL and one-shot are built; durable session continuation for CLI is not meaningfully built.

### `src/agent.rs`

- Match to VISION: Partial. The turn loop, streaming, and tool replay are present, but the guard pipeline is only applied inbound and on tool-call requests, not on outbound token flow or tool results.
- Security: `src/agent.rs:205-206` streams tokens straight to the sink with no `TextDelta` guard check.
- Security: `src/agent.rs:263-269` persists raw tool output before any redaction pass.
- Correctness: `src/agent.rs:164-196` can spin forever on inbound `Deny` or inbound approval refusal.
- Correctness: `src/agent.rs:327` in the ignored test constructs an assistant reply as `ChatMessage::system("ok")`, which is a bad sign: even the tests are muddled about role semantics.
- Built vs incomplete: approval handling exists, but only the tool-call path is realistically live.

### `src/auth.rs`

- Match to VISION: Mostly yes. OAuth device flow and refresh are real, not stubbed.
- Security: token storage permissions are weak. `save_tokens()` uses plain `std::fs::write`.
- Architecture: auth is tightly coupled to local filesystem state and hardcoded endpoints/client ID. That is acceptable for MVP, but it is not abstracted.
- Completeness: `AuthorizationResponse.code_challenge` is dead data in this runtime.
- Correctness: no major logic bug jumped out in the happy path.

### `src/config.rs`

- Match to VISION: Partial. It loads model and reasoning effort, but the fallback `system_prompt` path undercuts the layered identity model.
- Architecture: config owns a generic prompt string even though the real design centers on identity files. That fallback is what makes missing identity silently non-fatal.
- Built vs incomplete: basic config is built; the identity-v2 surface is rightly absent and should not be counted as a bug.
- Correctness: no major parsing bug.

### `src/context.rs`

- Match to VISION: Weak. `Identity` roughly matches v1. `History` does not match the actual runtime architecture because it is never fed session history in production.
- Architecture: `History` is dead production code. `set_history()` only appears in tests.
- Architecture: `ContextSource::name()` is unused.
- Correctness: `History::name()` returns `"context"` instead of `"history"`, which is minor but sloppy.
- Security: silent fallback from identity load failure to generic prompt happens here.
- Built vs incomplete: `Identity` is real; `History` is effectively a test helper masquerading as a runtime abstraction.

### `src/guard.rs`

- Match to VISION: Partial. There is a real guard pipeline with ordering semantics, but the security value is overstated because the checks are narrow heuristics and some paths are never invoked.
- Security: `ShellSafety` only inspects the top-level shell words. `python -c`, `perl -e`, `node -e`, shell substitution, destructive redirection, and countless other dangerous forms sail through.
- Security: `ExfilDetector` only recognizes a tiny set of reads and send paths. It misses ordinary exfil channels like `scp`, `rsync`, `python requests`, `openssl s_client`, `git push`, etc.
- Security: the presence of `TextDelta` is misleading because nothing calls it outside tests.
- Correctness: the tests at `src/guard.rs:527-538` claim "both directions" redaction, but they just run the inbound path twice. The suite overstates what is implemented.
- Built vs incomplete: the pipeline is built; its enforcement surface is incomplete and its tests are overconfident.

### `src/identity.rs`

- Match to VISION: Matches identity v1, not v2, which is fine.
- Architecture: good minimal loader, but it contributes to a bad failure mode because callers can swallow its errors and keep running.
- Correctness: no major bug here.
- Built vs incomplete: real v1 implementation; no operator layer yet, which is roadmap, not a defect.

### `src/lib.rs`

- Match to VISION: Neutral. This is just module re-exports.
- Architecture: no issue.
- Built vs incomplete: no issue.

### `src/llm/mod.rs`

- Match to VISION: Mostly yes. The shared types are coherent.
- Architecture: `TurnMeta.reasoning_trace` exists without a real producer in the OpenAI backend, so the abstraction is ahead of the implementation.
- Correctness: no direct bug here.
- Built vs incomplete: `reasoning_trace` is stub territory.

### `src/llm/openai.rs`

- Match to VISION: Mixed. SSE parsing and tool-call accumulation are substantial, but there are serious prompt and tool-ID handling bugs.
- Correctness: `src/llm/openai.rs:64-71` collapses all system messages to the last one and can discard the real governing prompt.
- Correctness: `src/llm/openai.rs:220-236` fabricates `"unknown"` IDs/names, violating the design rule against ghost tool entries.
- Correctness: `src/llm/openai.rs:742` also invents `"unknown"` names when tool-call deltas finish without a captured name.
- Security: streamed output is forwarded raw; no secret redaction hook is applied in provider or caller.
- Completeness: `reasoning_trace` is not populated.
- Built vs incomplete: SSE support is real; some edge-case handling is half-done and directly contradicts the documented pitfalls.

### `src/server.rs`

- Match to VISION: Poor. This file claims queue-driven execution but implements a WebSocket side channel that bypasses the queue and leaves it inconsistent.
- Security: `WsAutoApprove` is an explicit approval bypass.
- Security: query-param API key auth is a compromise path with leak risk.
- Correctness: WebSocket messages are enqueued but never marked processed.
- Correctness: HTTP-enqueued messages are never consumed by any worker.
- Architecture: the server owns the only recovery path (`recover_stale_messages()`), but because WS traffic never transitions to `processing`, crash recovery is not actually helping the live path.
- Built vs incomplete: health/session/message endpoints exist; actual queue-driven agent execution is incomplete.

### `src/session.rs`

- Match to VISION: Partial. JSONL persistence exists, but the resume and trimming behavior are much weaker than advertised.
- Correctness: `src/session.rs:274-314` trims the wrong end of history whenever there is no leading system prompt.
- Correctness: removing two messages at a time is not safe for assistant/tool/tool-result sequences.
- Correctness: `src/session.rs:232-264` only reloads today's file, so persistence across days is not real session replay.
- Correctness: `src/session.rs:147-177` drops system entries on load, making persistence lossy.
- Architecture: token accounting mixes provider metadata totals with tokenizer estimates and assumes `message_tokens` stays aligned to `messages`; that alignment is brittle.
- Built vs incomplete: persistence is built, but robust replay semantics are not.

### `src/store.rs`

- Match to VISION: Partial. The queue/store itself is real and reasonably small.
- Architecture: a single `rusqlite::Connection` behind a mutex keeps things simple but serializes all store access. That is acceptable for MVP, not elegant.
- Correctness: `recover_stale_messages()` blindly resets every `processing` row, with no age check. In a future multi-worker setup this will be wrong immediately.
- Bigger issue: the file is fine in isolation; the real problem is that the server does not actually use it as the source of truth for execution.
- Built vs incomplete: built, but underused by the server path.

### `src/template.rs`

- Match to VISION: Fine for v1 template vars.
- Architecture: intentionally tiny.
- Correctness: no concrete bug. The implementation is naive but adequate for literal `{{var}}` replacement.
- Built vs incomplete: built.

### `src/tool.rs`

- Match to VISION: Only partially. Async execution and timeout exist, but the "sandbox" and descendant kill guarantees are weaker than claimed.
- Security: RLIMITs are not a meaningful isolation boundary for a hostile shell command.
- Correctness: timeout cleanup does not guarantee descendant termination.
- Correctness: empty commands are accepted despite the error text.
- Architecture: the tool always returns one big inline string, which is simple, but it means every caller downstream must deal with unbounded output until the roadmap's file-backed cap lands.
- Built vs incomplete: async shell execution is built; safe containment is not.

### `src/turn.rs`

- Match to VISION: Partial. It correctly centralizes turn construction, tool definitions, and guard resolution.
- Architecture: `build_default_turn()` is canonical, which is good. But it wires in a dead `History` context source and therefore overstates how composable context assembly currently is.
- Architecture: `resolve_verdict()` matches the intended precedence rules.
- Correctness: `check_inbound()` uses "message count changed" as its only signal that context assembly modified the prompt. Context sources that mutate in place are invisible to that flag.
- Built vs incomplete: the abstraction is mostly real, but one of its core context components is a no-op in production.

### `src/util.rs`

- Match to VISION: Fine.
- Correctness: `utc_timestamp()` uses `unwrap_or_default()` if the clock is before epoch, silently fabricating `1970-01-01T00:00:00Z`. That is minor but not ideal.
- Built vs incomplete: built.

## Bottom Line

The codebase is compact and readable, and several core pieces are real: the agent loop, OpenAI SSE parsing, SQLite queue, OAuth flow, and JSONL persistence are not stubs. The trouble is that the runtime's most important guarantees break at the seams:

- the server does not actually run from the queue,
- approval is bypassed over WebSocket,
- the system prompt can be replaced by later operational messages,
- session trimming and replay are not trustworthy for long-running sessions,
- and the shell "sandbox" is nowhere near strong enough to justify confidence.

The project is not failing because the roadmap is unfinished. It is failing where existing code already claims stronger semantics than it actually delivers.
