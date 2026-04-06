# autopoiesis

A lightweight Rust agent runtime with a tiered execution model, guarded shell tooling, structured planning, and a SQLite-backed queue.

## Layout

- Generated runtime data defaults to `.aprs/`.
- Sessions live under `.aprs/sessions/`.
- The shared SQLite queue/store lives at `.aprs/queue.sqlite`.
- Runtime workspace defaults to `.aprs/workspace/`.
- OAuth auth is stored at `~/.aprs/auth.json`.
- Shipped prompts live under `src/shipped/identity-templates/`.
- Shipped skills live under `src/shipped/skills/`.

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
- OAuth device-code auth and browser-based OAuth2 + PKCE login.
- Optional ratatui TUI for interactive CLI sessions (`--features tui`).
- Token-aware context trimming.

## Usage

```bash
cargo build --release
./scripts/code-graph stats .
./scripts/code-graph structure --path src
./target/release/autopoiesis --session ad-hoc "list files in the current directory"
./target/release/autopoiesis enqueue --session silas-t1 "check the queue backlog"
./target/release/autopoiesis --session ad-hoc
cargo run                                   # no-arg default: TUI on "interactive" session
./target/release/autopoiesis serve --port 8423
./target/release/autopoiesis auth login
./target/release/autopoiesis auth browser-login
./target/release/autopoiesis auth status
./target/release/autopoiesis auth logout
./target/release/autopoiesis sub add src/shipped/identity-templates/context.md
./target/release/autopoiesis sub remove src/shipped/identity-templates/context.md
./target/release/autopoiesis sub list
./target/release/autopoiesis plan status 123
./target/release/autopoiesis plan resume 123
./target/release/autopoiesis plan cancel 123
./target/release/autopoiesis plan list
```

When `agents.toml` defines registry-backed always-on sessions such as `silas-t1`, those sessions stay queue-owned. Use `autopoiesis enqueue --session <id> "..."` for them; bare CLI mode only runs ad hoc or request-owned sessions directly.

## Code Graph

This repo is set up to use `code-graph` for semantic code navigation.

- Use [`scripts/code-graph`](scripts/code-graph) so the tool works even if the installed binary is not on `PATH`.
- The local project registry alias is `autopoiesis-rs`.
- Claude Code hooks are installed under `.claude/` for automatic enrichment of symbol-like searches.

Examples:

```bash
./scripts/code-graph stats .
./scripts/code-graph structure --path src --depth 2
./scripts/code-graph find build_turn_for_config
./scripts/code-graph refs build_turn_for_config
./scripts/code-graph context build_turn_for_config
./scripts/code-graph impact build_turn_for_config
```

## Quick Start

```bash
cargo build --release
cp agents.toml agents.local.toml 2>/dev/null || true
./target/release/autopoiesis auth login       # device-code flow
./target/release/autopoiesis auth browser-login  # browser OAuth2 + PKCE
./target/release/autopoiesis --session ad-hoc "show me the repo layout"
```

### TUI mode

```bash
cargo build --release
./target/release/autopoiesis
```

Launches a three-region ratatui interface: output scroll, status bar (active tool + budget), and an input box. `Ctrl+C` twice force-quits. TUI is compiled in by default (`tui` is a default feature); disable with `--no-default-features` for a headless build.

When invoked with no arguments, the TUI build defaults to an `"interactive"` session (created on first use). Pass `--session <name>` to resume a named session.

The binary reads `agents.toml` from the working directory. If the file is missing, the runtime falls back to built-in defaults and the shipped assets under `src/shipped/`.

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

Default shipped asset paths:

- `constitution.md` - policy layer
- `agents/<name>/agent.md` - T1 character layer
- `context.md` - runtime context layer
- `src/shipped/skills/*.toml` - local shipped skill definitions

Template variables such as `{{model}}`, `{{cwd}}`, and `{{tools}}` are resolved at runtime.

If you use domain context packs, `context_extend` must stay under `src/shipped/identity-templates/`.

## Runtime Data

Autopoiesis keeps generated data out of the repo root by default:

- `.aprs/sessions/` stores per-session JSONL history and result artifacts.
- `.aprs/queue.sqlite` stores sessions, queue rows, subscriptions, and plan state.
- `.aprs/workspace/` is the default generated workspace root.

That separation is intentional:

- `src/shipped/` is versioned, shipped input data.
- `.aprs/` is generated, local runtime state.

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
├─ tui/                          (feature = "tui", ratatui + crossterm)
│  ├─ event.rs
│  ├─ state.rs
│  ├─ bridge.rs
│  ├─ render.rs
│  ├─ input.rs
│  └─ mod.rs
├─ shipped/
│  ├─ identity-templates/
│  └─ skills/
├─ lib.rs
├─ paths.rs
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
cargo build --release
cargo test
cargo test --features integration
cargo fmt --check
cargo clippy -- -D warnings
```

## License

MIT
