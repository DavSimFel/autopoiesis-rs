# Doc Review 2026-03-24

Scope: `README.md`, `AGENTS.md`, `docs/index.md`, `docs/vision.md`, `docs/roadmap.md`, `docs/risks.md`, `docs/architecture/overview.md`, `docs/specs/identity-v2.md`.

## Findings

1. **Medium - identity naming is split across "current" and "future" docs without a bridge.**
   - `README.md:61-64` and `docs/architecture/overview.md:104-108` describe the live stack as `identity/constitution.md`, `identity/identity.md`, `identity/context.md`.
   - `docs/vision.md:75-83` and `docs/specs/identity-v2.md:23-29, 41-49` describe `agent.md` in `identity-templates/` as if it is the active model.
   - Fix: add a one-line bridge that says "README/architecture describe v1; vision/spec describe v2," or rename the current-state sections so a junior dev can tell which paths are live today.

2. **Medium - `docs/risks.md` has a stale file name.**
   - `docs/risks.md:42-43` says: `constitution.md and operator.md are described as immutable/operator-only.`
   - There is no `operator.md` in the current docs or tree; the v2 spec uses `agent.md`.
   - Fix: replace `operator.md` with the real file name and say whether this is a legacy v1 reference or a v2 goal.

3. **Low - `docs/index.md` uses audience names with no legend.**
   - `docs/index.md:3-6` says `David`, `Silas`, and `Codex`, but never explains who David or Silas are.
   - Fix: add a short legend or rename the rows to roles like `operator`, `implementer`, and `agent author`.

4. **Low/Medium - the research table claims live citations that are not present in the reviewed docs.**
   - `docs/index.md:28-32` labels `research/security-model.md` as `Referenced by: identity-v2, risks`.
   - In the requested doc set, those live docs do not actually link to the security model.
   - Fix: either add direct links from the living docs or move the row out of the "cited by living docs" table.

## Overall

No broken relative links were found in the requested files; the main cleanup is about terminology drift and making the v1 -> v2 transition explicit.
