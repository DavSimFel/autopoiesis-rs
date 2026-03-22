# Taint Tracking Plan

## Goal
Add message-level `Principal` metadata, persist it through session JSONL replay, and use turn taint to disable shell standing approvals whenever any input message is non-operator.

## Mechanical Changes

1. Add `src/principal.rs` and move shared principal logic there.
   - Define `Principal::{Operator, User, System, Agent}`.
   - Add `is_trusted()`.
   - Add helpers for queue-source mapping and server transport/source naming.
   - Re-export the module from `lib.rs`.

2. Extend `ChatMessage` in `src/llm/mod.rs`.
   - Add `principal: Principal`.
   - Default missing principals to `Operator` for backwards compatibility.
   - Add helper constructors that accept an optional principal.
   - Update all direct struct literals and call sites that need non-default principals.

3. Persist principal in `src/session.rs`.
   - Add `principal` to `SessionEntry`.
   - Serialize it into JSONL output.
   - Restore it on replay, defaulting missing values to `Operator`.
   - Keep existing replay/trimming behavior intact.

4. Add taint state to `Turn` in `src/turn.rs`.
   - Cache whether the current turn is tainted after inbound assembly.
   - Expose `Turn::is_tainted()`.
   - Pass a taint-aware guard context into every guard check.

5. Update guard plumbing in `src/gate/`.
   - Add a small context object carrying `tainted`.
   - Keep allow/deny precedence unchanged.
   - Make `ShellSafety` skip standing approvals when tainted.
   - Leave allow and deny pattern handling unchanged.

6. Propagate principals through runtime flows.
   - `src/server.rs`: use shared `Principal`, derive request role/source from it, and keep queue source naming stable.
   - `src/agent.rs`: map queued source strings to principals, create `ChatMessage`s with the right principal, and use turn taint for tool-result provenance.
   - `src/main.rs`: keep CLI enqueueing operator-trusted messages.
   - Update tests so assistant/tool/system fixtures carry the intended principals.

## Self-Review Checks

- Older JSONL entries must still replay because missing principal fields default to `Operator`.
- Standing approvals must be skipped only when the current turn is tainted; allow/deny patterns must still behave the same.
- Assistant messages created by the provider must be marked `Agent`, or taint will silently fail open.
- Tool results must not be left as default-operator messages, or future turns can incorrectly regain standing approvals.
- Queue source-to-principal mapping must stay deterministic for `cli`, `*-operator`, `*-user`, and unknown sources.
- Direct `ChatMessage` struct literals in tests and provider code must compile after the new field is added.
