# Guarded Shell Executor

The guarded shell execution sequence is now centralized in `src/agent/shell_execute.rs`.
It handles shell tool calls through the existing guard pipeline, approval flow, output redaction, and output capping so the agent loop and future plan-step execution share the same behavior.
