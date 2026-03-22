# Plan: Update documentation to reflect P0 fixes and current code state

## Context

Three P0 fixes shipped on 2026-03-22 (commits b03843b, 33ef098, a54f212) plus a docs cleanup (4b0d967). The markdown docs still describe these bugs as open/unfixed, and several line counts and test counts are stale. Additionally, vision.md has unstaged local changes that add MVP/V1/V2 milestone tags — these should be committed as part of this update.

## Files to modify

### 1. `docs/current/risks.md` — Mark P0s as fixed, update date

**Changes:**
- Move P0-1, P0-2, P0-3 from "P0 — Critical" to a new "## Fixed" section at the bottom (or mark each with a ✓ FIXED tag and reference the fixing commit)
- Recommended approach: keep them documented but clearly marked as resolved, with commit refs. This preserves the audit trail while making the current risk posture clear
- Update the "Updated:" date at the top

**Specifics:**
- P0-1: Fixed in a54f212 — `ShellSafety::with_policy()` in guard.rs, `[shell]` config in agents.toml
- P0-2: Fixed in b03843b — `Principal` enum in server.rs enforces role based on auth key
- P0-3: Fixed in 33ef098 — `MAX_DENIALS_PER_TURN` + `break 'agent_turn` in agent.rs

### 2. `docs/current/architecture.md` — Update module map line counts

**Changes:**
- Update line 8: "17 source files, ~8K lines" → "17 source files, ~7.9K lines"
- Update module map (lines 12-30) with actual counts:
  ```
  main.rs (213L)        → unchanged description
  agent.rs (1400L)      → was 1261L
  server.rs (1110L)     → was 870L
  session.rs (956L)     → correct
  llm/openai.rs (948L)  → correct
  guard.rs (675L)       → was 897L
  context.rs (383L)     → correct
  turn.rs (351L)        → was 349L
  tool.rs (322L)        → correct
  store.rs (297L)       → correct
  auth.rs (401L)        → correct
  config.rs (264L)      → was 177L
  identity.rs (165L)    → correct
  template.rs (86L)     → correct
  util.rs (95L)         → correct
  lib.rs (14L)          → correct
  llm/mod.rs (185L)     → (already listed or add)
  ```
- Add note about shell policy to the guard pipeline section (ShellSafety now supports configurable policy)
- Update "Two execution paths, one Turn" to note that `build_default_turn` now takes a config with shell policy

### 3. `docs/roadmap.md` — Move P0 fixes to Done

**Changes:**
- In section "1. Security stack", mark items 1a as done (all 3 P0 PRs merged)
- Add to "Done" section:
  - P0-1: Shell default-approve with configurable policy (`[shell]` in agents.toml)
  - P0-2: HTTP role enforcement via Principal enum
  - P0-3: Approval denial terminates turn (MAX_DENIALS_PER_TURN + break)
- Keep 1b (standing approvals), 1c (taint tracking), 1d (budget enforcement) as not-yet-done

### 4. `README.md` — Fix test count

**Changes:**
- Line 93: "# 124 unit tests" → remove hardcoded count or update to "# run unit tests"
  - Prefer removing the count entirely since it will go stale again

### 5. `AGENTS.md` — Update line count

**Changes:**
- Line 27: "17 Rust source files (~7.6K lines)" → "17 Rust source files (~7.9K lines)"

### 6. `docs/vision.md` — Commit existing unstaged changes

The file already has local changes (MVP/V1/V2 milestone tags added throughout). These should be reviewed and committed as-is — they look correct and were part of the prior docs cleanup that didn't get staged.

**Status:** Verified — the unstaged changes are intact and match the intended milestone-tagged version.

## Verification

1. `git diff` — confirm all changes are as expected
2. Read each modified file to verify no broken markdown links
3. `cargo test` — docs changes shouldn't affect tests, but sanity check
4. Cross-reference: every claim in risks.md about current state should match what the code does
