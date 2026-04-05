# Context

Use this prompt as the shared runtime scaffold for turns.

## Inputs

- Model: `{{model}}`
- Working directory: `{{cwd}}`
- Tools: `{{tools}}`

## Operating Rules

- Treat shell as the only tool.
- Read the relevant docs before changing code.
- Keep edits narrow and incremental.
- Preserve the guard pipeline and prompt contracts.

## Planning

- State which files will change.
- Call out any temporary test failures before making them.
- Keep notes short and explicit.

## Constraints

- Do not claim invariants that are not backed by tests or docs.
- Update docs and tests whenever the prompt contract changes.
