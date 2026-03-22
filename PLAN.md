# Budget Enforcement Plan

## Inputs Reviewed

Already read before writing this plan:

- All source files under `src/`
- `agents.toml`
- `docs/roadmap.md`
- `docs/current/risks.md`

## Scope

Implement roadmap item `1d` as a guard-only budget gate with per-turn, per-session, and per-day token ceilings.

Keep the existing guard precedence and error flow unchanged:

- `resolve_verdict()` still short-circuits on `Deny`
- Budget checks are preflight checks only
- `BudgetGuard` must return `Verdict::Deny` with a clear, user-facing reason

Use the same accounting basis already used by `Session`:

- count `TurnMeta.input_tokens + TurnMeta.output_tokens`
- do not add a second token estimator inside the guard
- do not count `reasoning_tokens` for the budget ceilings unless a later milestone explicitly changes the policy

## File Changes

### `src/config.rs`

- Add `BudgetConfig` with these fields, all `Option<u64>`:
  - `max_tokens_per_turn`
  - `max_tokens_per_session`
  - `max_tokens_per_day`
- Add `pub budget: Option<BudgetConfig>` to `Config`.
- Parse `[budget]` from `agents.toml`.
- Keep the section optional:
  - missing `[budget]` means `Config::budget == None`
  - present section with omitted keys means the corresponding fields stay `None`
- Add config tests for:
  - full budget table
  - partial budget table
  - missing budget table

### `src/gate/mod.rs`

- Add `pub mod budget;`
- Re-export `BudgetGuard` with `pub use budget::BudgetGuard;`
- Extend `GuardContext` so it can carry budget counters alongside `tainted`.
  - Recommended shape: a small nested budget snapshot struct with `turn_tokens`, `session_tokens`, and `day_tokens`
  - Keep `Default` zeroed so existing guard tests remain simple
- Do not change `resolve_verdict()` ordering; `BudgetGuard` uses the existing deny precedence

### `src/gate/budget.rs`

- Add a new file with `BudgetGuard` implementing `Guard`.
- Make it stateless with respect to counting:
  - it reads the current counts from `GuardContext`
  - it only compares counts against the configured ceilings
- On `GuardEvent::Inbound(_)`:
  - deny if `turn_tokens` is above `max_tokens_per_turn`
  - deny if `session_tokens` is above `max_tokens_per_session`
  - deny if `day_tokens` is above `max_tokens_per_day`
  - if more than one ceiling is violated, return one deterministic reason that names each violated ceiling
- On every other event, return `Verdict::Allow`
- Use a stable gate id such as `budget`
- Keep the denial reason explicit and actionable, for example:
  - which ceiling was exceeded
  - the configured limit
  - the observed token count
- Add unit tests for:
  - per-turn exceeded
  - per-session exceeded
  - per-day exceeded
  - under budget
  - no effective budget limits set means allow

### `src/session.rs`

- Add the smallest read-only helper(s) needed to expose the live budget snapshot:
  - latest completed turn token total
  - cumulative session token total
  - current day's JSONL token total
- Keep `Session` as the accounting source of truth; do not re-derive counts in the guard or agent loop.

### `src/agent.rs`

- Read the live budget snapshot from `Session` immediately before the inbound guard check.
- Copy that snapshot into a `GuardContext` and pass `Some(context)` to `Turn::check_inbound()`.

### `src/turn.rs`

- Import `BudgetGuard`.
- In `build_default_turn()`, add `BudgetGuard` when budget configuration is enabled.
  - If all three budget fields are `None`, skip adding the guard so the default path stays a no-op
  - Prefer inserting the budget guard early in the inbound guard chain so budget denial short-circuits before other work
- Change `check_inbound()` to accept `Option<GuardContext>` from the caller.
- Merge the optional caller-supplied budget snapshot with the `tainted` flag computed inside `check_inbound()` before calling `resolve_verdict()`.
- Use this optional-parameter path instead of a `set_budget_snapshot()` mutator so `Turn` stays stateless between calls.
- Update inbound call sites and tests that do not have a live snapshot yet to pass `None`.
- Keep the current guard trait and verdict precedence intact
- Update any `GuardContext` struct literals in tests to use `..Default::default()` after the context grows budget fields
- Add turn-level wiring tests for:
  - budget configured and exceeded => deny
  - no budget config => allow

### `src/lib.rs`

- No changes needed
- `gate` is already public

## Budget Snapshot Flow

The guard should not own token accounting. The session layer stays the source of truth.

Planned handoff:

1. `Session` continues to accumulate token totals from persisted `TurnMeta` data.
2. Right before the inbound guard check, `agent.rs` asks `Session` for the live budget snapshot and copies it into a `GuardContext`.
3. `Turn::check_inbound()` accepts `Option<GuardContext>`, merges any caller-supplied budget snapshot with the taint flag it computes locally, and forwards the combined context to `resolve_verdict()`.
4. `BudgetGuard` reads the snapshot and decides whether the inbound message is still admissible.

Snapshot fields:

- `turn_tokens`: the most recent completed assistant turn total
- `session_tokens`: cumulative total for the whole session
- `day_tokens`: cumulative total for the current day's JSONL file

If `Session` is missing a read accessor for one of those values, add the smallest possible read-only helper there rather than re-deriving counts inside the guard.

## Tests

Keep the tests aligned with the existing `#[cfg(test)]` style in this repo.

- `src/config.rs`
  - load a budget table with all three values
  - load a partial budget table
  - keep `budget == None` when the section is absent
- `src/gate/budget.rs`
  - per-turn exceeded
  - per-session exceeded
  - per-day exceeded
  - under budget
  - no effective limits => allow
- `src/turn.rs`
  - `build_default_turn()` wires the guard when config is present
  - `build_default_turn()` does not block when budget config is absent

## Order Of Operations

1. Add `BudgetConfig` and parser support in `src/config.rs`, then run the config tests.
2. Add `src/gate/budget.rs` and its isolated unit tests, with `src/gate/mod.rs` exports and expanded `GuardContext`.
3. Add the `Session` read helper(s), then thread the live token snapshot through `agent.rs` into `Turn::check_inbound(..., Some(context))` so the guard can read current counts without doing its own accounting.
4. Wire `BudgetGuard` into `build_default_turn()` only when budget limits are enabled.
5. Add the turn wiring tests for the configured and no-config cases.
6. Run the repo checks in this order:
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy -- -D warnings`
   - `cargo build --release`
