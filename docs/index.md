# Docs Index

> Reading order by role:
> - Operator: README -> vision -> roadmap -> risks
> - Implementer: AGENTS.md -> risks -> architecture/overview -> relevant spec
> - Reviewer: index -> the docs for the file area being changed -> risks

## Live Docs

| Document | Path | Purpose |
|----------|------|---------|
| Vision | [vision.md](vision.md) | Current system direction and shipped capabilities |
| Roadmap | [roadmap.md](roadmap.md) | What remains after the shipped phases |
| Risks | [risks.md](risks.md) | Open hazards and resolved audit items |

## Architecture

| Document | Path | Covers |
|----------|------|--------|
| Overview | [architecture/overview.md](architecture/overview.md) | Module map, data flow, and current runtime behavior |
| Guarded Shell Executor | [architecture/guarded-shell-executor.md](architecture/guarded-shell-executor.md) | Shared shell execution path |

## Specs

| Document | Path | Status |
|----------|------|--------|
| Identity v2 | [specs/identity-v2.md](specs/identity-v2.md) | Implemented, describes the live identity stack |
| Plan Engine | [specs/plan-engine.md](specs/plan-engine.md) | Implemented, describes the live plan executor |

## Reference

| Document | Path | Purpose |
|----------|------|---------|
| README | [README.md](../README.md) | Entry point and usage |
| AGENTS | [AGENTS.md](../AGENTS.md) | Working instructions for codex agents |

## Archive

| Document | Path | Purpose |
|----------|------|---------|
| Security model research | [archive/security-model.md](archive/security-model.md) | Historical background only |
| Vibe coding research | [archive/vibe-coding-2026.md](archive/vibe-coding-2026.md) | External industry research |
| Doc review audit 2026-03-24 | [archive/doc-review-2026-03-24.md](archive/doc-review-2026-03-24.md) | Completed documentation audit |
| Restructure plan | [archive/restructure-plan.md](archive/restructure-plan.md) | Completed historical structure plan |

## Rules

- Architecture docs describe the current code, not future intent.
- Shipped specs stay live when they are still the most precise normative docs.
- Research stays only while it is actually referenced.
- Every merge that changes `src/` must update the docs that describe that code.
