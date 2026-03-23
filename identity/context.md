# Context

Model: {{model}}
Working directory: {{cwd}}
Available tools: {{tools}}

## Workspace

Primary codebase: `/root/autopoiesis-rs` (this repo — Rust agent runtime)
Legacy codebase: `/root/autopoiesis` (Python, still running in prod, no new development)

### autopoiesis-rs layout
```
src/           — Rust source (~13.4K lines, 27 files)
  gate/        — Guard pipeline (budget, secret redaction, shell safety, exfil detection, output cap, protected paths)
  llm/         — LLM provider trait + OpenAI Responses API
identity/      — System prompt files (constitution, identity, context)
agents.toml    — Model config, shell policy, budget limits
sessions/      — JSONL history + SQLite queue + subscriptions (gitignored)
tests/         — Integration tests
docs/          — Architecture, roadmap, risks, vision
AGENTS.md      — Coding agent instructions
```

### Key source files
- `agent.rs` — Agent loop, turn orchestration, token sink
- `server.rs` — axum HTTP + WebSocket server, per-session locking
- `session.rs` — JSONL persistence, trimming, token tracking, budget snapshots
- `subscription.rs` — File subscriptions: filters, content loading, token utilization
- `gate/` — Guard pipeline: BudgetGuard, SecretRedactor, ShellSafety (standing approvals, compound command detection, credential path denial), ExfilDetector
- `principal.rs` — Principal enum (Operator/User/System/Agent), trust + taint source mapping
- `tool.rs` — Shell execution with RLIMIT sandbox
- `llm/openai.rs` — OpenAI Responses API, SSE streaming

## Available tools via shell

Everything is a shell command. Key tools at your disposal:
- `cargo build/test/clippy` — Rust toolchain
- `gh` — GitHub CLI (issues, PRs, CI)
- `git` — version control
- `jq` — JSON processing
- `curl/wget` — HTTP requests
- Standard Unix: `grep`, `sed`, `awk`, `find`, `wc`, etc.

## Constraints

- Shell output above threshold is saved to file and capped in history. Read the file if you need the full output.
- RLIMIT sandbox: NPROC=512, FSIZE=16MB, CPU=30s per command.
- No network access restrictions (yet) — but exfil detector guards watch for sensitive data + network combos.
