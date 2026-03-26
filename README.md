# autopoiesis

A lightweight Rust agent runtime with a tiered execution model, guarded shell tooling, structured planning, and a SQLite-backed queue.

## What It Does

- Interactive CLI agent with prompt and REPL modes.
- HTTP and WebSocket server for queue-driven sessions.
- Tiered runtime:
  - T1 and T3 can use shell.
  - T2 uses `read_file` only.
- Guard pipeline for budget, secret redaction, shell safety, exfil detection, and output capping.
- Plan engine for structured T2 work.
- Local skill catalog with summary loading and full T3 skill injection.
- Session persistence in daily JSONL files.
- SQLite store for sessions, queue items, subscriptions, and plans.
- OAuth device flow auth.
- Token-aware context trimming.

## Usage

```bash
cargo build --release
./target/release/autopoiesis "list files in the current directory"
./target/release/autopoiesis
./target/release/autopoiesis serve --port 8423
./target/release/autopoiesis auth login
./target/release/autopoiesis auth status
./target/release/autopoiesis auth logout
./target/release/autopoiesis sub add identity-templates/context.md
./target/release/autopoiesis sub remove identity-templates/context.md
./target/release/autopoiesis sub list
./target/release/autopoiesis plan status 123
./target/release/autopoiesis plan resume 123
./target/release/autopoiesis plan cancel 123
./target/release/autopoiesis plan list
```

## Configuration

`agents.toml` in the working directory:

```toml
[agents.silas]
identity = "silas"

[agents.silas.t1]
model = "gpt-5.4-mini"
reasoning = "medium"

[agents.silas.t2]
model = "gpt-5.4-mini"
reasoning = "low"

[models]
default = "gpt5_mini"

[models.catalog.gpt5_mini]
provider = "openai"
model = "gpt-5.4-mini"
caps = ["fast", "cheap", "reasoning"]
context_window = 128000
cost_tier = "cheap"
cost_unit = 1
enabled = true
```

Identity files live in `identity-templates/`:

- `constitution.md` - policy layer
- `agents/<name>/agent.md` - T1 character layer
- `context.md` - runtime context layer

Template variables such as `{{model}}`, `{{cwd}}`, and `{{tools}}` are resolved at runtime.

## Architecture

```text
main.rs
├─ agent/
│  ├─ loop_impl.rs
│  ├─ queue.rs
│  ├─ shell_execute.rs
│  └─ spawn.rs
├─ server/
│  ├─ mod.rs
│  ├─ http.rs
│  ├─ ws.rs
│  ├─ auth.rs
│  └─ queue.rs
├─ gate/
│  ├─ budget.rs
│  ├─ shell_safety.rs
│  ├─ secret_redactor.rs
│  ├─ exfil_detector.rs
│  ├─ output_cap.rs
│  ├─ streaming_redact.rs
│  └─ secret_patterns.rs
├─ llm/
│  ├─ mod.rs
│  └─ openai.rs
├─ plan.rs
├─ plan/
│  ├─ runner.rs
│  ├─ executor.rs
│  ├─ notify.rs
│  ├─ patch.rs
│  └─ recovery.rs
├─ lib.rs
├─ config.rs
├─ context.rs
├─ session.rs
├─ store.rs
├─ turn.rs
├─ tool.rs
├─ spawn.rs
├─ skills.rs
├─ subscription.rs
├─ delegation.rs
├─ model_selection.rs
├─ read_tool.rs
├─ principal.rs
├─ identity.rs
├─ template.rs
├─ auth.rs
├─ cli.rs
└─ util.rs
```

## Safety

The guard pipeline reduces blast radius, but it is not a sandbox. Shell commands still run as the current user with filesystem and network access. RLIMIT caps only cover process count, file size, and CPU.

## Documentation

- [docs/index.md](docs/index.md) - docs manifest and reading order
- [docs/architecture/overview.md](docs/architecture/overview.md) - current runtime architecture
- [docs/risks.md](docs/risks.md) - known hazards and resolved audit items
- [docs/roadmap.md](docs/roadmap.md) - what remains
- [docs/vision.md](docs/vision.md) - shipped capabilities and the remaining direction
- [docs/specs/](docs/specs/) - implementation-backed specs
- [AGENTS.md](AGENTS.md) - working instructions for codex agents

## Stats

- `src/` Rust source files: `52`
- `src/` Rust source lines: `34,821`
- Rust tests in `src/` + `tests/`: `558`
- Commits on `HEAD`: `159`

## Tests

```bash
cargo test
cargo test --features integration
cargo fmt --check
cargo clippy -- -D warnings
```

## License

MIT
