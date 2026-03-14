# Review Fix Verification Tests

These tests verify the P1 fixes from the xhigh code review.

## Fix #3: Tool-call ordering must be deterministic
- Stream 3 tool calls in order (A, B, C)
- After parsing, tool_calls vec must be [A, B, C] in insertion order

## Fix #4: Concurrent function-call argument streams
- Interleaved delta events for two different call_ids
- Must produce two separate, correct tool calls

## Fix #5: load_today trims to context limit
- Create session with low token limit
- Persist many messages exceeding the limit
- load_today should trim automatically

## Fix #8: main.rs uses lib.rs module tree
- main.rs should use `use autopoiesis::*` not `mod agent; mod auth; ...`
