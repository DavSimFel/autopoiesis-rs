# Tracing Migration Plan

## 1. Files read
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/architecture/overview.md`
- `src/agent.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/gate/budget.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/secret_patterns.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/main.rs`
- `src/principal.rs`
- `src/server.rs`
- `src/session.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact changes per file
- `Cargo.toml`: add `tracing = "0.1"` and `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }`; keep default features on so layered subscriber composition stays available.
- `src/util.rs`: add the shared library-owned tracing target constants/helpers used by `src/main.rs`, `src/auth.rs`, and `src/cli.rs`, so the binary and library code reference one source of truth instead of repeating target strings; if needed, also place tiny writer-seam helpers here when that keeps test-only plumbing out of business logic.
- `src/main.rs`: replace the simple startup plan with a concrete `init_tracing()` helper that builds three layers: a diagnostic layer to stderr controlled by `RUST_LOG`, a message-only stdout layer filtered to the shared dedicated stdout target, and a message-only stderr layer filtered to the shared dedicated stderr target; import those targets from the library module instead of defining them in the binary crate; explicitly exclude the dedicated user-output targets from the diagnostic layer so user-visible lines do not get duplicated; replace every existing `println!/eprintln!` site in `main.rs` with the appropriate target and level so subscription listings, utilization summaries, auth command summaries, stale-queue recovery notices, and CLI denials still show up even when `RUST_LOG=warn`; keep the REPL `print!("> ")` prompt unchanged.
- `src/agent.rs`: add a per-agent-turn span around `run_agent_loop`; log turn lifecycle at `info`, approval/tool state transitions at `debug`, policy denials at `warn`; replace the queue-drain `eprintln!` sites with diagnostic tracing macros only, not user-output targets; do not change queue, approval, or persistence behavior.
- `src/turn.rs`: instrument guard evaluation with a span per inbound/tool/text check and debug logs per guard result; warn when a deny verdict wins; leave `resolve_verdict()` precedence and mutation behavior untouched.
- `src/tool.rs`: add a shell execution span covering `execute()` and `run_with_timeout()`; debug-log spawn, timeout selection, exit, and truncation state; warn on timeout cleanup failures; keep process-group, timeout, and output-capture logic identical.
- `src/server.rs`: add spans for each HTTP handler, each websocket upgrade/session, each queue drain, and auth middleware; log server bind/recovery at `info`, recoverable failures and denials at `warn`, queue/session transitions at `debug`; replace all `eprintln!` sites with diagnostic tracing macros only because HTTP/WS clients already receive their own protocol responses.
- `src/llm/openai.rs`: add `trace` logs for SSE chunk receipt, parsed event kind, and trailing-buffer handling; add lightweight `debug` logs for request setup (model, tool count, reasoning enabled); avoid logging raw prompt text, raw tool output, or full SSE payloads.
- `src/session.rs`: replace the malformed tool-entry `eprintln!` with `warn!`; add `trace` logs for JSONL replay progress and `debug` logs around trimming/budget snapshots; avoid logging raw line contents and log structural data only.
- `src/context.rs`: replace the identity fallback `eprintln!` with `warn!`.
- `src/gate/shell_safety.rs`: replace the standing-approval `eprintln!` audit line with a `debug!` event carrying the matched pattern and taint state; keep policy decisions identical.
- `src/auth.rs`: replace `println!/eprintln!` with tracing macros routed through the shared dedicated user-output target helpers from the library module so the original stream semantics are preserved without duplicating target strings; keep the progress-dot `print!(".")` behavior unchanged; remove bare blank-line `println!()` calls by emitting blank message-only events on the dedicated target so the OAuth flow still renders cleanly without diagnostic prefixes.
- `src/cli.rs`: replace `println!/eprintln!` with a split strategy instead of generic diagnostic logs: `CliTokenSink::on_complete()` emits a blank message through the shared dedicated stdout target helper so it remains a plain newline, approval banners and denial text use the shared dedicated stderr target helper, and flush/read failures become diagnostic `warn!`; add a small CLI I/O seam for tests, either via writer-and-reader-injectable constructors or via extracted pure rendering helpers plus a narrow input abstraction, so end-to-end output assertions do not require brittle process-wide stdout/stderr/stdin hacks; keep `print!`/`eprint!` token streaming and approval prompt input behavior unchanged in the default path.
- No edits are planned for the remaining read files unless compile fixes require import cleanup only.

## 3. What tests to write
- `src/main.rs`: add focused tests around the extracted tracing-init helper with captured stdout/stderr writers; assert that `RUST_LOG=warn` suppresses diagnostic `info!` events, does not suppress `autopoiesis.stdout` or `autopoiesis.stderr` events, and does not duplicate those user-output events into the diagnostic sink.
- `src/main.rs`: add a test that a blank event on the dedicated stdout target renders exactly one newline and nothing else; this is the regression test for `CliTokenSink::on_complete()`.
- `src/cli.rs`: add end-to-end output-capture tests for the real mixed path, not just helper routing, using the CLI I/O seam instead of process-global stdout/stderr/stdin redirection: assert that `on_token("abc"); on_complete()` yields `abc\n` in order on stdout, and that approval-banner emission still appears before the prompt on stderr without blocking on real stdin; if small helpers are extracted for approval-output emission, also test that approval banners go to the dedicated stderr target and flush/read failures stay on the diagnostic path.
- `src/util.rs`: add small tests for the shared target constants/helpers so all caller modules stay aligned on the same dedicated targets.
- `src/auth.rs` and `src/main.rs`: if small helpers are extracted for auth/subcommand/user-output emission, add tests that login/status/logout/subscription messages map to the correct dedicated target and remain visible with restrictive `RUST_LOG`.
- `src/turn.rs`: if helper functions are introduced for event labeling or guard-span setup, add unit tests that their inputs map to the expected event labels while existing verdict-precedence tests continue to prove no behavioral drift.
- `src/session.rs`: if a helper is added for trace-safe JSONL logging, add a unit test that it logs structural metadata only and does not expose raw content.
- `src/llm/openai.rs`: extend existing SSE tests only if helper extraction changes control flow; assertions stay on parser behavior, not on formatted logs.
- `src/server.rs`: add a small unit test only if request/WS span naming or principal labeling is factored into helpers; keep assertions on helper outputs, not subscriber formatting.
- Existing behavior tests in `src/agent.rs`, `src/turn.rs`, `src/tool.rs`, `src/server.rs`, `src/session.rs`, and `src/llm/openai.rs` remain the main guard against business-logic drift and must keep passing unchanged.
- Full regression checks remain mandatory: `cargo build --release`, `cargo test`, `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test --features integration` when auth is available.

## 4. Order of operations
1. Add the two tracing dependencies in `Cargo.toml`.
2. Add the shared target constants/helpers in `src/util.rs` first so both the library modules and `src/main.rs` have a valid compile-time home for them.
3. Build `init_tracing()` next in `src/main.rs`, including the dedicated stdout/stderr user-output layers and the diagnostic-layer exclusion for those targets.
4. Add the subscriber/output-capture tests for the layered init helper, the shared target helpers, and the `src/cli.rs` writer seam before migrating any user-visible `println!/eprintln!` sites.
5. Migrate backend-only diagnostics and spans next in `src/context.rs`, `src/session.rs`, `src/gate/shell_safety.rs`, `src/agent.rs`, `src/turn.rs`, `src/tool.rs`, and `src/server.rs`; these changes are observational and should keep the existing behavior tests green.
6. Migrate `src/main.rs`, `src/auth.rs`, and `src/cli.rs` last, using the already-tested shared dedicated user-output helpers and writer seam so user-visible output does not silently change under `RUST_LOG`.
7. Add the noisier `trace` instrumentation in `src/llm/openai.rs` and the structural JSONL tracing in `src/session.rs` after the rest compiles, because those are the hottest paths and easiest places to leak too much detail.
8. Run targeted tests for any extracted helpers, then run the full required check set.
9. If pre-commit hooks or stats generation touch docs, review the diff and keep only the expected mechanical updates.

## 5. Risk assessment
- Highest risk: trace logging can leak secrets if raw SSE lines, raw JSONL entries, or raw prompt/tool payloads are logged. Mitigation: log event type, sizes, counters, call IDs, file paths, and line numbers only; do not log full content strings.
- High risk: user-visible output can be suppressed or redirected if it rides the same `EnvFilter`-controlled layer as diagnostics. Mitigation: split user output onto dedicated stdout/stderr targets with message-only formatting, exclude those targets from the diagnostic layer, and test that restrictive `RUST_LOG` values do not hide them.
- High risk: `CliTokenSink::on_complete()` and approval banners will regress if they pick up timestamps, levels, or duplicate emission. Mitigation: route those paths through the dedicated message-only layers and add captured-writer tests for the exact blank-line and banner behavior.
- Medium risk: global tracing initialization can break tests or secondary startup paths if it is not idempotent. Mitigation: use `try_init` or a one-time helper and test that path directly.
- Medium risk: span instrumentation in async queue/WS code can accidentally capture large values or hold references across awaits. Mitigation: keep span fields to cheap scalars and IDs and enter spans locally without moving business logic.
- Medium risk: hot-path trace logging in `src/llm/openai.rs` and `src/session.rs` can add noise or measurable overhead. Mitigation: keep expensive formatting out of trace-disabled paths and stick to structured fields instead of large `format!` strings.
