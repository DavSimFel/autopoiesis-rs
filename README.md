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
├─ app/
│  ├─ args.rs
│  ├─ enqueue_command.rs
│  ├─ plan_commands.rs
│  ├─ session_run.rs
│  ├─ subscription_commands.rs
│  └─ tracing.rs
├─ agent/
│  ├─ loop_impl.rs (+submodules)
│  ├─ queue.rs (+submodules)
│  ├─ shell_execute.rs
│  ├─ child_drain.rs (+submodules)
│  ├─ audit.rs
│  └─ usage.rs
├─ child_session/
│  ├─ create.rs
│  └─ completion.rs
├─ server/
│  ├─ http.rs
│  ├─ ws.rs
│  ├─ auth.rs
│  ├─ queue.rs
│  ├─ queue_worker.rs
│  ├─ session_lock.rs
│  └─ state.rs
├─ gate/
│  ├─ budget.rs
│  ├─ shell_safety.rs
│  ├─ secret_redactor.rs
│  ├─ exfil_detector.rs
│  ├─ output_cap.rs
│  ├─ streaming_redact.rs
│  ├─ secret_catalog.rs
│  ├─ protected_paths.rs
│  └─ command_path_analysis.rs
├─ llm/
│  ├─ history_groups.rs
│  └─ openai/
│     ├─ request.rs
│     └─ sse.rs
├─ plan.rs
├─ plan/
│  ├─ runner.rs
│  ├─ notify.rs
│  ├─ patch.rs
│  └─ recovery.rs
├─ config/
│  ├─ agents.rs
│  ├─ domains.rs
│  ├─ file_schema.rs
│  ├─ load.rs
│  ├─ models.rs
│  ├─ policy.rs
│  ├─ runtime.rs
│  └─ spawn_runtime.rs
├─ context/
│  ├─ history.rs
│  ├─ identity_prompt.rs
│  ├─ session_manifest.rs
│  ├─ skill_instructions.rs
│  ├─ skill_summaries.rs
│  └─ subscriptions.rs
├─ session/
│  ├─ budget.rs
│  ├─ delegation_hint.rs
│  ├─ jsonl.rs
│  └─ trimming.rs
├─ session_runtime/
│  ├─ drain.rs
│  └─ factory.rs
├─ store/
│  ├─ message_queue.rs
│  ├─ migrations.rs
│  ├─ sessions.rs
│  ├─ plan_runs.rs
│  ├─ step_attempts.rs
│  └─ subscriptions.rs
├─ turn/
│  ├─ builders.rs
│  ├─ tiers.rs
│  └─ verdicts.rs
├─ observe/
│  ├─ otel.rs
│  └─ sqlite.rs
├─ lib.rs
├─ tool.rs
├─ skills.rs
├─ subscription.rs
├─ delegation.rs
├─ model_selection.rs
├─ session_registry.rs
├─ read_tool.rs
├─ principal.rs
├─ identity.rs
├─ template.rs
├─ terminal_ui.rs
├─ test_support.rs
├─ auth.rs
├─ logging.rs
└─ time.rs
```

## Safety

The guard pipeline reduces blast radius, but it is not a sandbox. Shell commands still run as the current user with filesystem and network access. RLIMIT caps only cover process count, file size, and CPU.

## Roles

Throughout the docs and commit history, three names appear:
- **David** — human operator and project owner
- **Silas** — the AI agent persona running on this runtime (currently hosted on OpenClaw, migrating to autopoiesis)
- **Codex** — the OpenAI Codex coding agent used to implement changes to this repo

## Documentation

- [docs/index.md](docs/index.md) - docs manifest and reading order
- [docs/architecture/overview.md](docs/architecture/overview.md) - current runtime architecture
- [docs/risks.md](docs/risks.md) - known hazards and resolved audit items
- [docs/roadmap.md](docs/roadmap.md) - what remains
- [docs/vision.md](docs/vision.md) - shipped capabilities and the remaining direction
- [docs/specs/](docs/specs/) - implementation-backed specs
- [AGENTS.md](AGENTS.md) - working instructions for codex agents

## Stats

- `src/` Rust source files: `111`
- `src/` Rust source lines: `42,699`
- Rust tests in `src/` + `tests/`: `533`
- Commits on `HEAD`: `190`

## Tests

```bash
cargo test
cargo test --features integration
cargo fmt --check
cargo clippy -- -D warnings
```

## License

MIT
