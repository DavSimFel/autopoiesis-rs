# Prevention Strategy

## Integration Tests

Add end-to-end tests that assert the seams between components, not just local helpers.

1. Queue is the single source of truth.
   - Enqueue a WebSocket message.
   - Assert the worker dequeues it, marks it `processing`, then `processed`.
   - Assert no direct execution path runs without a queue row.

2. HTTP and WebSocket share the same queue semantics.
   - Enqueue identical prompts through HTTP and WebSocket.
   - Assert both paths persist the same session history shape and final queue status.

3. Approval verdicts cannot be bypassed.
   - Return a tool call that requires approval.
   - Assert HTTP rejects it without execution.
   - Assert WebSocket emits an approval request and blocks until a client response arrives.
   - Assert a rejected approval never produces a tool result entry.

4. System prompt survives the full lifecycle.
   - Start with identity files.
   - Add later operational `system` messages.
   - Restart from persisted JSONL.
   - Assert the first system message is still the provider instructions and later system messages remain replayable conversation state.

5. Session trimming preserves structural invariants.
   - Persist `assistant(tool_call) -> tool -> assistant` sequences.
   - Force trim on append and on reload.
   - Assert no retained tool result lacks its originating assistant tool call.

6. Outbound redaction is enforced before user-visible or disk-visible output.
   - Stream a secret from the provider.
   - Return a secret from the tool.
   - Assert the token sink, session JSONL, and tool replay history all contain only redacted text.

7. Timeout cleanup kills descendants.
   - Spawn a child that ignores `SIGTERM` and a descendant that writes a marker later.
   - Force timeout.
   - Assert the marker is never written.

## Pre-Merge Checklist

1. Queue paths:
   - No execution path may consume prompt content without first claiming a queue row.
   - Every claimed queue row must end in `processed` or `failed`.

2. Guard paths:
   - Inbound text, tool calls, tool batches, streamed model output, and tool output all go through guards.
   - No server path may substitute an auto-approve handler for interactive approvals.

3. Prompt handling:
   - The first `system` message is preserved as instructions.
   - Later `system` messages are appended as replayable conversation state.

4. Persistence:
   - Reload covers all session day files, not only today.
   - Reload preserves `system`, `assistant`, `tool`, and tool-call metadata needed for replay.

5. Trimming:
   - Trimming logic is role-aware.
   - Trimming never splits assistant/tool round-trips.

6. Shell execution:
   - Timeout cleanup terminates the whole process group.
   - Docs describe RLIMIT caps honestly and do not call them a sandbox.

7. Secrets:
   - Token files use `0600`.
   - Tests cover both inbound and outbound redaction.

8. Verification:
   - `cargo test`
   - `cargo clippy -- -D warnings`

## Architectural Rules

1. One queue, one worker contract.
   - WebSocket, HTTP, CLI, and recovery paths must all use the same dequeue/mark lifecycle.

2. Approval is part of execution, not UI sugar.
   - Approval-required tool calls must suspend execution until an explicit handler decision arrives.

3. Context sources may add instructions, never redefine ownership of persisted messages.
   - The agent loop owns persistence of the live user message.

4. Session history is a typed transcript, not a bag of strings.
   - Replay and trim code must operate on message roles and tool-call relationships explicitly.

5. Security claims must match implementation.
   - Heuristics and RLIMITs are risk reduction, not sandboxing.
   - If isolation is missing, document the gap and keep TODOs close to the code that needs it.

6. Cross-component invariants need integration tests.
   - Any change that touches queueing, prompting, guards, persistence, or tool execution must add or update an end-to-end invariant test.
