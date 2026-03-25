# Phase 3a Plan: Structured Read API

## 1. Files Read

### Docs and config
- `Cargo.toml`
- `agents.toml`
- `docs/risks.md`
- `docs/roadmap.md`
- `docs/specs/identity-v2.md`
- `docs/vision.md`
- `docs/architecture/overview.md`

### All source files under `src/`
- `src/agent.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/delegation.rs`
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
- `src/model_selection.rs`
- `src/principal.rs`
- `src/server.rs`
- `src/session.rs`
- `src/spawn.rs`
- `src/store.rs`
- `src/subscription.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

## 2. Exact Changes Per File

### `src/read_tool.rs` (new)
- Add `ReadFile` tool implementing `Tool`.
- Add a small parsed-args struct for:
  - `path: String` required
  - `offset: Option<u64>` interpreted as 1-based starting line
  - `limit: Option<u64>` interpreted as max number of lines returned
- `name()` returns `"read_file"`.
- `definition()` returns a `FunctionTool` schema with only `path` required and `additionalProperties: false`.
- `execute()` stays async and wraps the blocking read path in `tokio::task::spawn_blocking`.
- Internal blocking read path will:
  - resolve relative paths against `std::env::current_dir()`
  - canonicalize the requested file and configured allowed roots where possible
  - deny if the resolved file is outside configured `allowed_paths`
  - deny if the resolved path matches the shared protected-path catalog from `gate/secret_patterns.rs`
  - return `file not found` if the path does not exist
  - reject non-UTF-8 text reads with a clear error instead of lossy conversion
  - read line-by-line, apply `offset`/`limit`, and enforce `max_read_bytes` on the rendered slice
  - treat `offset` past EOF as a successful empty read (header only, empty body)
  - treat `limit` past EOF as natural truncation at EOF, not an error
  - prepend the exact provenance header format:
    - `<meta source=read_file path={normalized_path} principal=operator />`
- Keep the return payload plain text only: header plus file content. No extra JSON envelope.
- Phase 3a default roots stay strictly operator-authored so the fixed `principal=operator` tag remains honest. Neither `sessions/` nor the whole working tree are default roots in this phase.

### `src/config.rs`
- Add a new config surface for the read tool, likely `[read]`.
- Add a `ReadToolConfig` struct on `Config` with serde defaults.
- Default values:
  - `allowed_paths = ["identity-templates"]`
  - `max_read_bytes = 65536`
- Parse the new section in `RuntimeFileConfig`.
- Keep this config inert for current runtime behavior; it only feeds future `ReadFile` construction.
- Add validation for obviously bad config values:
  - empty `allowed_paths` entry rejected
  - `max_read_bytes == 0` rejected or normalized fail-closed
- Add unit tests for defaults and explicit overrides.

### `src/gate/secret_patterns.rs`
- Expose a reusable helper for path-based protected-path checks so `ReadFile` and `ShellSafety` share one source of truth.
- Reuse the existing normalization logic instead of duplicating `.env` / `auth.json` / `~/.ssh` / `.aws/credentials` rules inside the new tool.
- Keep shell behavior unchanged; this is a surface extraction, not a policy change.
- Add unit tests covering direct file-path checks for:
  - `~/.autopoiesis/auth.json`
  - `.env`, `.env.local`, `.env.production.local`
  - `~/.ssh/id_rsa`, `~/.ssh/id_ed25519`
  - `~/.aws/credentials`
  - safe near-misses such as `config/auth.json` and `.env.example`

### `src/lib.rs`
- Export the new module with `pub mod read_tool;`.

### `src/turn.rs`
- Update test-only `Config` literals/helpers to populate the new `read` field.
- No runtime/tool-selection behavior change in Phase 3a.

### `src/server.rs`
- Update the `test_state()` `Config` literal to populate the new `read` field.
- No server behavior change in Phase 3a.

### `src/spawn.rs`
- Update `test_config()` to populate the new `read` field.
- No spawn/runtime behavior change in Phase 3a.

### `src/agent.rs`
- Update test-only `Config` literals/helpers to populate the new `read` field.
- No agent loop/runtime behavior change in Phase 3a.

### `tests/` (conditional sweep if integration tests/fixtures touch these surfaces)
- Update any integration tests, fixtures, or test helpers outside `src/` that:
  - construct `Config` directly
  - assert against the checked-in `agents.toml`
  - rely on the exact shipped config shape
- Keep this as compile-preservation and fixture-maintenance only unless an existing integration expectation genuinely needs new assertions.

### `src/session.rs`
- Add a provenance preservation test only.
- No runtime logic change expected.
- Test should append a tool result whose content starts with the `<meta ... />` header, reload the JSONL, and assert the content is preserved byte-for-byte in replayed history.

### `agents.toml`
- Add an explicit `[read]` section documenting the defaults the runtime will use later:
  - `allowed_paths = ["identity-templates"]`
  - `max_read_bytes = 65536`
- `sessions/` and the full working tree stay out of the default shipped policy until provenance principal semantics are expanded beyond the fixed `operator` tag.
- This does not change current tool selection; it just makes the read policy explicit in the shipped config.

### `docs/architecture/overview.md`
- Update the module map to include `read_tool.rs`.
- Note that the read tool exists but tier-based tool selection is still deferred to Phase 3b.
- Let the pre-commit stats update line/file counts.

### `docs/specs/identity-v2.md`
- Document the new operator-facing `[read]` policy block and the Phase 3a boundary:
  - `read_file` exists as a standalone tool
  - default roots are limited to `identity-templates/`
  - tier-based T2/T1/T3 tool selection still lands in Phase 3b

### Deliberately unchanged in Phase 3a
- `src/tool.rs`
- `src/main.rs`
- runtime behavior in `src/turn.rs`
- runtime behavior in `src/agent.rs`
- runtime behavior in `src/server.rs`
- runtime behavior in `src/spawn.rs`
- `Cargo.toml`

Reason: the existing tool trait and turn plumbing are already sufficient, and Tier/T2 tool selection is explicitly deferred to Phase 3b. The touched files above only need config-literal/test-helper maintenance, not behavior changes.

## 3. Tests To Write

### `src/read_tool.rs`
- `definition_exposes_read_file_schema`
  - asserts tool name is `read_file`
  - asserts `path` is required
  - asserts `offset` and `limit` are optional numeric fields
- `execute_reads_allowed_file_and_prepends_provenance_header`
  - asserts header exists
  - asserts returned path is normalized
  - asserts body matches file contents
- `execute_resolves_relative_paths_against_cwd`
  - asserts relative repo paths work when under an allowed root
- `execute_applies_offset_and_limit_by_lines`
  - asserts 1-based offset semantics
  - asserts only requested lines are returned
- `execute_offset_past_eof_returns_header_with_empty_body`
  - asserts the call succeeds
  - asserts provenance header is still present
  - asserts no file body lines are returned
- `execute_limit_past_eof_truncates_naturally`
  - asserts the call succeeds
  - asserts returned body stops at EOF without error
- `execute_rejects_offset_zero`
  - asserts `offset=0` fails clearly instead of silently becoming line 1
- `execute_rejects_limit_zero`
  - asserts `limit=0` fails clearly instead of returning an empty success payload
- `execute_denies_path_outside_allowed_roots`
  - asserts access-denied error
- `execute_denies_traversal_out_of_allowed_root`
  - use an allowed root plus `..` escape attempt
  - asserts normalization/canonicalization closes the traversal path
- `execute_denies_symlink_escape_out_of_allowed_root`
  - Unix-only test
  - place a symlink inside an allowed root that points to a file outside the root
  - asserts canonical path comparison denies the read
- `execute_denies_protected_paths_even_if_under_allowed_root`
  - use `.env` and `auth.json` fixtures
  - asserts protected-path deny beats allow-root match
- `execute_returns_file_not_found_error`
  - asserts missing file is a distinct error
- `execute_returns_too_large_error_when_slice_exceeds_cap`
  - asserts no partial content leak
- `execute_rejects_invalid_or_empty_path_argument`
  - asserts clear argument error text

### `src/config.rs`
- `read_config_defaults_are_loaded_when_section_missing`
- `read_config_override_is_honored`
- `read_config_rejects_zero_max_read_bytes`
- `read_config_rejects_empty_allowed_path_entry`

### `src/turn.rs`, `src/server.rs`, `src/spawn.rs`, `src/agent.rs`
- compile-preservation tests/helpers updated for the new `Config.read` field
- no behavioral assertions added here beyond keeping existing tests compiling and green

### `tests/`
- integration-test/fixture sweep for `Config.read` and the shipped `agents.toml` shape, if any such tests exist
- no new feature assertions required here unless an existing integration fixture breaks

### `src/gate/secret_patterns.rs`
- `protected_path_helper_matches_same_catalog_as_shell_rules`
- `protected_path_helper_allows_safe_near_misses`

### `src/session.rs`
- `tool_result_with_provenance_header_round_trips_through_jsonl`
  - asserts the `<meta ... />` line survives persistence/reload unchanged

## 4. Order Of Operations

1. Add the reusable protected-path helper in `src/gate/secret_patterns.rs` with tests.
2. Add `ReadToolConfig` parsing/defaults in `src/config.rs`, update `agents.toml`, and in the same patch update every affected `Config`/config-shape test or fixture in `src/turn.rs`, `src/server.rs`, `src/spawn.rs`, `src/agent.rs`, and `tests/` so the tree still compiles.
3. Add `src/read_tool.rs` and export it from `src/lib.rs`.
4. Add the session round-trip test in `src/session.rs`.
5. Update `docs/architecture/overview.md` and `docs/specs/identity-v2.md`.
6. Run verification in repo-required order:
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy -- -D warnings`
   - `cargo build --release`
   - `cargo test --features integration` only if auth/live setup is available

This order keeps the policy seam stable first, then lands config plus all compile-required call-site and fixture maintenance together, then adds the standalone tool, then proves persistence, then updates docs.

## 5. Risk Assessment

### Provenance principal semantics
- The requested header format hardcodes `principal=operator`.
- Risk: files under `sessions/` or the general working tree can contain tainted or generated material, so the fixed tag would overstate trust.
- Plan: Phase 3a default policy excludes both `sessions/` and the general working tree. If either needs to become readable later, that change should ship with file-class-aware provenance, not under the fixed `operator` tag.

### Path escape via symlinks
- Lexical normalization alone is not enough if an allowed root contains symlinks pointing outside the root.
- Plan: prefer canonical path comparison for existing files and canonical allowed roots, with a fail-closed deny if resolution is ambiguous.

### Large-file behavior
- Rejecting based on whole-file size would make `offset`/`limit` much less useful.
- Plan: enforce `max_read_bytes` on the rendered slice, not just raw file size, by streaming lines in the blocking worker.

### Protected-path drift
- If `ReadFile` copied deny rules instead of sharing them, it would diverge quickly from `ShellSafety`.
- Plan: expose one helper from `secret_patterns.rs` and make `ReadFile` depend on it directly.

### Scope creep into Phase 3b
- Wiring `read_file` into `build_default_turn()` now would violate the requested boundary and risk changing T1 behavior.
- Plan: do not touch tier/tool selection in this phase; ship a standalone tool plus config and tests only.
