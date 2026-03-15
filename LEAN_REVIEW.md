# Lean Review: autopoiesis-rs

## Line counts (src/ + tests)

| File | Current LOC | Proposed LOC |
| --- | ---: | ---: |
| src/agent.rs | 380 | 376 |
| src/auth.rs | 325 | 319 |
| src/config.rs | 151 | 151 |
| src/context.rs | 391 | 356 |
| src/guard.rs | 862 | 862 |
| src/identity.rs | 154 | 154 |
| src/lib.rs | 14 | 14 |
| src/llm/mod.rs | 185 | 185 |
| src/llm/openai.rs | 811 | 811 |
| src/main.rs | 251 | 219 |
| src/server.rs | 471 | 440 |
| src/session.rs | 703 | 642 |
| src/store.rs | 259 | 259 |
| src/template.rs | 86 | 86 |
| src/tool.rs | 261 | 252 |
| src/turn.rs | 327 | 337 |
| src/util.rs | 96 | 96 |
| tests/MANUAL.md | 68 | 68 |
| tests/review_fixes.md | 19 | 19 |
| tests/integration.rs | 122 | 122 |

- **Total current LOC:** 5936
- **Total proposed LOC:** 5768
- **Estimated LOC reduction:** 168

## Concrete deletions (dead code / redundancy / duplication)

- `src/context.rs:216-220` remove redundant wrapper test `identity_gate_replaces_system_message`.
- `src/context.rs:230-234` remove redundant wrapper test `identity_gate_uses_fallback_on_missing_dir`.
- `src/context.rs:261-265` remove redundant wrapper test `identity_gate_applies_template_vars`.
- `src/context.rs:280-284` remove redundant wrapper test `history_gate_adds_history_to_messages`.
- `src/context.rs:298-302` remove redundant wrapper test `history_gate_respects_token_budget`.
- `src/context.rs:320-324` remove redundant wrapper test `history_gate_skips_system_messages`.
- `src/context.rs:338-342` remove redundant wrapper test `history_gate_newest_first`.
- `src/turn.rs:200-204` remove redundant wrapper test `empty_pipeline_allows_everything`.
- `src/turn.rs:221-225` remove redundant wrapper test `edit_gates_run_in_config_order`.
- `src/turn.rs:325-329` remove redundant wrapper test `full_pipeline_builds_complete_context`.
- `src/tool.rs:251-254` remove redundant wrapper test `execute_tool_definition_has_execute_name`.
- `src/tool.rs:224-226` remove dead helper `Shell::_default_timeout_ms` (unused duplicate of timeout constant math).
- `src/session.rs:335-357` remove dead `Session::list_sessions`, currently not part of active API flow in this crate.
- `src/session.rs:641-661` remove now-obsolete duplicate-only test `list_sessions_returns_jsonl_files_sorted`.
- `src/main.rs:175-200` remove duplicated local `default_turn()` factory.
- `src/server.rs:319-343` remove duplicated local `server_turn()` factory.

## Comments removed (obvious / low-value)

- `src/agent.rs:192`, `src/agent.rs:256` remove comments restating control-flow (tool-call and stop behavior).
- `src/agent.rs:284`, `src/agent.rs:296` remove `#[allow(dead_code)]` annotations on test-only impls now unnecessary.
- `src/server.rs:222`, `src/server.rs:249`, `src/server.rs:257`, `src/server.rs:265`, `src/server.rs:272` remove obvious parser/persistence flow comments.
- `src/session.rs:428`, `src/session.rs:436`, `src/session.rs:440`, `src/session.rs:453`, `src/session.rs:464`, `src/session.rs:464`, `src/session.rs:475`, `src/session.rs:491`, `src/session.rs:573`, `src/session.rs:575`, `src/session.rs:597`, `src/session.rs:601`, `src/session.rs:615`, `src/session.rs:626` remove obvious section headers and inline test explanations.

## Comments added (non-obvious intent)

- `src/session.rs:283` add intent comment in `trim_context`: keep one leading system message even while trimming older conversational history.

## Functions simplified / inlined

- `src/auth.rs:41-48` inline `read_tokens` body and remove wrapper function `load_tokens()`, reducing indirection for token loading.
- `src/turn.rs:53` inline mutation verdict computation in `check_inbound` (`messages.len() != baseline.len()`) and remove `verdict_from_mutations` helper.
- `src/turn.rs:113-137` introduce `build_default_turn(config)` and use it in both CLI and server entrypoints.
- `src/main.rs:98` replace local duplicated turn construction with `turn::build_default_turn(&provider_config)`.
- `src/server.rs:228` replace local duplicated turn construction with `turn::build_default_turn(&state.config)`.

## Notes

- No functional behavior changes were made; edits are scoped to dead code removal, deduplication, and comment tightening.
