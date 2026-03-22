# Phase 1 Refactor Plan

Scope is only the gate/CLI extraction described in `ANALYSIS.md`.
Do not touch the later-phase splits (`session/`, `server/`, `llm/`, `store/`, stringly-typed enums, provider factory dedup) beyond import-path fallout.

## Files To Read

### Docs
- `ANALYSIS.md:1-460`
- `docs/current/risks.md:1-58`
- `docs/current/architecture.md:1-93`
- `docs/roadmap.md:1-60`

### Source
- `src/lib.rs:1-14`
- `src/agent.rs:1-2020`
- `src/guard.rs:1-675`
- `src/turn.rs:1-351`
- `src/main.rs:1-213`
- `src/server.rs:1-1110`
- `src/config.rs:1-264`
- `src/context.rs:1-383`
- `src/identity.rs:1-165`
- `src/llm/mod.rs:1-185`
- `src/llm/openai.rs:1-948`
- `src/session.rs:1-956`
- `src/store.rs:1-297`
- `src/template.rs:1-86`
- `src/tool.rs:1-322`
- `src/util.rs:1-95`
- `src/auth.rs:1-401`

## What To Change

### `src/lib.rs`
- Replace `pub mod guard;` with `pub mod gate;` and add `pub mod cli;`.
- Keep the existing public module list otherwise unchanged.
- Do not leave `guard` as the primary export path after the migration; all internal call sites should move to `crate::gate`.

### `src/gate/mod.rs`
- Define the shared gate types here: `Severity`, `Verdict`, `GuardEvent`, and `Guard`, and keep them public so `Turn::guard` and any external callers can still name them.
- Re-export the gate implementations from the submodules: `SecretRedactor`, `ShellSafety`, and `ExfilDetector`.
- Re-export the shared secret catalog from `secret_patterns.rs` so `turn.rs` and the scanner can use one source of truth.
- Add `guard_text_output(turn, text)` here as a thin wrapper around `Turn::check_text_delta`; it returns `String::new()` on `Verdict::Deny` and returns the filtered text for `Allow`, `Modify`, or `Approve`.
- Add `guard_message_output(turn, message)` here; it should only touch `MessageContent::Text`, call `guard_text_output` on each text block, and then drop any empty text blocks while leaving tool-call and tool-result blocks intact.
- Re-export the internal helpers needed by `agent.rs`: `StreamingTextBuffer`, `cap_tool_output`, `safe_call_id_for_filename`, and `DEFAULT_OUTPUT_CAP_BYTES` with crate-private visibility.

### `src/gate/secret_patterns.rs`
- Add a single shared secret catalog for the three secret families currently duplicated across `turn.rs`, `guard.rs`, and `agent.rs`.
- Use named consts at the top of the file for every raw literal:
  - `SECRET_PATTERN_COUNT`
  - `OPENAI_SECRET_PREFIX`, `OPENAI_SECRET_REGEX`, `OPENAI_SECRET_MIN_SUFFIX_LEN`
  - `GITHUB_PAT_PREFIX`, `GITHUB_PAT_REGEX`, `GITHUB_PAT_SUFFIX_LEN`
  - `AWS_ACCESS_KEY_PREFIX`, `AWS_ACCESS_KEY_REGEX`, `AWS_ACCESS_KEY_SUFFIX_LEN`
- Define `SecretBodyKind` with variants for the three byte classes the streaming scanner needs: OpenAI token bytes, lowercase alphanumeric bytes, and uppercase alphanumeric bytes.
- Define `SecretSuffixLen` with `Minimum(usize)` and `Exact(usize)` so the streaming scanner does not own its own hardcoded thresholds.
- Define `SecretPattern` with `prefix`, `regex`, `body_kind`, and `suffix_len`.
- Define `SECRET_PATTERNS` in scanner order: OpenAI, GitHub PAT, AWS key.
- Keep this catalog crate-private unless a later phase needs it as public API.

### `src/gate/secret_redactor.rs`
- Move `SecretRedactor` here from `guard.rs`, including its `id` field, compiled regex list, `redact_text`, `redact_messages`, and `Guard` implementation.
- Keep `SecretRedactor` public so existing turn composition code can continue to construct it directly.
- Keep the current `new(patterns: &[&str])` constructor for ad hoc test coverage, and add a `from_catalog(&[SecretPattern])` constructor for the shared default pipeline.
- Keep the redaction marker as a named const at the top of the file.
- Keep `Default` for `SecretRedactor` only if you need it for tests; otherwise do not add new API surface.
- This file should own the text/inbound redaction behavior only; it should not know about streaming buffers or output capping.

### `src/gate/shell_safety.rs`
- Move `ShellSafety`, `ShellDefaultAction`, the policy parsing helpers, the glob matcher, and the `Guard` implementation here from `guard.rs`.
- Keep `ShellSafety` public so the turn builder and any external users can still name it directly.
- Put every moved literal at the top as named consts:
  - the guard id
  - the default action strings
  - the severity strings
  - the command field name
  - the allowlist-miss reason string
- Keep `ShellSafety::new()` and `ShellSafety::with_policy(policy: ShellPolicy)` exactly as the public entry points.
- Keep the `ShellPolicy` dependency on `crate::config` unchanged; the new module should still read policy values from `agents.toml`.

### `src/gate/exfil_detector.rs`
- Move `ExfilDetector`, its `Guard` implementation, and the command parsing helpers here from `guard.rs`.
- Keep `ExfilDetector` public for the same reason as the other guard types.
- Put the moved literals at the top as named consts or const arrays:
  - the guard id
  - the sensitive-read path fragments
  - the send-path fragments
  - the approval reason string
- Keep the current behavior of skipping commands that cannot be parsed, and only returning `Approve` when one batch contains both a sensitive read and a send path.

### `src/gate/streaming_redact.rs`
- Move the streaming secret buffer and the secret-prefix scanning logic here from `agent.rs`.
- Keep the buffer crate-private; `agent.rs` and the streaming tests are the only callers.
- Remove the `&Turn` dependency entirely.
- The new API must accept a callback or trait object that redacts a segment before emission. Use this signature:

```rust
pub(crate) fn push<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E, token: String)
where
    R: FnMut(String) -> String + ?Sized,
    E: FnMut(String) + ?Sized;

pub(crate) fn finish<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E)
where
    R: FnMut(String) -> String + ?Sized,
    E: FnMut(String) + ?Sized;
```

- `agent.rs` should own the closure that captures `&Turn` and passes each emitted segment through `guard_text_output`.
- `agent.rs` should also own the tiny emission adapter that forwards each token to `TokenSink::on_token`; the buffer must not import `TokenSink` from `agent.rs`.
- The scanner must consume `SECRET_PATTERNS` and `SecretBodyKind` from `secret_patterns.rs`; it must not duplicate the `sk-`, `ghp_`, or `AKIA` tables.
- Keep the buffer state private to this module unless a test needs to instantiate it directly.
- Put every moved literal at the top as named consts; do not leave raw `20`, `36`, or `16` thresholds in the scanner.

### `src/gate/output_cap.rs`
- Move `safe_call_id_for_filename`, `cap_tool_output`, and the output-cap threshold here from `agent.rs`.
- Keep these helpers crate-private; only `agent.rs` and the output-cap tests should call them.
- Put every moved literal at the top as named consts:
  - the default inline cap bytes
  - the results directory name
  - the `call_` prefix
  - the empty-call fallback suffix
  - the hex width used when escaping unsafe bytes
  - the KB divisor
  - the `sed -n` preview start and end lines
- Keep the file-write semantics unchanged: always create `sessions/{id}/results`, always write the full output to a file, and only inline the summary when `output.len() > threshold`.
- Keep the path-sanitization behavior exactly the same as today: ASCII alnum, `-`, and `_` stay literal; every other byte is hex-escaped into the filename.

### `src/cli.rs`
- Move `CliTokenSink`, `CliApprovalHandler`, and `format_denial_message` here from `agent.rs`.
- Keep `CliTokenSink`, `CliApprovalHandler`, and `format_denial_message` public so the binary entrypoint can import them.
- Keep the `TokenSink` and `ApprovalHandler` traits in `agent.rs`; the CLI types should implement those traits from the new module.
- Put every moved literal at the top as named consts:
  - the low/medium/high approval prefixes
  - the prompt text
  - the accepted yes-response token
- Keep the sink behavior unchanged: token sink writes to stdout and flushes, approval handler writes the prompt to stderr and reads one line from stdin, and both return failure cleanly if I/O fails.
- `format_denial_message` should remain a pure formatting helper so `main.rs` and `server.rs` can share it.

### `src/agent.rs`
- Remove `CliTokenSink`, `CliApprovalHandler`, `format_denial_message`, `guard_text_output`, `guard_message_output`, the streaming secret prefix state machine, `StreamingTextBuffer`, `safe_call_id_for_filename`, `cap_tool_output`, and `DEFAULT_OUTPUT_CAP_BYTES` from this file.
- Drop the now-unneeded `std::fs`, `std::io`, and `std::path::Path` imports once those helpers move out.
- Keep `TokenSink`, `ApprovalHandler`, `TurnVerdict`, `QueueOutcome`, `command_from_tool_call`, `append_approval_denied`, `append_hard_deny`, `make_denial_verdict`, `run_agent_loop`, `process_message`, and `drain_queue` here.
- Update `run_agent_loop` so it:
  - constructs the new `StreamingTextBuffer`
  - passes a `FnMut(String) -> String` redaction callback that closes over `&Turn`
  - passes a separate token-emission callback that forwards to the selected `TokenSink`
  - calls `guard_message_output(turn, &mut turn_reply.assistant_message)` before persisting the assistant message
  - calls `guard_text_output(turn, result)` before `cap_tool_output(...)`
- Import the moved helpers from `crate::gate` instead of using local copies.
- Keep the loop/queue integration tests here only if they validate orchestration, persistence, or denial flow end to end; move the streaming and cap-specific tests out.

### `src/turn.rs`
- Replace every `crate::guard` import with `crate::gate`.
- Update `build_default_turn` so the default secret redactor is built from the shared catalog, not from inline regex literals.
- Keep `resolve_verdict` here; it is still the guard-ordering coordinator for turn execution.
- Update the tests to use `crate::gate` imports and the shared secret catalog instead of repeating raw secret regex strings.

### `src/main.rs`
- Add `cli` to the library imports.
- Replace `agent::CliTokenSink` with `cli::CliTokenSink`.
- Replace `agent::CliApprovalHandler` with `cli::CliApprovalHandler`.
- Replace `agent::format_denial_message` with `cli::format_denial_message`.
- Leave the REPL loop, `resolve_session_id`, and the auth/serve subcommands in place.

### `src/server.rs`
- Replace every `crate::guard` reference with `crate::gate`.
- Replace the denial-message formatting calls with `crate::cli::format_denial_message`.
- Keep `WsTokenSink`, `NoopTokenSink`, `RejectApprovalHandler`, and `WsApprovalHandler` here; they still implement the `agent` traits.
- Update the test-only `NeedsApproval` guard in the server tests to use `crate::gate::{Guard, GuardEvent, Severity, Verdict}`.

### `src/guard.rs`
- Delete the file after the new `gate/` modules are wired in and all call sites have been switched.
- Do not leave a partial compatibility shim in this phase unless you explicitly want to preserve the old public path; the default plan is to finish with only `gate/`.

## Cycle Fix For `StreamingTextBuffer`

The cycle to break is:
- `turn.rs` needs the gate abstractions.
- `StreamingTextBuffer` currently needs `Turn` so it can call `guard_text_output`.
- `Turn` depends on `Guard`, `Verdict`, and `GuardEvent`.

The fix is to move the `Turn`-aware bit out of the buffer and into the caller.

The exact shape should be:
- `gate/streaming_redact.rs` owns `StreamingTextBuffer`.
- `StreamingTextBuffer::push` and `StreamingTextBuffer::finish` accept a redaction callback plus a plain `FnMut(String)` token-emission callback, not `TokenSink` and not `Turn`.
- `agent.rs` creates the closure that captures `&Turn` and forwards each emitted segment through `guard_text_output(turn, segment)`.
- `agent.rs` also creates the emission adapter that calls `token_sink.on_token(token)`; the buffer itself must not import the `TokenSink` trait.
- `gate/streaming_redact.rs` never imports `Turn`.
- `gate/mod.rs` can still own `guard_text_output(turn, ...)` because that helper is a thin gate wrapper; the buffer itself must not call it directly.

## What Tests To Write Or Move

### `src/gate/secret_redactor.rs`
- Move `redacts_openai_api_key`, `redacts_github_pat`, `redacts_aws_key`, `preserves_normal_text`, `redacts_in_both_directions`, `redacts_multiple_secrets_in_one_message`, and `text_delta_is_redacted_when_modified` from `src/guard.rs`.
- Add one direct `ToolResult` coverage test so the `MessageContent::ToolResult` branch stays covered after extraction.
- Rewrite the tests to source the default patterns from `SECRET_PATTERNS` instead of repeating the regex literals.

### `src/gate/shell_safety.rs`
- Move `invalid_command_json_falls_back_to_default_policy`, `default_config_approves_unmatched_command`, `deny_pattern_takes_precedence_over_allow_pattern`, `deny_pattern_blocks_matching_command`, `allow_pattern_allows_matching_command`, `unmatched_command_approves_when_default_is_approve`, and `unmatched_command_allows_when_default_is_allow`.
- Keep the local `shell_policy(...)` helper in this file so the policy permutations remain easy to read.

### `src/gate/exfil_detector.rs`
- Move `catches_piped_exfiltration`, `allows_safe_batch`, `detects_read_then_curl`, `detects_read_sensitive_then_network`, and `single_command_no_exfiltration`.
- Add one test for the no-command-json path if you want direct coverage of the `continue` branch.

### `src/gate/streaming_redact.rs`
- Move `outbound_redaction_is_streamed_and_persisted_before_session_write`, `outbound_text_is_streamed_incrementally_to_sink`, `on_complete_is_only_emitted_after_final_stop`, `outbound_secret_split_across_tokens_is_redacted_before_sink`, and `outbound_fixed_length_secret_prefixes_are_redacted_before_sink`.
- Rewrite them as direct `StreamingTextBuffer` unit tests that use the new callback-based API, not `&Turn`.
- Add one new test that instantiates `StreamingTextBuffer` with a plain callback like `|segment| segment` and a simple `Vec<String>` sink closure to prove the buffer no longer needs `Turn` or `TokenSink` at all.
- Keep the secret-pattern inputs sourced from the shared catalog, not from inline copies.

### `src/gate/output_cap.rs`
- Move `tool_output_below_threshold_is_inline_and_saved_to_file`, `cap_tool_output_sanitizes_call_id_before_path_use`, `cap_tool_output_creates_results_directory`, and `tool_output_above_threshold_is_capped_with_metadata_pointer`.
- Rewrite them as direct `cap_tool_output(...)` and `safe_call_id_for_filename(...)` tests instead of driving the whole agent loop.
- Keep one integration test in `src/agent.rs` for the full agent-loop redaction path; the cap helper itself should be tested here.

### `src/gate/mod.rs`
- Add direct tests for `guard_text_output` and `guard_message_output`.
- Verify the `Deny` path returns an empty string, and verify `guard_message_output` removes empty text blocks while keeping non-text blocks intact.

### `src/cli.rs`
- Add a pure formatting test for `format_denial_message`.
- If you extract a private `severity_prefix` helper, add a small table-driven test for low/medium/high prefixes.

### `src/turn.rs`
- Keep `empty_turn_allows_everything`, `guard_events_run_in_configuration_order`, `validate_gates_short_circuit_on_deny`, `deny_beats_approve`, `approve_beats_allow`, and `full_turn_builds_complete_context`.
- Update the shared-secret test input in `full_turn_builds_complete_context` to use the catalog from `gate/secret_patterns.rs` instead of repeating the regex literal.

### `src/agent.rs`
- Keep the orchestration tests that validate queue draining, session persistence, inbound redaction persistence, inbound denial, context insertion, and the denial counter.
- Keep `tool_output_is_redacted_before_persist` here because it exercises the end-to-end agent loop, not the output-cap helper.
- Remove the streaming and output-cap tests from this file once they have been moved.

### `src/server.rs`
- Keep the existing server integration tests.
- Update their imports from `crate::guard` to `crate::gate` and from `agent::format_denial_message` to `cli::format_denial_message`.

### `src/main.rs`
- Keep the `resolve_session_id` tests unchanged.
- No new tests are required here beyond the import-path update.

## Order Of Operations

1. Add `src/gate/mod.rs`, `src/gate/secret_patterns.rs`, `src/gate/secret_redactor.rs`, `src/gate/shell_safety.rs`, `src/gate/exfil_detector.rs`, and `src/cli.rs` while leaving the old `guard.rs` and `agent.rs` helpers in place for the moment; wire `src/lib.rs` to export the new modules.
2. Move the `src/guard.rs` unit tests into the matching gate submodules and switch `turn.rs` to `crate::gate` plus the shared secret catalog.
3. Move the CLI presentation helpers into `src/cli.rs`, then switch `main.rs` and `server.rs` to import `CliTokenSink`, `CliApprovalHandler`, and `format_denial_message` from `cli`, and switch every remaining `Severity` import to `gate`.
4. Extract `StreamingTextBuffer` into `gate/streaming_redact.rs` and `safe_call_id_for_filename`/`cap_tool_output` into `gate/output_cap.rs`, then switch `agent.rs` to use them through the new callback signature.
5. Move `guard_text_output` and `guard_message_output` into `gate/mod.rs` and update `agent.rs` to call the new location.
6. Delete `src/guard.rs`, remove its module export from `src/lib.rs`, and sweep the remaining imports/tests so every call site uses `crate::gate`.
7. Finish by running `cargo test`, `cargo build --release`, `cargo fmt --check`, and `cargo clippy -- -D warnings`; run `cargo test --features integration` as well if auth credentials are available.

Each step should compile and pass tests before the next one starts.

## Risk Assessment

- The biggest failure mode is a new cycle or a borrow issue in the streaming refactor; if `StreamingTextBuffer` still sees `Turn`, the split did not really happen.
- The shared secret catalog can drift if `turn.rs`, `secret_redactor.rs`, and `streaming_redact.rs` still carry their own literals.
- `safe_call_id_for_filename` must remain strict; any regression there can turn a provider-controlled call id into a path traversal or shell suggestion issue.
- Moving `format_denial_message` and the CLI sink/handler out of `agent.rs` changes import paths in both `main.rs` and `server.rs`; miss one and the build will fail late.
- Removing `guard.rs` is a public API break if external code imports `autopoiesis::guard`; that is acceptable only if the phase is explicitly allowed to break the old path.
- The known risks in `docs/current/risks.md` still apply after the split, especially queue atomicity and the OpenAI SSE trailing-buffer bug; this phase does not fix those.
