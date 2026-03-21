# Context

Model: {{model}}
Working directory: {{cwd}}
Available tools: {{tools}}

## Workspace

Primary codebase: `/root/autopoiesis-rs` (this repo — Rust agent runtime)
Legacy codebase: `/root/autopoiesis` (Python, still running in prod, no new development)

### autopoiesis-rs layout
```
src/           — Rust source (~7.5K lines, 17 files)
identity/      — System prompt files (constitution, identity, context)
agents.toml    — Model config (model name, reasoning effort)
sessions/      — JSONL history + SQLite queue (gitignored)
tests/         — Integration tests
AGENTS.md      — Coding agent instructions
VISION.md      — Architecture and roadmap
```

### Key source files
- `agent.rs` — Agent loop, turn orchestration, token sink
- `server.rs` — axum HTTP + WebSocket server
- `session.rs` — JSONL persistence, trimming, token tracking
- `guard.rs` — Secret redaction, shell safety, exfil detection
- `tool.rs` — Shell execution with RLIMIT sandbox
- `llm/openai.rs` — OpenAI Responses API, SSE streaming

## Available tools via shell

Everything is a shell command. Key tools at your disposal:
- `cargo build/test/clippy` — Rust toolchain
- `gh` — GitHub CLI (issues, PRs, CI)
- `git` — version control
- `rg` (ripgrep) — fast code search
- `jq` — JSON processing
- `curl/wget` — HTTP requests
- `sqlite3` — query session databases
- Standard Unix: `grep`, `sed`, `awk`, `find`, `wc`, etc.

## Constraints

- Shell output above threshold is saved to file and capped in history. Read the file if you need the full output.
- RLIMIT sandbox: NPROC=512, FSIZE=16MB, CPU=30s per command.
- No network access restrictions (yet) — but exfil detector guards watch for sensitive data + network combos.
