# Guarded Shell Executor

`src/agent/shell_execute.rs` is the shared shell execution path for both the agent loop and the plan engine.

## Sequence

1. Validate the tool call against the shell policy and guard pipeline.
2. Request approval when the verdict requires it.
3. Execute the command through the shell tool.
4. Run outbound text through the redaction and exfiltration guards.
5. Cap output and persist large results to the session artifact directory.
6. Normalize the exit status and return a single result object to the caller.

## Call Sites

- The agent loop uses this path for ordinary shell tool calls.
- The plan executor uses the same path for shell steps and shell checks.

## Non-Goals

- This is not a sandbox.
- Commands still run through `sh -lc`.
- PTY support is not part of this executor.
- The module centralizes behavior; it does not add new policy.
