# Session 4b Plan: Gate Cleanup

This turn is planning only. No code was changed, no tests were run, and the `openclaw system event --text 'Session 4b done: gate cleanup' --mode now` command is deferred until the implementation session actually finishes.

## 1. Files Read

### Standards, docs, and config

- `CODE_STANDARD.md`
- `docs/risks.md`
- `docs/architecture/overview.md`
- `Cargo.toml`
- `agents.toml`

### All `src/` files

- `src/agent/loop_impl.rs`
- `src/agent/loop_impl/tests.rs`
- `src/agent/mod.rs`
- `src/agent/queue.rs`
- `src/agent/queue/tests.rs`
- `src/agent/shell_execute.rs`
- `src/agent/spawn.rs`
- `src/agent/spawn/tests.rs`
- `src/agent/tests.rs`
- `src/agent/tests/common.rs`
- `src/agent/tests/regression_tests.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config/agents.rs`
- `src/config/domains.rs`
- `src/config/file_schema.rs`
- `src/config/load.rs`
- `src/config/mod.rs`
- `src/config/models.rs`
- `src/config/policy.rs`
- `src/config/runtime.rs`
- `src/config/spawn_runtime.rs`
- `src/config/tests.rs`
- `src/context.rs`
- `src/delegation.rs`
- `src/gate/budget.rs`
- `src/gate/command_path_analysis.rs`
- `src/gate/exfil_detector.rs`
- `src/gate/mod.rs`
- `src/gate/output_cap.rs`
- `src/gate/protected_paths.rs`
- `src/gate/secret_catalog.rs`
- `src/gate/secret_redactor.rs`
- `src/gate/shell_safety.rs`
- `src/gate/streaming_redact.rs`
- `src/identity.rs`
- `src/lib.rs`
- `src/llm/history_groups.rs`
- `src/llm/mod.rs`
- `src/llm/openai.rs`
- `src/main.rs`
- `src/model_selection.rs`
- `src/plan.rs`
- `src/plan/executor.rs`
- `src/plan/notify.rs`
- `src/plan/patch.rs`
- `src/plan/recovery.rs`
- `src/plan/runner.rs`
- `src/principal.rs`
- `src/read_tool.rs`
- `src/server/auth.rs`
- `src/server/http.rs`
- `src/server/mod.rs`
- `src/server/queue.rs`
- `src/server/ws.rs`
- `src/session/budget.rs`
- `src/session/delegation_hint.rs`
- `src/session/jsonl.rs`
- `src/session/mod.rs`
- `src/session/tests.rs`
- `src/session/trimming.rs`
- `src/skills.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/time.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### `src/gate/secret_redactor.rs`

- Keep the Session 1 fail-closed constructor shape: `SecretRedactor::new()` must continue to return `Result<Self, regex::Error>` and compile every pattern eagerly.
- Add `Policy:` comments at the security boundaries:
  - above `new()` to state that redaction configuration is validated eagerly and invalid regexes must fail construction instead of silently disabling a pattern
  - above `redact_messages()` to state that inbound/tool-result content is rewritten before persistence
  - above `check()` to state that inbound history and streaming text are redacted in place, and deny is never used as a substitute for redaction here
- Add an `Invariant:` comment above `default_catalog()` explaining that `SECRET_PATTERNS` is repo-owned static configuration and invalid built-in patterns are a programmer bug that must abort immediately
- No algorithm rewrite is planned unless review during edit uncovers an actual mismatch with the standard

### `src/gate/shell_safety.rs`

- Add `Policy:` comments documenting the decision ladder inside `evaluate_command()`:
  - deny patterns run before all other shell policy checks
  - parse failure is a security boundary and must not be treated as a safe command
  - protected reads and prompt/skills writes are hard-denied before allowlist/default handling
  - compound commands always require explicit approval even if an allow pattern would otherwise match
  - standing approvals are disabled for tainted turns
- Tighten malformed argument handling to match `CODE_STANDARD.md` fail-closed requirements:
  - replace the current `command_from_args(...).ok()?` plus `unwrap_or_default()` path
  - malformed JSON or a missing/non-string `command` field should produce an explicit `Verdict::Deny`, not default back to `allow`/`approve`
- Keep typed config usage from Session 1 as-is: no stringly-typed severity/default parsing should be reintroduced
- Preserve the current deny/approve/allow precedence and protected-path logic

### `src/agent/shell_execute.rs`

- Add `Invariant:` comments at the shell execution boundaries:
  - `guarded_shell_execute_call()` and `guarded_shell_execute_prechecked()` only accept `execute` tool calls
  - `guarded_shell_execute_prechecked()` assumes the caller has already applied shell approval/deny policy
  - `execute_shell_call()` must preserve the order `execute -> derive exit code -> redact -> cap -> persist/return`
  - denial paths must return no raw shell output and no result artifact path
- Keep the Session 1 serde-based error serialization; do not reintroduce hand-built JSON strings
- Add small cleanup only if needed for clarity; no interface changes are planned

### `src/gate/budget.rs`

- Add a `Policy:` comment above `check()` explaining that budget enforcement is an inbound preflight hard deny and intentionally does not redact or mutate content
- Add a short comment near `violations()` that the reporting order is deterministic because tests and operator output depend on stable ordering

### `src/gate/exfil_detector.rs`

- Add `Policy:` comments describing scope and limits:
  - this is a heuristic batch detector layered on top of primary shell policy, not a sandbox
  - the detector escalates read+send sequences to approval instead of silently allowing them
  - parse failures in shell tokenization are treated conservatively inside `has_sensitive_read()`
- Tighten `command_from_args()` for malformed `execute` payloads:
  - current malformed JSON path is silently skipped
  - malformed JSON or a missing/non-string `command` field should produce a conservative batch-level `Verdict::Approve` with high severity instead of disappearing from exfil checks
  - keep ShellSafety as the primary hard fail-closed boundary, but do not let malformed batch entries become invisible to the heuristic detector

### `src/gate/output_cap.rs`

- Add a `Policy:` comment above `cap_tool_output()` that all tool output is written to the bounded `results/` artifact first, and oversized inline responses are replaced with a pointer string so transcripts do not carry raw oversized output
- Add an `Invariant:` comment above `safe_call_id_for_filename()` explaining that result filenames must never trust raw tool call IDs
- Keep the pointer contract unchanged unless tests reveal a leak

### `src/gate/streaming_redact.rs`

- Add `Policy:` comments explaining:
  - why `prefix_holdback()` exists
  - why partial prefixes must remain buffered until the guard can decide whether a secret is present
  - why active secret continuation keeps redacting across token boundaries
- No behavior change is planned unless the comment pass exposes an unstated invariant that needs a focused assertion

### `src/tool.rs`

- Add `Policy:` / `Invariant:` comments at the shell execution boundaries:
  - RLIMITs are bounded-resource controls, not isolation
  - Unix pre-exec sets a new process group so termination reaches descendants
  - stdout/stderr share a single capture budget
  - after the capture cap is hit, draining continues briefly to let the child exit cleanly while still discarding excess bytes
  - timeout errors distinguish between original timeout expiry and post-cap drain expiry
- Keep command parsing and timeout clamping typed and fail-closed
- No behavioral changes are planned unless the comment pass exposes a real mismatch

### `src/read_tool.rs`

- Add `Policy:` comments around:
  - deny-first path validation before file reads
  - normalization and allowed-root checks
  - protected path rejection
  - Unix component-by-component `openat` walk / non-Unix symlink defenses
  - provenance header construction
- Keep the current fail-closed posture: malformed args, parent traversals, symlinks, protected paths, and oversize files must continue to error out instead of degrading to best-effort reads

### `src/gate/mod.rs`

- Verify the facade exports remain the intended surface:
  - public: `BudgetGuard`, `ExfilDetector`, `SecretRedactor`, `ShellSafety`
  - internal: `output_cap`, `protected_paths`, `streaming_redact`
  - test-only: `SECRET_PATTERNS`
- Add `Policy:` comments above:
  - `guard_text_output()` to state that denied outbound deltas collapse to empty text instead of partially persisting unsafe content
  - `guard_message_output()` to state that assistant text and tool-call arguments are guarded before persistence
  - `redact_tool_call_arguments()` to state that argument redaction must never leak the original string on a serializer failure
- Remove the remaining serializer fail-open behavior:
  - current `serde_json::to_string(&value).unwrap_or(arguments)` can leak original unredacted arguments if serialization fails
  - replace that fallback with a conservative redacted object/string path that is constructed without ever falling back to the original arguments
  - keep the malformed-JSON wrapper path conservative and explicit

### `src/agent/loop_impl/tests.rs`

- Add a guard-interaction regression for malformed `execute` arguments with both `ShellSafety` and `ExfilDetector` enabled
- Assert that the final result is the `shell-policy` deny path and that the approval handler is never invoked
- Keep the assertion at the agent-loop level so it proves the real deny-over-approve integration behavior, not just unit-level guard behavior

### Caller-side gate import cleanup

- Audit and update gate import sites that depend on the facade surface while this cleanup is in flight:
  - `src/agent/shell_execute.rs`
  - `src/read_tool.rs`
  - `src/turn.rs`
  - any affected tests that import internal gate helpers or test-only exports
- If `src/gate/mod.rs` visibility or re-exports change, adjust these call sites in the same patch so the tree stays compiling at every step

### Files reviewed with no planned functional edit

- `src/gate/command_path_analysis.rs`
- `src/gate/protected_paths.rs`

These already carry the right heuristic/not-a-sandbox framing and did not show obvious Session 4b follow-up work beyond keeping their imports aligned if the facade changes.

## 3. What Tests To Write

### `src/gate/shell_safety.rs`

- Add a regression test that malformed `execute` arguments are hard-denied even when shell default action is `Allow`
- Add a regression test that a missing `command` field is also hard-denied
- Preserve existing tests for protected reads, protected writes, compound commands, allowlist hits, and taint-blocked standing approvals

### `src/gate/mod.rs`

- Add a test that malformed tool-call argument JSON containing a secret is persisted only through the conservative redacted wrapper and never echoes the original string
- Add a test that structured tool-call argument JSON with embedded secrets is redacted and never falls back to the original argument string
- Implement the code path so no synthetic serializer-failure harness is required; the contract test is the absence of any path back to the original arguments

### `src/agent/shell_execute.rs`

- Add a test that `guarded_shell_execute_call()` rejects non-`execute` tool calls
- Add a test that `guarded_shell_execute_prechecked()` rejects non-`execute` tool calls
- Add a regression test that tool execution errors still return valid JSON error output that is redacted/capped in the normal path

### `src/gate/secret_redactor.rs`

- Keep the invalid-regex constructor test
- Add an explicit regression if needed for the built-in catalog invariant comment path only if code changes there

### `src/gate/exfil_detector.rs`

- Add a batch-level regression for malformed JSON producing the conservative high-severity approval outcome instead of silent skip
- Add a regression for a missing/non-string `command` field producing the same conservative batch outcome
- Keep existing read+send heuristics tests intact

### `src/agent/loop_impl/tests.rs`

- Add an integration regression with both `ShellSafety` and `ExfilDetector` active on malformed `execute` arguments
- Assert that `shell-policy` deny wins over the batch-level exfil approval and the approval handler call count remains zero

### `src/gate/streaming_redact.rs`

- Add a regression for a secret prefix split across token boundaries to ensure holdback prevents prefix leakage
- Add a regression for final flush behavior on an incomplete candidate so literal text and redaction boundaries remain correct

### `src/tool.rs`

- Keep existing timeout, truncation, and drain-after-cap tests
- Add only targeted assertions if a behavior change is required; comments alone should not force broad test churn

### `src/read_tool.rs`

- Add a regression only if any path-policy code moves:
  - protected path deny remains hard error
  - provenance header remains present on successful reads
  - parent traversal / symlink protections remain intact

### Whole-tree verification

- Run focused unit tests for the edited modules first
- Then run `cargo build --release`
- Then run `cargo test`
- Then run `cargo fmt --check`
- Then run `cargo clippy -- -D warnings`
- Then run `xtask/lint.sh`

## 4. Order Of Operations

1. Start with the pure comment / low-risk guard files:
   - `src/gate/secret_redactor.rs`
   - `src/gate/budget.rs`
   - `src/gate/output_cap.rs`
   - `src/gate/streaming_redact.rs`

2. Fix the actual fail-closed issues before facade cleanup:
   - `src/gate/shell_safety.rs`
   - `src/gate/exfil_detector.rs` if malformed-arg skipping is tightened

3. Harden the persistence/redaction facade and fix caller imports in the same slice:
   - `src/gate/mod.rs`
   - `src/agent/shell_execute.rs`
   - `src/read_tool.rs`
   - `src/turn.rs`
   - any directly affected gate-related tests

4. Add shell execution boundary comments and any tiny invariant tests:
   - `src/tool.rs`

5. Run targeted tests for the touched files after each cluster to keep the work green and isolate regressions quickly

6. Run full verification:
   - `cargo build --release`
   - `cargo test`
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `xtask/lint.sh`

7. Only after the implementation session passes verification, run:
   - `openclaw system event --text 'Session 4b done: gate cleanup' --mode now`

## 5. Risk Assessment

### Highest risk

- `src/gate/shell_safety.rs`: changing malformed-argument handling from default-policy fallback to explicit fail-closed behavior can break existing tests and any flows that accidentally relied on malformed `execute` payloads being merely approved
- `src/gate/mod.rs`: removing `unwrap_or(arguments)` changes the exact persisted shape of redacted tool-call arguments in rare serializer-failure paths; tests need to pin the new conservative contract

### Medium risk

- `src/gate/exfil_detector.rs`: if malformed tool-call args stop being silently skipped, batch approval behavior changes and may require test updates in turn/agent integration coverage
- `src/agent/shell_execute.rs`: invariant-only edits are low risk, but if any helper extraction happens around the guarded execution pipeline it can subtly change denial/result-file behavior

### Low risk

- `src/gate/secret_redactor.rs`
- `src/gate/budget.rs`
- `src/gate/output_cap.rs`
- `src/gate/streaming_redact.rs`
- `src/tool.rs`
- `src/read_tool.rs`

These should mostly be comment additions plus narrow assertions, provided the implementation does not broaden scope.

### Out-of-scope / no planned deletions

- No module splits or dependency changes are planned
- No public API expansion is planned
- No delete list is expected for Session 4b unless a review during implementation finds a genuinely dead import/re-export in `src/gate/mod.rs`
