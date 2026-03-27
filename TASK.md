# Task: LOC Reduction — Production Code Optimization

NO REGRESSIONS. 624 tests must pass. Reduce production lines of code by removing duplication, generalizing code paths, and improving patterns. Only real production code counts.

## Optimizations to implement (8 commits, one per optimization)

### 1. Delete src/plan/executor.rs (if it still exists)
Check first. If already deleted, skip this commit.

### 2. Queue/drain consolidation (~145 prod lines)
- src/session_runtime/drain.rs: extract shared helper for system/assistant/unsupported-role handling
- Replace the two hand-written match message.role.as_str() blocks with one generic path
- src/agent/queue.rs: reduce to minimal public shim
- Keep pub async fn drain_queue(...) signature stable

### 3. Store forwarding removal (~285 prod lines)
- src/store/mod.rs: delete the ~43 forwarding methods in the impl Store block
- Move impl Store blocks into: sessions.rs, message_queue.rs, plan_runs.rs, step_attempts.rs, subscriptions.rs
- Each submodule gets its own impl Store { } with the methods that were forwarded
- Keep ALL public API signatures identical — callers must not notice
- The Store struct and constructor stay in mod.rs

### 4. plan_runs.rs SQL/update/query dedup (~140 prod lines)
- Factor repeated validation patterns and SQL query construction
- Use a small helper for the dynamic UPDATE SET field = CASE builder
- Keep all existing behavior identical

### 5. step_attempts.rs validation/finalization dedup (~70 prod lines)
- Factor repeated validation and finalization patterns
- Keep all existing behavior identical

### 6. command_path_analysis.rs write-side dedup (~155 prod lines)
- Consolidate the identity_template_* detection chain into a shared pipeline
- Keep existing public entry points and outward semantics unchanged

### 7. command_path_analysis.rs read-side dedup (~135 prod lines)
- Collapse the 5 paired protected/target read-analysis functions into a shared matcher
- Keep existing public entry points and outward semantics unchanged

### 8. Test fixture consolidation (~85 test lines)
- Deduplicate test_store() across plan/notify.rs, plan/patch.rs, plan/recovery.rs
- Deduplicate test_state() across server/auth.rs, server/http.rs
- One shared #[cfg(test)] support module
- Test-only commit, no production behavior change

## Safety Rules
- Zero behavior change per optimization
- Test count must not drop below 624
- No pub (not pub(crate)) signature changes
- One pattern per commit
- After each commit: cargo fmt --check, cargo clippy -- -D warnings, cargo test
- Macros are last resort — prefer functions, generics, traits

## Discovery approach
- Run jscpd and tokei FIRST before implementing
- Count exact before/after lines for each optimization
- Start from biggest files, work top-down

When completely finished, run: openclaw system event --text "Done: LOC reduction — 8 optimizations committed" --mode now
