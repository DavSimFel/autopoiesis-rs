# Restructure Plan

Basis for this plan: `AGENTS.md`, `agents.toml`, `Cargo.toml`, `docs/architecture/overview.md`, and every Rust file under `src/`.

Counting note: `prod` means code outside `#[cfg(test)]` modules and dedicated test-only files. `test` means inline `#[cfg(test)]` code plus dedicated test-only files. `src/` totals: `15,754 prod / 20,353 test / 36,107 total`.

## 1. Current State

### Root And Top-Level Modules

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/auth.rs` | 364 | 49 | Runs local OpenAI device-code auth, token refresh, token persistence, and auth HTTP helpers. |
| `src/cli.rs` | 118 | 64 | Implements terminal token streaming, approval prompts, and denial formatting; it is terminal UI, not CLI argument parsing. |
| `src/config.rs` | 770 | 940 | Loads `agents.toml`, resolves the active agent/tier/domain/model, validates read/subscription/spawn settings, loads skills, and retargets runtime config for spawned children. |
| `src/context.rs` | 555 | 791 | Defines context providers for identity, skills, subscriptions, and history, and also contains token-bounded history replay logic. |
| `src/delegation.rs` | 60 | 129 | Holds delegation thresholds and the logic that decides when to suggest T2 delegation. |
| `src/identity.rs` | 42 | 150 | Loads identity markdown files and picks the T1 vs T2 identity file list. |
| `src/lib.rs` | 25 | 0 | Declares crate modules and reexports `Principal`. |
| `src/main.rs` | 663 | 331 | Owns the clap CLI, tracing setup, auth/plan/subscription subcommands, server launch, interactive REPL, and one-shot queue drain path. |
| `src/model_selection.rs` | 89 | 125 | Selects an enabled model from the configured catalog/routes. |
| `src/plan.rs` | 198 | 256 | Defines plan action/spec types and also parses and validates fenced `plan-json` blocks from assistant text. |
| `src/principal.rs` | 68 | 29 | Defines principals (`User`, `Operator`, `System`, `Agent`) and maps them to taint/trust/request-role behavior. |
| `src/read_tool.rs` | 471 | 431 | Implements the `read_file` tool: argument parsing, path policy, secure open, line slicing, and provenance headers. |
| `src/session.rs` | 620 | 963 | Persists session JSONL history, replays history, trims context, computes budget snapshots, and stores delegation hints. |
| `src/skills.rs` | 175 | 279 | Loads local skills, validates them, resolves requested skills, and estimates their token cost. |
| `src/spawn.rs` | 306 | 936 | Creates child sessions, resolves child tier/model/skills, writes child metadata, and enqueues completion back to the parent. |
| `src/store.rs` | 1966 | 2207 | Owns the SQLite connection and all persistence for sessions, queued messages, subscriptions, plan runs, plan attempts, and schema migration. |
| `src/subscription.rs` | 477 | 125 | Defines subscription filters and records, normalizes subscription paths, estimates tokens, and implements the jq-like subset renderer. |
| `src/template.rs` | 13 | 73 | Performs simple `{{var}}` template substitution. |
| `src/tool.rs` | 491 | 222 | Defines the `Tool` trait and the shell `execute` tool with timeouts, RLIMITs, process-group kill, and bounded output capture. |
| `src/turn.rs` | 436 | 1160 | Defines the `Turn` runtime object, guard precedence, taint handling, and all T1/T2/T3 turn builders. |
| `src/util.rs` | 99 | 95 | Mixes tracing formatter/output-target helpers with UTC timestamp generation. |

### `src/agent/`

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/agent/loop_impl.rs` | 595 | 1 | Runs a single agent turn: inbound checks, streaming, tool execution, denial persistence, budget charging, and delegation hint persistence. |
| `src/agent/loop_impl/tests.rs` | 0 | 1780 | Exhaustive turn-loop behavior tests. |
| `src/agent/mod.rs` | 143 | 2 | Reexports the agent surface and defines `TokenSink` and `ApprovalHandler`. |
| `src/agent/queue.rs` | 321 | 1 | Processes one queued message by role and contains two queue-drain loops. |
| `src/agent/queue/tests.rs` | 0 | 493 | Tests queue processing status transitions and denial/completion semantics. |
| `src/agent/shell_execute.rs` | 150 | 443 | Shared guarded shell execution path that sits between turn guards and the shell tool. |
| `src/agent/spawn.rs` | 228 | 1 | Reconstructs a spawned child runtime, drains the child queue, and applies T2 plan handoff. |
| `src/agent/spawn/tests.rs` | 0 | 1537 | Tests spawned-child drain, metadata, and T2 handoff behavior. |
| `src/agent/tests.rs` | 0 | 2 | Declares agent test modules. |
| `src/agent/tests/common.rs` | 0 | 447 | Shared fake providers, fake tools, and helper fixtures for agent tests. |
| `src/agent/tests/regression_tests.rs` | 0 | 248 | Holds targeted regressions around approvals, budget enforcement, and fresh turn builders. |

### `src/gate/`

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/gate/budget.rs` | 75 | 124 | Denies turns that exceed configured turn/session/day token budgets. |
| `src/gate/exfil_detector.rs` | 159 | 152 | Detects read-then-send exfiltration sequences across tool-call batches. |
| `src/gate/mod.rs` | 135 | 189 | Defines guard traits, verdict/context types, reexports guards, and applies guard checks to outbound text/tool arguments. |
| `src/gate/output_cap.rs` | 72 | 143 | Spills oversized tool output to a session artifact file and returns a bounded pointer message. |
| `src/gate/secret_patterns.rs` | 1599 | 549 | Stores secret regex/prefix catalogs and also implements protected-path detection plus command heuristics for reads/writes to protected paths. |
| `src/gate/secret_redactor.rs` | 105 | 178 | Redacts secrets in inbound/outbound text using the static secret catalog. |
| `src/gate/shell_safety.rs` | 256 | 626 | Applies shell allow/approve/deny policy, standing approvals, and protected-path/identity-template write checks. |
| `src/gate/streaming_redact.rs` | 248 | 98 | Performs incremental secret redaction while the model is streaming text. |

### `src/llm/`

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/llm/mod.rs` | 215 | 0 | Defines chat/tool/message/result types and the provider trait. |
| `src/llm/openai.rs` | 608 | 643 | Builds OpenAI Responses requests, parses SSE frames, accumulates streamed state, and returns `StreamedTurn`. |

### `src/plan/`

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/plan/executor.rs` | 22 | 62 | Thin adapter from the plan runner into guarded shell execution. |
| `src/plan/notify.rs` | 209 | 178 | Builds failure payloads and atomically moves plan runs to `waiting_t2` while enqueueing the T2 notification. |
| `src/plan/patch.rs` | 343 | 355 | Creates plan runs, merges updated T2 plan actions, and enqueues escalations/terminal actions. |
| `src/plan/recovery.rs` | 132 | 314 | Recovers crashed/stale plan runs and attempts and converts them into failure payloads for T2. |
| `src/plan/runner.rs` | 1064 | 939 | Claims runnable plan runs, executes shell/spawn steps, evaluates checks, builds payloads, updates plan status, and releases claims. |

### `src/server/`

| Path | Prod | Test | What it actually does today |
| --- | ---: | ---: | --- |
| `src/server/auth.rs` | 89 | 135 | Authenticates API keys for HTTP and WebSocket requests and maps them to principals. |
| `src/server/http.rs` | 161 | 366 | Defines HTTP request/response DTOs, `HttpError`, and the health/session/enqueue handlers. |
| `src/server/mod.rs` | 113 | 0 | Creates `ServerState`, performs stale recovery, builds the router, and starts the Axum server. |
| `src/server/queue.rs` | 367 | 939 | Owns per-session lock leasing, server-side queue drain orchestration, and HTTP background worker bootstrapping. |
| `src/server/ws.rs` | 339 | 53 | Owns WebSocket frames, prompt routing, token sink, approval handler, and the WS session loop. |

## 2. Problems

- **`src/store.rs` is the largest SRP violation in the crate.**
  - Session registry logic: `src/store.rs:173-295`
  - Message queue logic: `src/store.rs:297-400`
  - Plan-run persistence: `src/store.rs:402-956` and `src/store.rs:1250-1295`
  - Step-attempt persistence: `src/store.rs:958-1248` and `src/store.rs:1297-1331`
  - Subscription registry: `src/store.rs:1333-1580`
  - Schema bootstrap/migration: `src/store.rs:103-170` and `src/store.rs:1590-1862`
  - Direct duplication: the dynamic SQL builders in `src/store.rs:473-566` and `src/store.rs:832-919`
  - Direct duplication: the pending-plan claim query in `src/store.rs:575-624` and `src/store.rs:1250-1294`

- **`src/config.rs` mixes runtime state, file schema, defaults, validation, active-agent selection, domain extension, and child-runtime retargeting.**
  - Initial active-agent/domain/tier resolution is in `src/config.rs:241-324`
  - Spawned-child retargeting repeats the same shape in `src/config.rs:382-489`
  - Policy type defaults/validation live in the same file as parsing and runtime assembly (`src/config.rs:1609-1710`)

- **`src/context.rs` is five modules wearing one name.**
  - Identity prompt injection: `src/context.rs:21-95`
  - Skill summary context: `src/context.rs:97-180`
  - Subscription materialization: `src/context.rs:183-362`
  - History replay into context: `src/context.rs:364-554`
  - The name `context::Identity` also conflicts with the actual identity loader in `src/identity.rs`

- **History grouping and token estimation are duplicated across `context.rs` and `session.rs`.**
  - Token estimation: `src/context.rs:383-400` vs `src/session.rs:259-279`
  - Assistant/tool round-trip grouping: `src/context.rs:421-499` vs `src/session.rs:379-490`
  - This is a real drift risk because both modules must preserve the same “never split assistant/tool round-trips” invariant.

- **`src/turn.rs` mixes runtime behavior with construction policy.**
  - Runtime/guard behavior: `src/turn.rs:18-232`
  - Tier resolution and build policy: `src/turn.rs:234-435`
  - It is also the main coupling knot between config, context, skills, subscription, tools, and guards.

- **Queue drain logic is duplicated in four places.**
  - Static-turn drain vs fresh-turn drain in `src/agent/queue.rs:139-216` and `src/agent/queue.rs:219-314`
  - Server copies the same loop with async lock choreography in `src/server/queue.rs:43-140` and `src/server/queue.rs:147-270`
  - Subscription load + turn builder + provider factory are copied in:
    - `src/main.rs:630-654`
    - `src/main.rs:667-691`
    - `src/server/ws.rs:126-176`
    - `src/server/queue.rs:276-325`

- **`src/main.rs` is a binary god-object.**
  - Clap types, tracing, auth commands, plan commands, subscription commands, server launch, REPL, and one-shot prompt execution are all in one file.
  - `src/cli.rs` is misnamed because it is not the CLI parser; it is terminal I/O.

- **`src/server/queue.rs` owns three unrelated responsibilities.**
  - Server-side per-session locking: `src/server/queue.rs:12-40`
  - Session drain orchestration: `src/server/queue.rs:43-270`
  - HTTP background worker bootstrap: `src/server/queue.rs:274-358`

- **`src/agent/loop_impl.rs` mixes the turn runner with persistence side effects and budget policy.**
  - Audit and denial persistence helpers: `src/agent/loop_impl.rs:41-91`
  - Token charging and post-turn budget denial: `src/agent/loop_impl.rs:133-199`
  - Inbound persistence and delegation hints: `src/agent/loop_impl.rs:201-232`
  - Core runner: `src/agent/loop_impl.rs:234-595`

- **`src/plan.rs` and `src/plan/executor.rs` have poor responsibility boundaries.**
  - `src/plan.rs` mixes spec types with parsing and validation.
  - `src/plan/executor.rs` is only a wrapper around `agent::shell_execute`; the name implies much more than it does.

- **`src/plan/runner.rs` is the second major orchestration god-object.**
  - Shared checks/parsing/payload code: `src/plan/runner.rs:136-499`
  - Shell-step lifecycle branch: `src/plan/runner.rs:562-764`
  - Spawn-step lifecycle branch: `src/plan/runner.rs:766-989`
  - The shell and spawn branches repeat: attempt creation, initial summary serialization, failure conversion, plan advancement, and attempt finalization.

- **`src/gate/secret_patterns.rs` is overloaded and misnamed.**
  - Secret catalog types/constants live beside path protection and command-analysis heuristics.
  - It is imported by redaction, streaming redaction, shell safety, exfil detection, the read tool, and turn tests.
  - A change to “secret patterns” can unintentionally change filesystem policy behavior.

- **Naming problems hide responsibilities.**
  - `src/spawn.rs` and `src/agent/spawn.rs` do different jobs but share the same module name.
  - `src/cli.rs` is terminal UI; the actual CLI is in `src/main.rs`.
  - `src/context.rs::Identity` is a context source, not the identity loader.
  - `src/util.rs` is not “misc”; it contains logging and time utilities.

- **Small but real shared helpers are duplicated instead of centralized.**
  - Denial formatting is duplicated in `src/agent/loop_impl.rs:120-122` and `src/cli.rs:117-119`
  - UTC formatting logic is duplicated between `src/util.rs:55-92` and `src/store.rs:1936-2015`

- **Large inline test blocks still dominate several production files.**
  - Biggest offenders: `src/store.rs`, `src/config.rs`, `src/context.rs`, `src/session.rs`, `src/turn.rs`, `src/plan/runner.rs`
  - The agent area already follows the better pattern: separate test modules/files next to the production module.

## 3. Target Structure

### Proposed Tree

```text
src/
  agent/
    mod.rs
    audit.rs
    child_drain.rs
    message_processor.rs
    shell_execute.rs
    turn_runner.rs
    usage.rs
    tests/
  app/
    args.rs
    plan_commands.rs
    session_run.rs
    subscription_commands.rs
    tracing.rs
  child_session/
    completion.rs
    create.rs
    mod.rs
  config/
    agents.rs
    domains.rs
    file_schema.rs
    load.rs
    mod.rs
    models.rs
    policy.rs
    runtime.rs
    spawn_runtime.rs
    tests.rs
  context/
    history.rs
    identity_prompt.rs
    mod.rs
    skill_instructions.rs
    skill_summaries.rs
    subscriptions.rs
    tests.rs
  gate/
    budget.rs
    command_path_analysis.rs
    exfil_detector.rs
    mod.rs
    output_cap.rs
    protected_paths.rs
    secret_catalog.rs
    secret_redactor.rs
    shell_safety.rs
    streaming_redact.rs
  llm/
    history_groups.rs
    mod.rs
    openai/
      mod.rs
      request.rs
      sse.rs
  plan/
    mod.rs
    notify.rs
    parse.rs
    patch.rs
    recovery.rs
    shell_execute.rs
    spec.rs
    runner/
      checks.rs
      mod.rs
      payloads.rs
      shell_step.rs
      spawn_step.rs
      tick.rs
      tests.rs
  server/
    auth.rs
    http.rs
    mod.rs
    queue_worker.rs
    session_lock.rs
    state.rs
    ws.rs
  session/
    budget.rs
    delegation_hint.rs
    jsonl.rs
    mod.rs
    trimming.rs
    tests.rs
  session_runtime/
    drain.rs
    factory.rs
    mod.rs
  store/
    message_queue.rs
    migrations.rs
    mod.rs
    plan_runs.rs
    sessions.rs
    step_attempts.rs
    subscriptions.rs
  auth.rs
  delegation.rs
  identity.rs
  lib.rs
  logging.rs
  main.rs
  model_selection.rs
  principal.rs
  read_tool.rs
  skills.rs
  subscription.rs
  template.rs
  terminal_ui.rs
  time.rs
  tool.rs
  turn/
    builders.rs
    mod.rs
    tiers.rs
    tests.rs
    verdicts.rs
```

### New/Changed Modules

| Path | Responsibility | What moves in from where |
| --- | --- | --- |
| `src/main.rs` | Thin Tokio entrypoint that only calls the app runner. | Keeps the top-level `main()` wrapper from current `src/main.rs`; everything else moves out. |
| `src/app/args.rs` | Owns clap types and argument parsing. | `Cli`, `Commands`, and all arg structs from `src/main.rs:21-177`. |
| `src/app/tracing.rs` | Owns tracing subscriber construction and startup. | `build_tracing_subscriber*` and `init_tracing()` from `src/main.rs:190-261`. |
| `src/app/plan_commands.rs` | Owns plan CLI helpers and handlers. | `plan_run_retries`, `resolve_plan_run_for_status`, `format_plan_run_summary`, and `handle_plan_command()` from `src/main.rs:300-420`. |
| `src/app/subscription_commands.rs` | Owns subscription CLI helpers and handlers. | `default_subscription_topic`, `subscription_filter`, rendering helpers, and `handle_subscription_command()` from `src/main.rs:184-299` and `src/main.rs:422-500`. |
| `src/app/session_run.rs` | Runs the interactive REPL and one-shot CLI session path. | The no-subcommand branch from `src/main.rs:560-700`, after it starts using `session_runtime::factory`. |
| `src/terminal_ui.rs` | Terminal token output, approval prompting, and denial rendering. | Rename/move current `src/cli.rs`. |
| `src/logging.rs` | Tracing formatter and user-output tracing targets. | `PlainMessageFormatter`, `STDOUT_USER_OUTPUT_TARGET`, and `STDERR_USER_OUTPUT_TARGET` from `src/util.rs`. |
| `src/time.rs` | UTC timestamp formatting helpers shared by store/session/plan. | `utc_timestamp()` from `src/util.rs` and `format_system_time()` from `src/store.rs`. |
| `src/config/mod.rs` | Public config facade and reexports. | Public API surface from current `src/config.rs`. |
| `src/config/runtime.rs` | Defines runtime `Config` and lightweight accessors. | `Config`, `active_agent_definition()`, and `active_t1_config()` from `src/config.rs`. |
| `src/config/load.rs` | Loads file config into runtime config and applies env overrides. | `load()`, `load_typed()`, `from_file*()`, default runtime assembly, skills-dir resolution, and env override logic from `src/config.rs:158-356`. |
| `src/config/spawn_runtime.rs` | Retargets runtime config for spawned child sessions. | `with_spawned_child_runtime*()` from `src/config.rs:359-490`. |
| `src/config/agents.rs` | Defines agent/tier config and active-agent selection rules. | `AgentsConfig`, `AgentDefinition`, `AgentTierConfig`, `select_active_agent()`, and `validate_agent_identity()` from `src/config.rs:1256-1384`. |
| `src/config/models.rs` | Defines model catalog and route config types. | `ModelsConfig`, `ModelDefinition`, `ModelRoute` from `src/config.rs:1303-1335`. |
| `src/config/domains.rs` | Defines domain packs and validates domain prompt extensions. | `DomainsConfig`, `DomainConfig`, and `validate_domain_context_extend()` from `src/config.rs:1337-1404`. |
| `src/config/policy.rs` | Defines shell/read/subscription/queue/budget policy types and validation. | `BudgetConfig`, `QueueConfig`, `ReadToolConfig`, `SubscriptionsConfig`, `ShellPolicy`, defaults, and validators from `src/config.rs`. |
| `src/config/file_schema.rs` | Defines the raw TOML file schema. | `RuntimeFileConfig` and `AuthFileSection` from `src/config.rs:1233-1254` and `src/config.rs:1350-1353`. |
| `src/store/mod.rs` | Keeps `Store` as the facade and owns shared transaction/reexport wiring. | `Store`, `with_transaction()`, and public reexports from current `src/store.rs`. |
| `src/store/migrations.rs` | Initializes and migrates SQLite schema. | `Store::new()` schema bootstrap plus `ensure_*`, `cleanup_legacy_plan_rows()`, and related helpers from `src/store.rs:103-170` and `src/store.rs:1590-1862`. |
| `src/store/sessions.rs` | Owns session and child-session persistence. | `create_session()`, `create_child_session*()`, `list_sessions()`, `get_parent_session()`, `get_session_metadata()`, and `list_child_sessions()` from `src/store.rs:173-295`. |
| `src/store/message_queue.rs` | Owns queued-message persistence and stale-message recovery. | `QueuedMessage`, `enqueue_message*()`, `dequeue_next_message()`, `mark_processed()`, `mark_failed()`, and `recover_stale_messages()` from `src/store.rs:297-400`. |
| `src/store/plan_runs.rs` | Owns plan-run persistence and status transitions. | `PlanRun`, `NullableUpdate`, `PlanRunUpdateFields`, `create/get/update/claim/release/list/recover/resume/cancel` from `src/store.rs:402-956` and `src/store.rs:1250-1295`. |
| `src/store/step_attempts.rs` | Owns plan step-attempt persistence and recovery. | `StepAttempt`, `StepAttemptRecord`, `next/max attempt`, `record/update/finalize/crash/get` from `src/store.rs:958-1248` and `src/store.rs:1297-1331`. |
| `src/store/subscriptions.rs` | Owns subscription persistence and session-visible merge behavior. | `SubscriptionRow`, `create/delete/list/list_for_session/refresh` from `src/store.rs:1333-1580`. |
| `src/llm/history_groups.rs` | Centralizes `ChatMessage` token estimation and assistant/tool round-trip grouping. | Shared logic now duplicated in `src/context.rs:383-499` and `src/session.rs:259-490`. |
| `src/context/mod.rs` | Public context-provider facade and reexports. | `ContextSource` trait and reexports from current `src/context.rs`. |
| `src/context/identity_prompt.rs` | Loads and injects the rendered identity prompt. | `Identity` and `inject_identity_prompt()` from `src/context.rs:21-95`. |
| `src/context/skill_summaries.rs` | Adds the lightweight skill catalog summary to a turn. | `SkillContext` from `src/context.rs:97-180`. |
| `src/context/skill_instructions.rs` | Adds full skill instructions for spawned T3 turns. | `SkillLoader` from `src/context.rs:108-147`. |
| `src/context/subscriptions.rs` | Materializes session subscriptions into bounded system messages. | `SubscriptionContext` from `src/context.rs:183-362`. |
| `src/context/history.rs` | Adds bounded session history to a turn using shared history grouping. | `History` from `src/context.rs:364-554`, after it starts using `llm::history_groups`. |
| `src/session/mod.rs` | Public session facade and reexports. | `Session` type and public surface from current `src/session.rs`. |
| `src/session/jsonl.rs` | Reads/writes JSONL session entries and converts them to/from `ChatMessage`. | `SessionEntry`, `to_entry()`, `message_from_entry()`, `append_entry_to_file()`, file replay helpers, and `today_path()` from `src/session.rs`. |
| `src/session/trimming.rs` | Enforces max-context trimming without splitting assistant/tool round-trips. | `estimate_message_tokens()`, trim helpers, and `ensure_context_within_limit()` from `src/session.rs`, rewritten to use `llm::history_groups`. |
| `src/session/budget.rs` | Computes turn/session/day token totals for budget guards. | `latest_turn_tokens()`, `today_token_total()`, `budget_snapshot()`, and token-total accessors from `src/session.rs`. |
| `src/session/delegation_hint.rs` | Persists and clears queued delegation hints. | `.delegation_hint` helpers from `src/session.rs:560-613`. |
| `src/turn/mod.rs` | Owns the `Turn` runtime object and public guard/tool/context methods. | `Turn` and runtime methods from `src/turn.rs:17-182`. |
| `src/turn/verdicts.rs` | Owns guard precedence resolution. | `resolve_verdict()` from `src/turn.rs:184-232`. |
| `src/turn/tiers.rs` | Resolves T1/T2/T3 from config. | `TurnTier` and `resolve_tier()` from `src/turn.rs:234-249`. |
| `src/turn/builders.rs` | Builds configured T1/T2/T3 turns and shared turn-construction helpers. | `identity_vars_for_turn()`, `add_budget_guard()`, `build_turn_with_tool()`, and the public `build_*turn` functions from `src/turn.rs:251-435`. |
| `src/agent/turn_runner.rs` | Runs one agent turn from inbound prompt to final verdict. | `run_agent_loop()` from `src/agent/loop_impl.rs`. |
| `src/agent/audit.rs` | Owns denial/audit persistence and the shared denial message formatter. | `append_audit_note()`, `persist_denied_assistant_text()`, `append_*deny*()` helpers, and `format_denial_message()` from `src/agent/loop_impl.rs`. |
| `src/agent/usage.rs` | Owns token charging and post-turn budget enforcement. | `token_total()`, `charged_turn_meta()`, and `post_turn_budget_denial()` from `src/agent/loop_impl.rs`. |
| `src/agent/message_processor.rs` | Processes one queued message by role, with or without a fresh turn builder. | `process_queued_message*()` from `src/agent/queue.rs:15-120`. |
| `src/agent/child_drain.rs` | Drains spawned child sessions and performs T2 plan handoff. | Rename/move `src/agent/spawn.rs`. |
| `src/session_runtime/mod.rs` | Shared facade for single-session runtime helpers. | New shared module. |
| `src/session_runtime/drain.rs` | Drains one session queue using shared logic for CLI, HTTP worker, and WebSocket. | The duplicated drain loops from `src/agent/queue.rs:139-314` and `src/server/queue.rs:43-270`. |
| `src/session_runtime/factory.rs` | Loads subscriptions and constructs a turn builder plus provider factory for a session. | The repeated setup code from `src/main.rs:630-691`, `src/server/ws.rs:126-176`, and `src/server/queue.rs:276-325`. |
| `src/child_session/mod.rs` | Public child-session spawn facade and type reexports. | Public spawn surface now split out of `src/spawn.rs`. |
| `src/child_session/create.rs` | Creates child sessions and resolves child tier/model/skills. | `SpawnRequest`, `SpawnResult`, child metadata types, budget/model/tier/skill validation, and `spawn_child()` from `src/spawn.rs:17-235`. |
| `src/child_session/completion.rs` | Enqueues parent completion messages and extracts the latest assistant response. | `enqueue_child_completion()`, `should_enqueue_child_completion()`, `build_completion_message()`, and `latest_assistant_response()` from `src/spawn.rs:237-297`. |
| `src/server/state.rs` | Defines server state and session-id helpers. | `ServerState`, `generate_session_id()`, and `validate_session_id()` from `src/server/mod.rs`. |
| `src/server/session_lock.rs` | Owns per-session lock creation and lease cleanup. | `ServerState::session_lock()` and `SessionLockLease` from `src/server/queue.rs:12-40`. |
| `src/server/queue_worker.rs` | Starts HTTP queue workers and adapts server state to shared session runtime drain. | `spawn_http_queue_worker()` and server-specific no-op sinks from `src/server/queue.rs:274-358`. |
| `src/server/mod.rs` | Keeps only server startup, stale recovery, router wiring, and reexports. | `run()` and `router()` from current `src/server/mod.rs`, after `ServerState` and lock logic move out. |
| `src/plan/spec.rs` | Owns plan action/spec types only. | `PlanAction`, `PlanActionKind`, `PlanStepSpec`, `SpawnStepSpec`, `ShellCheckSpec`, `ShellExpectation` from `src/plan.rs:18-78`. |
| `src/plan/parse.rs` | Owns fenced `plan-json` extraction and validation. | `extract_plan_action()`, `extract_plan_json_block()`, and validation helpers from `src/plan.rs:79-194`. |
| `src/plan/shell_execute.rs` | Plan-specific adapter over guarded shell execution. | Rename/move current `src/plan/executor.rs`. |
| `src/plan/runner/checks.rs` | Parses shell output and evaluates shell checks. | `artifact_path()`, `parse_shell_output_text()`, `evaluate_check()`, `run_check()`, and `run_checks()` from `src/plan/runner.rs:130-351`. |
| `src/plan/runner/payloads.rs` | Builds summary/crash/failure payloads and serialization helpers. | `serialize_json()`, `build_step_summary()`, `build_step_crash()`, `build_waiting_t2_failure_details()`, and related helpers from `src/plan/runner.rs:353-499`. |
| `src/plan/runner/shell_step.rs` | Executes shell steps and finalizes their attempts. | The shell branch from `src/plan/runner.rs:562-764`. |
| `src/plan/runner/spawn_step.rs` | Executes spawn steps and finalizes their attempts. | The spawn branch from `src/plan/runner.rs:766-989`. |
| `src/plan/runner/tick.rs` | Claims runnable plan runs, delegates to `run_plan_step`, and releases claims. | `tick_plan_runner()` from `src/plan/runner.rs:993-1068`. |
| `src/plan/runner/mod.rs` | Owns shared runner types and `run_plan_step()`. | `StepOutcome`, check outcome types, `run_plan_step()`, and high-level shared helpers from `src/plan/runner.rs`. |
| `src/gate/secret_catalog.rs` | Secret prefixes, regexes, and streaming-redaction metadata only. | `SecretPattern`, `SecretBodyKind`, `SecretSuffixLen`, and `SECRET_PATTERNS` from the top of `src/gate/secret_patterns.rs`. |
| `src/gate/protected_paths.rs` | Protected-path catalogs and normalized path checks. | `protected_path_fragments()`, `path_is_protected()`, and related protected-path helpers from `src/gate/secret_patterns.rs`. |
| `src/gate/command_path_analysis.rs` | Shell command heuristics for reads/writes to protected or target paths. | `simple_command_reads_*()`, `command_writes_*()`, and supporting argv/script helpers from `src/gate/secret_patterns.rs`. |
| `src/llm/openai/request.rs` | Shapes internal messages/tools into OpenAI Responses API request payloads. | `build_input()` and `build_tools()` from `src/llm/openai.rs:60-154`. |
| `src/llm/openai/sse.rs` | Parses SSE lines and maintains incremental stream state. | `SseEvent`, `parse_sse_line()`, state structs, and `apply_sse_event()` from `src/llm/openai.rs:157-477`. |
| `src/llm/openai/mod.rs` | Owns provider construction and network transport only. | `OpenAIProvider` construction and `stream_completion()` transport loop from current `src/llm/openai.rs`, after request/SSE logic move out. |

### Modules That Should Stay In Place

These modules already have a reasonably tight responsibility and should stay as single files, only with import updates: `src/auth.rs`, `src/delegation.rs`, `src/identity.rs`, `src/lib.rs`, `src/model_selection.rs`, `src/principal.rs`, `src/read_tool.rs`, `src/skills.rs`, `src/subscription.rs`, `src/template.rs`, `src/tool.rs`, `src/server/auth.rs`, `src/server/http.rs`, `src/server/ws.rs`, `src/gate/budget.rs`, `src/gate/exfil_detector.rs`, `src/gate/output_cap.rs`, `src/gate/secret_redactor.rs`, `src/gate/shell_safety.rs`, and `src/gate/streaming_redact.rs`.

### Test Layout

For every large module above, move inline tests into co-located sibling modules:

- `src/config/tests.rs`
- `src/context/tests.rs`
- `src/session/tests.rs`
- `src/turn/tests.rs`
- `src/plan/runner/tests.rs`
- `src/store/*` sibling `tests.rs` files per submodule
- `src/agent/*` test files retained in the existing co-located style

That keeps the “junior readability” rule intact without changing behavior.

## 4. Migration Order

Each step below is intended to keep the crate green after `cargo build --release`, `cargo test`, `cargo fmt --check`, and `cargo clippy -- -D warnings`.

1. **Extract shared pure helpers first.**
   - Move UTC formatting into `src/time.rs`.
   - Move denial-message formatting into `src/agent/audit.rs`.
   - Extract `src/llm/history_groups.rs` from the duplicated context/session logic.
   - Safe independently: yes.
   - Depends on: nothing.

2. **Split `src/store.rs` behind the same `store::Store` API.**
   - Create `store/mod.rs`, `migrations.rs`, `sessions.rs`, `message_queue.rs`, `plan_runs.rs`, `step_attempts.rs`, and `subscriptions.rs`.
   - Keep callers unchanged by reexporting all current row types and methods from `store/mod.rs`.
   - Safe independently: yes, if the public API stays stable.
   - Depends on: step 1 only for `time.rs`.

3. **Split `src/config.rs` behind the same `config::Config` API.**
   - Create `config/mod.rs`, `runtime.rs`, `load.rs`, `spawn_runtime.rs`, `agents.rs`, `models.rs`, `domains.rs`, `policy.rs`, and `file_schema.rs`.
   - Preserve the existing constructors/accessors so callers do not change yet.
   - Safe independently: yes.
   - Depends on: nothing after step 1.

4. **Split `src/context.rs`, `src/session.rs`, and `src/turn.rs`.**
   - Move history logic to `llm::history_groups`.
   - Turn `context.rs`, `session.rs`, and `turn.rs` into directory modules with `tests.rs`.
   - Safe independently: yes, once steps 1 and 3 exist.
   - Depends on: steps 1 and 3.

5. **Split the agent runtime internals.**
   - Move `run_agent_loop()` into `agent/turn_runner.rs`.
   - Move audit and token/budget helpers into `agent/audit.rs` and `agent/usage.rs`.
   - Move per-message role handling into `agent/message_processor.rs`.
   - Safe independently: yes.
   - Depends on: step 4.

6. **Extract shared single-session runtime helpers.**
   - Create `session_runtime/drain.rs` from the duplicated queue-drain loops.
   - Create `session_runtime/factory.rs` for subscription loading, turn building, and provider factory construction.
   - Update `agent/queue.rs` and `server/queue.rs` to delegate to the shared runtime.
   - Safe independently: yes, after the agent split.
   - Depends on: steps 3, 4, and 5.

7. **Rename the two different “spawn” modules into one child-session area plus one agent drain area.**
   - `src/spawn.rs` becomes `src/child_session/{create,completion}.rs`.
   - `src/agent/spawn.rs` becomes `src/agent/child_drain.rs`.
   - Keep temporary reexports so other modules compile while imports are updated.
   - Safe independently: yes.
   - Depends on: steps 2, 3, and 6.

8. **Split the plan domain.**
   - `src/plan.rs` becomes `plan/spec.rs` and `plan/parse.rs`.
   - `src/plan/executor.rs` becomes `plan/shell_execute.rs`.
   - `src/plan/runner.rs` becomes `plan/runner/{mod,checks,payloads,shell_step,spawn_step,tick}.rs`.
   - Safe independently: yes, if `plan/mod.rs` preserves reexports.
   - Depends on: steps 2, 3, and 7.

9. **Shrink the entrypoints and server orchestration.**
   - Split `src/main.rs` into `app/{args,tracing,plan_commands,subscription_commands,session_run}.rs`.
   - Rename `src/cli.rs` to `src/terminal_ui.rs`.
   - Split `src/server/mod.rs` into `state.rs`, `session_lock.rs`, and `queue_worker.rs`.
   - Safe independently: yes.
   - Depends on: steps 6 and 8.

10. **Finish the remaining overloaded internals.**
    - Split `src/gate/secret_patterns.rs` into `secret_catalog.rs`, `protected_paths.rs`, and `command_path_analysis.rs`.
    - Split `src/llm/openai.rs` into `openai/{mod,request,sse}.rs`.
    - Move any remaining large inline tests into sibling `tests.rs` files.
    - Safe independently: yes, but do it last because these paths are behavior-sensitive.
    - Depends on: steps 4 and 8.

## 5. Risks

- **SQLite transaction boundaries can break silently during the store split.**
  - `create_child_session_with_task()`, `notify_plan_failure()`, stale-plan recovery, and step-attempt crash/finalize paths must stay atomic.

- **The plan runner has race-sensitive semantics.**
  - `update_plan_run_status_preserving_failed()` and `release_plan_run_claim()` must preserve the current “failed wins” and “claimed row always ends terminal or released” behavior.

- **The assistant/tool round-trip invariant is easy to regress.**
  - The shared `llm::history_groups` extraction must preserve both session trimming and context replay behavior exactly.

- **Budget semantics are split across multiple modules.**
  - Same-turn budget denial in the agent loop and pre-spawn parent-budget checks must stay unchanged after the agent/session/child-session refactor.

- **Guard ordering must remain identical.**
  - `deny > approve > allow/modify` precedence in `Turn` cannot change, and the same guard set must still be applied to inbound messages, tool calls, and streamed text.

- **Server lock choreography is subtle.**
  - The per-session mutex must still serialize one session’s queue drain, and the store mutex must still not be held across network/model streaming.

- **The child-session rename touches multiple boundaries at once.**
  - Stored metadata format, latest-assistant-response extraction, and T2 plan handoff all depend on both the parent and child runtime paths.

- **`src/gate/secret_patterns.rs` is security-sensitive.**
  - Splitting it must not widen protected-path access or weaken identity-template write detection for shell commands and `read_file`.

- **`src/llm/openai.rs` is protocol-sensitive.**
  - The SSE parser must keep handling chunk boundaries, terminal events, and tool-call assembly exactly as it does now.

- **Test relocation can break visibility even if behavior is unchanged.**
  - Large inline tests currently rely on private helpers in the same file; moving them to sibling modules must preserve module visibility and fixture reuse.
