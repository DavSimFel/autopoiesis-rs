# Plan Revision Feedback

Read PLAN.md (current plan) and TASK.md (original task).

The plan converged at 375 production lines saved. This is too conservative. A surface-level scan before Codex even started found 1000-1500 lines of opportunity. The plan stopped at low-hanging fruit.

## Specific Gaps

### 1. Store SQL boilerplate (MISSED ENTIRELY)
plan_runs.rs (588 prod lines) and step_attempts.rs (348 prod lines) have repetitive validation patterns and query construction. Count the duplication.

### 2. command_path_analysis.rs (UNDERSIZED)
Plan only addresses 5 paired read functions. There are ~10 identity_template_* functions that are structurally parallel detection chains:
- identity_template_script_writes_path
- identity_template_shell_wrapper_script
- identity_template_args_write_redirection
- identity_template_redirection_token_writes_target
- identity_template_direct_write_command
- identity_template_mentions_target
- identity_template_destination_argument
- identity_template_git_subcommand

135 lines is too conservative for this file.

### 3. session_runtime/drain.rs (UNDERSIZED)
Plan claims 90 lines. The two near-identical drain paths in 581 lines are bigger than that. Count the actual duplicated blocks.

### 4. Signature Test (NEVER RUN)
Functions with 5+ parameters doing similar work across call sites. Run this discovery rule now.

### 5. Test helpers (EXCLUDED)
test_store() clones across plan/notify.rs, plan/patch.rs, plan/recovery.rs are ~200 lines. Test code IS real code worth deduplicating. Include as separate tracked category.

### 6. One More Case Test (NEVER RUN)
Match/if-else chains with N arms doing nearly the same thing. Run this now.

## Requirements
- Revise PLAN.md with additional optimizations
- Each new optimization: what, where, measured line count, risk
- Target: 800+ total lines (prod + test dedup tracked separately)
- Do NOT remove existing optimizations, ADD to them

When completely finished, run: openclaw system event --text "Done: PLAN.md revised with expanded optimizations" --mode now
