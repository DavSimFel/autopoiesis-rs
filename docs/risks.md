# Current Risks and Broken Invariants

> Single source of truth for known hazards. Both `AGENTS.md` and `docs/vision.md` link here.
> Updated: 2026-03-26

## Open Bugs

No confirmed P1 bugs remain open after the current audit pass.

## Structural Risks

These are still real, but they are design limitations rather than currently broken invariants.

- Shell guards are heuristic, not a sandbox. Regex, glob, and substring checks reduce risk but do not contain arbitrary execution.
- Shell remains the self-management surface. Taint forces approval, but it does not block execution.
- There is no PTY shell yet.
- There is no filesystem or network sandboxing yet.
- The server principal model is still operator-versus-user, not per-caller multi-tenancy.
- Subscriptions are durable, queryable data, but they are not yet injected into turn-context assembly.
- `History` is still a dormant abstraction unless explicitly wired in; any future use must preserve assistant/tool round-trips.
## Fixed

### Audit fixes

The following audit items are resolved:

- P0-1: Shell containment hardening
- P0-2: HTTP caller role enforcement
- P0-3: Approval denial terminates the turn
- P0-4: Shell metacharacter bypass closes through compound-command approval
- P0-5: Protected credential paths are hard denied
- P1-1: Global server serialization removed
- P1-2: Queue claiming is atomic
- P1-3: Provider-controlled `call_id` is sanitized
- P1-4: SSE trailing events are not dropped
- P1-5: Session replay no longer silently drops unknown entries
- P1-6: History trimming is no longer treated as a live invariant break
- P1-7: Taint does not permanently stick after assistant replies
- P1-8: Denied tool calls are persisted without broken tool-result replay
- P1-10: Budget enforcement now blocks the same turn
- P1-11: Approval prompts now show the user message under review

### Additional resolved issues

- P1-9: `Session::append` now keeps disk persistence and in-memory state aligned.

## Notes

- The remaining architecture gaps are tracked in `docs/roadmap.md`.
- If a structural risk becomes a runtime invariant, it should move here and gain a concrete regression test.
