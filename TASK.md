# LOC Reduction Task

NO REGRESSIONS. 595 tests must pass. Reduce production lines of code by removing duplication, generalizing code paths, and improving patterns. Only real production code counts.

## Step 1: Run Analysis Tools FIRST

Before writing anything, run these and include output in your reasoning:

1. `npx jscpd src/ --min-lines 5 --min-tokens 50` — find code clones
2. `tokei src/ --sort lines` — real LOC breakdown
3. grep-based searches per discovery rules below

## Discovery Rules

1. **Grep Test:** 3+ structurally identical code blocks differing only in names/values = candidate. Prove with grep.
2. **Deletion Test:** Can you delete a function and have callers use something that already exists? Thin wrappers with no added logic/safety = candidate.
3. **Signature Test:** Functions with 5+ params doing similar things = wants struct method or builder.
4. **One More Case Test:** match/if-else with N arms doing nearly the same thing with different values = table or loop.
5. **Module Boundary Test:** Module A importing 3+ things from B's internals to reconstruct what B does = wrong abstraction location.
6. **cfg(test) Pollution Test:** #[cfg(test)] code in non-test files beyond assert imports is suspect. Test infra in prod code should be behind trait seams.

## Safety Rules (non-negotiable)

7. **Zero behavior change.** Every optimization must be provably equivalent.
8. **Test count must not drop.** 595 before = 595 after. Moving tests fine. Deleting tests not.
9. **Public API freeze.** No `pub` (not `pub(crate)`) signature changes.
10. **One pattern per commit.** Don't mix optimizations.

## Search Strategy

11. Start from biggest files, work top-down.
12. Look for parallel structures — two modules solving the same problem independently.
13. Count, don't guess. State "removes N prod lines, touches M test lines" for each.
14. Macros are last resort. Prefer functions, generics, traits.

## Known Hotspots (verify, don't assume)

- store/mod.rs: ~47 delegation methods possibly eliminable via distributed `impl Store` blocks in submodules
- gate/command_path_analysis.rs: 126 mentions of identity_template, ~10 parallel detection functions
- test_store() duplicated across plan/notify.rs, plan/patch.rs, plan/recovery.rs
- plan/runner.rs: #[cfg(test)] thread_local pollution in prod code
- plan/runner.rs + agent/shell_execute.rs: parallel guarded shell execution patterns

## Output

Write PLAN.md with:
1. Tool output summaries (jscpd clones found, tokei breakdown)
2. Each optimization: what, where, lines saved (prod), risk level
3. Order of operations (each step keeps tests green)
4. Total expected savings

When completely finished, run: openclaw system event --text "Done: PLAN.md written for LOC reduction" --mode now
