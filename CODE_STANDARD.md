# CODE_STANDARD.md

This repository is maintained by agents. The standard is the contract. If a change does not satisfy the standard, the change is out of scope.

## File Shapes

Every file must fit one primary shape:

- entrypoint
- orchestrator
- validator / parser
- security gate
- persistence adapter
- model / DTO
- thin wrapper
- test module

If a file needs two primary shapes, split it. A file that is both policy and I/O is already too large.

## Rules

### 1. One primary responsibility per file

Rule: one file owns one clear boundary and one clear verb.

Good examples: `src/subscription.rs` is a single validation/rendering boundary; `src/llm/mod.rs` is a typed message-model boundary.

Bad examples: `src/store.rs` mixes migration, CRUD, plan persistence, step attempts, and subscriptions; `src/agent/loop_impl.rs` mixes audit helpers, approvals, streaming, persistence, and execution; `src/main.rs` mixes CLI schema, tracing, server launch, REPL, and one-shot execution.

Enforcement: review rejects mixed-boundary files, and each cleanup session must include a delete list for the old shape.

### 2. Validation fails closed

Rule: invalid config, invalid policy, invalid regexes, and invalid shell shapes are errors. They are never silently downgraded to a default.

Good examples: `src/subscription.rs` returns `Result` from parsing and rendering helpers; `src/config.rs` already has a structured `ConfigError` boundary.

Bad examples: `src/gate/secret_redactor.rs` dropping invalid regexes with `ok()`; `src/gate/shell_safety.rs` turning unknown shell-policy values into `Approve` and `Medium`; raw string policy fields in `src/config.rs` that force later reparsing.

Enforcement: `xtask lint` greps for the fail-open patterns listed below, and config/security tests must prove invalid input is rejected.

### 3. Typed data stays typed

Rule: if the data has structure, keep the structure. Do not flatten structured values into ad hoc strings.

Good examples: `src/llm/mod.rs` models ordered `ChatMessage` content and tool calls; `src/subscription.rs` uses `SubscriptionFilter` instead of stringly filter flags.

Bad examples: `src/session.rs` flattening ordered assistant content into newline-joined text and reconstructing a different shape on replay; `src/agent/shell_execute.rs` and `src/agent/loop_impl.rs` hand-building JSON error payloads with string interpolation; `src/config.rs` keeping finite policy values as raw `String`s.

Enforcement: transcript and JSON-shape regression tests, plus the manual-JSON grep check in `xtask lint`.

### 4. Preserve ordered transcripts end to end

Rule: assistant text and tool-call interleaving is semantic. The live stream, JSONL persistence, and replay path must preserve the same ordered message graph.

Good examples: `llm/history_groups.rs` is the single source of truth for grouping; `session/jsonl.rs` uses the same grouping logic for persistence and replay.

Bad examples: `src/session.rs` collapsing text and tool calls into separate buckets; `src/llm/openai.rs` rebuilding assistant output as text first and tool calls second.

Enforcement: one round-trip regression that emits mixed text/tool content, saves it, reloads it, and checks the block order and metadata are unchanged.

### 5. Shared behavior lives in one helper

Rule: if two entrypoints do the same thing, they call the same helper. Duplicate control flow is a bug farm.

Good examples: `llm/history_groups.rs` centralizes transcript grouping; `session_runtime/drain.rs` centralizes single-session queue draining.

Bad examples: duplicate queue-drain loops in `src/agent/queue.rs`, `src/server/queue.rs`, `src/main.rs`, and `src/server/ws.rs`; duplicate denial formatting in `src/cli.rs` and `src/agent/loop_impl.rs`.

Enforcement: each cleanup session must name the helper it removes duplication into, and the old copy must be deleted or replaced by a thin wrapper only.

### 6. Security boundaries are explicit

Rule: comments at policy and security boundaries explain the threat model and the reason the code exists.

Good examples: `src/tool.rs` says RLIMITs are not a sandbox; `src/subscription.rs` documents its filter shapes and validation rules.

Bad examples: missing prose on queue precedence, shell-policy decisions, protected-path behavior, and session round-trip invariants.

Enforcement: review only. If the comment does not explain the boundary, it is not good enough.

### 7. State machines use explicit state

Rule: long-lived control flow uses explicit state, enums, and small helpers. It does not rely on a pile of booleans and counters to stay readable.

Good examples: `src/llm/mod.rs` uses typed enums for roles and content; `src/turn.rs` uses verdicts and severity types.

Bad examples: `src/agent/loop_impl.rs` carrying multiple mutable flags to keep one giant async loop alive.

Enforcement: split state machines into smaller modules and keep the branch structure obvious.

### 8. No silent loss of errors at boundaries

Rule: no `#[allow(...)]` in production code, no `.ok()` drops at security or persistence boundaries, and no silent downgrade of a validation failure.

Good examples: `SubscriptionFilter::from_flags` and `ConfigError` surface validation failure directly.

Bad examples: `SecretRedactor::new` discarding invalid regexes; `ShellSafety::with_policy_and_skills_dirs` defaulting unknown values; manual JSON error strings in shell execution.

Enforcement: the `clippy` deny block below, the grep checks below, and boundary-specific tests.

### 9. Every cleanup change has a delete list

Rule: a refactor must remove something concrete. Adding layers without deleting the old shape is no cleanup.

Good examples: a compatibility shim that is explicitly temporary and then removed in the final sweep; a split file tree with the old file deleted after reexports are stable.

Bad examples: adding a wrapper and keeping the old duplicated implementation forever.

Enforcement: every session must list the files it will delete or the old code paths it will remove.

### 10. Thin wrappers stay thin

Rule: wrappers may delegate. They may not accrete policy, parsing, or state-machine logic.

Good examples: `Config::load` and `Config::from_file` are thin wrappers over a typed boundary.

Bad examples: `src/plan/executor.rs` when it becomes a second plan system; `src/cli.rs` when it becomes the terminal UI, prompt formatter, approval driver, and denial formatter all at once.

Enforcement: if a wrapper starts branching on policy, split it.

### 11. Naming describes responsibility

Rule: file names describe the responsibility in the file, not just the layer it happens to live in.

Good examples: `terminal_ui.rs`, `logging.rs`, `time.rs`, `child_session/`.

Bad examples: `cli.rs` when it is terminal I/O, and `util.rs` when it is formatting and timestamp code.

Enforcement: review only.

## Enforcement

- `clippy` denies: `clippy::allow_attributes_without_reason`, `clippy::dbg_macro`, `clippy::expect_used`, `clippy::todo`, `clippy::too_many_arguments`, `clippy::unimplemented`, `clippy::unwrap_used`.
- `xtask lint` runs the grep checks below, then `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo test --features integration --test integration` when `~/.autopoiesis/auth.json` exists.
- Tests must cover transcript round-trip fidelity, fail-closed config validation, shell/error serialization, queue drain behavior, and plan runner recovery.
- Review checks comments, naming, file-shape adherence, and delete lists.

## Operating Rule

If a proposed change can only be described as "make it cleaner," stop and restate it as a file-shape change or a boundary fix. If it cannot be stated that way, it is not ready.
