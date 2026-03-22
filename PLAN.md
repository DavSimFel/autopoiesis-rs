# Standing Approvals Plan

Scope: add a third shell-policy tier for pre-approved but audited commands.

## Mechanical changes

### `src/config.rs`
- Add `standing_approvals: Vec<String>` to `ShellPolicy`.
- Keep the field defaulting to an empty list when `agents.toml` omits it.
- Extend the config tests to cover parsing a populated `standing_approvals` list and the empty default case.

### `src/gate/shell_safety.rs`
- Store the standing-approval patterns on `ShellSafety`.
- Evaluate in this order: deny, allow, standing approval, default action.
- When a standing approval matches, return `Verdict::Allow`, log the matched pattern to stderr, and record the match in an internal audit field.
- Keep allow/deny/default behavior unchanged for all non-standing cases.

### Tests
- Add coverage for:
  - standing approval match returns `Allow`
  - deny still overrides standing approval
  - allow still overrides standing approval and does not log
  - unmatched commands still follow the configured default
  - empty standing approvals do not change existing behavior

## Review checklist
- Confirm the new config field deserializes with and without an explicit TOML value.
- Confirm the standing-approval path does not change deny/allow precedence.
- Confirm the audit/log path only fires for standing approvals.
- Run `cargo check` after each code edit, then full fmt/test/clippy before commit.
