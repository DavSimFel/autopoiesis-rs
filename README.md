# autopoiesis

A lightweight agent runtime in Rust. One binary, one tool (shell), messages in, actions out.

## What it does

- **Interactive CLI agent** — prompt from the command line or drop into a REPL
- **HTTP + WebSocket server** — queue-driven sessions with streaming token output
- **Shell as the universal tool** — file I/O, web requests, process management, self-configuration — all through `sh -lc`
- **Guard pipeline** — secret redaction, shell safety checks, exfiltration detection. Deny > approve > allow semantics
- **RLIMIT sandbox** — child processes run with NPROC, FSIZE, and CPU limits
- **Session persistence** — daily JSONL files with full tool call round-trip
- **SQLite message queue** — ordered, persistent, crash-recoverable inbox per session
- **Identity system** — constitution, personality, and context assembled from markdown files with template variables
- **OAuth device flow** — authenticate with OpenAI, token auto-refresh
- **Token-aware context trimming** — stays within budget, drops oldest turns first

## Usage

```bash
# Build
cargo build --release

# One-shot prompt
./target/release/autopoiesis "list files in the current directory"

# Interactive REPL
./target/release/autopoiesis

# Start HTTP + WebSocket server
./target/release/autopoiesis serve --port 8423

# Auth
./target/release/autopoiesis auth login
./target/release/autopoiesis auth status
./target/release/autopoiesis auth logout
```

## Server API

```
POST /api/sessions                    Create session
POST /api/sessions/:id/messages       Enqueue message
GET  /api/sessions/:id/messages/next  Dequeue next pending message
GET  /api/health                      Health check
WS   /api/ws/:session_id              Streaming chat (send JSON, receive token stream)
```

All endpoints require `X-API-Key` header (or `?api_key=` query param for WebSocket).

## Configuration

`agents.toml` in the working directory:

```toml
[agent]
model = "gpt-5.3-codex-spark"
reasoning_effort = "medium"
```

Identity files in `identity/`:
- `constitution.md` — safety boundaries, amendment rules
- `identity.md` — name, voice, behavior defaults
- `context.md` — working memory, active focus

Template variables (`{{model}}`, `{{cwd}}`, `{{tools}}`) are resolved at runtime.

## Architecture

```
main.rs          CLI entrypoint, REPL, server launch
├─ agent.rs      Agent loop: turn orchestration, tool execution, approval flow
├─ turn.rs       Context assembly + guard checks + tool dispatch
├─ context.rs    ContextSource trait: Identity (prompt files) + History (token-budgeted replay)
├─ tool.rs       Shell tool: async execution, RLIMIT sandbox, process-group timeout kill
├─ guard.rs      Guard pipeline: SecretRedactor, ShellSafety, ExfilDetector
├─ session.rs    JSONL persistence, token tracking, context trimming
├─ store.rs      SQLite session registry + message queue
├─ server.rs     axum HTTP + WebSocket, API key auth middleware
├─ llm/
│  ├─ mod.rs     LlmProvider trait, message types, tool call structs
│  └─ openai.rs  OpenAI Responses API, SSE streaming, token counting
├─ auth.rs       OAuth device flow, token refresh
├─ config.rs     agents.toml loading
├─ identity.rs   System prompt from identity/*.md files
├─ template.rs   {{var}} template rendering
└─ util.rs       Timestamps, helpers
```

## Tests

```bash
cargo test                        # 92 unit tests
cargo test --features integration # + live API tests (requires auth)
```

## License

MIT
