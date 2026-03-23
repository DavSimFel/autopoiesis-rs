# autopoiesis

A lightweight agent runtime in Rust. One binary, one tool (shell), messages in, actions out.

## What it does

- **Interactive CLI agent** — prompt from the command line or drop into a REPL
- **HTTP + WebSocket server** — queue-driven sessions with streaming token output
- **Shell as the universal tool** — file I/O, web requests, process management, self-configuration — all through `sh -lc`
- **Guard pipeline** — secret redaction, shell safety checks, exfiltration detection. Deny > approve > allow semantics
- **RLIMIT resource caps** — child processes run with NPROC, FSIZE, and CPU limits; this is not filesystem or network isolation
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
GET  /api/health                      Health check
GET  /api/sessions                    List sessions
POST /api/sessions                    Create session
POST /api/sessions/:id/messages       Enqueue message
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
main.rs            CLI entrypoint, REPL, server launch, sub add/remove/list
├─ agent.rs        Agent loop: turn orchestration, tool execution, approval flow
├─ turn.rs         Context assembly + guard checks + tool dispatch
├─ context.rs      ContextSource trait: Identity (prompt files) + History (replay)
├─ tool.rs         Shell tool: async execution, RLIMIT caps, process-group timeout kill
├─ gate/
│  ├─ mod.rs       Guard trait, Verdict/Severity, guard_text_output/guard_message_output
│  ├─ shell_safety.rs     Policy-driven allow/deny, standing approvals, compound command detection
│  ├─ secret_redactor.rs  Regex secret redaction in message content
│  ├─ secret_patterns.rs  Shared pattern catalog + protected credential path detection
│  ├─ streaming_redact.rs Byte-by-byte secret redaction during SSE streaming
│  ├─ exfil_detector.rs   Cross-call read+send pattern detection
│  ├─ budget.rs    Per-turn/session/day token ceiling enforcement
│  └─ output_cap.rs       Shell output cap + file-backed result storage
├─ subscription.rs File subscriptions: filters, content loading, token utilization
├─ session.rs      JSONL persistence, token tracking, context trimming, budget snapshots
├─ store.rs        SQLite session registry + message queue + subscriptions
├─ server.rs       axum HTTP + WebSocket, Principal-based auth middleware
├─ llm/
│  ├─ mod.rs       LlmProvider trait, message types, tool call structs
│  └─ openai.rs    OpenAI Responses API, SSE streaming, token counting
├─ auth.rs         OAuth device flow, token refresh
├─ config.rs       agents.toml loading, ShellPolicy, BudgetConfig
├─ principal.rs    Principal enum, trust + taint source mapping
├─ identity.rs     System prompt from identity/*.md files
├─ cli.rs          CLI display helpers, denial formatting
├─ template.rs     {{var}} template rendering
└─ util.rs         Timestamps, helpers
```

## Tests

```bash
cargo test                        # run unit tests
cargo test --features integration # + live API tests (requires auth)
cargo fmt --check                 # formatting
cargo clippy -- -D warnings       # lints
```

## Safety

The guard pipeline (SecretRedactor, ShellSafety, ExfilDetector) provides **risk reduction, not containment**. Shell commands run as the current user with full filesystem and network access. RLIMIT caps NPROC/FSIZE/CPU only. See [docs/current/risks.md](docs/current/risks.md) for known hazards.

## Documentation

- [docs/current/architecture.md](docs/current/architecture.md) — how the code works today
- [docs/current/risks.md](docs/current/risks.md) — known broken invariants and hazards
- [docs/roadmap.md](docs/roadmap.md) — build order and priorities
- [docs/vision.md](docs/vision.md) — future-state design
- [AGENTS.md](AGENTS.md) — instructions for AI agents working on this repo

## License

MIT
