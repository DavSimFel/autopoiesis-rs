# Docs Index

> Reading order by audience:
> - **Operator** (David): README → vision → roadmap
> - **Agent / Reviewer** (Silas): index → relevant spec or architecture doc → risks
> - **Coding agent** (Codex): AGENTS.md → relevant docs for the files being changed

## Live Documents

| Document | Path | Purpose | Updated |
|----------|------|---------|---------|
| **Vision** | [vision.md](vision.md) | Where we're going | On design decisions |
| **Roadmap** | [roadmap.md](roadmap.md) | What's next, priority order | After every merge |
| **Risks** | [risks.md](risks.md) | Live hazards, open P1s | After reviews |

## Architecture (as-built)

| Document | Path | Covers |
|----------|------|--------|
| **Overview** | [architecture/overview.md](architecture/overview.md) | Module map, execution flow, data model |

## Specs (pre-implementation)

| Spec | Path | Status |
|------|------|--------|
| **Identity v2** | [specs/identity-v2.md](specs/identity-v2.md) | Design complete, not built |

## Research (cited by living docs)

| Document | Path | Referenced by |
|----------|------|---------------|
| **Security Model** | [research/security-model.md](research/security-model.md) | Referenced by identity-v2 design process (debate files) |

## Archive

Superseded specs, retired research, and obsolete docs: [archive/](archive/)

## Rules

- Every merge that changes `src/` must update relevant docs in the same merge.
- Specs draft in `docs/specs/`. After shipping: fold into architecture, archive spec.
- Research stays only while cited by a living spec or architecture doc.
- Architecture docs describe current code, not future intent.
