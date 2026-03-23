1. Files read

- `docs/current/risks.md`
- `docs/current/architecture.md`
- `docs/roadmap.md`
- `docs/vision.md`
- `Cargo.toml`
- `agents.toml`
- `src/agent.rs`
- `src/auth.rs`
- `src/cli.rs`
- `src/config.rs`
- `src/context.rs`
- `src/gate/{budget.rs, exfil_detector.rs, mod.rs, output_cap.rs, secret_patterns.rs, secret_redactor.rs, shell_safety.rs, streaming_redact.rs}`
- `src/identity.rs`
- `src/lib.rs`
- `src/llm/{mod.rs, openai.rs}`
- `src/main.rs`
- `src/principal.rs`
- `src/server.rs`
- `src/session.rs`
- `src/store.rs`
- `src/template.rs`
- `src/tool.rs`
- `src/turn.rs`
- `src/util.rs`

2. Exact changes per file

- `Cargo.toml`: add one Rust jq-evaluation dependency so `--jq` is implemented in-process; reuse existing `serde_json`, `regex`, and `tiktoken-rs` for the other filter types and utilization accounting.
- `src/lib.rs`: export a new `subscription` module.
- `src/store.rs`: extend `Store::new()` schema init with a `subscriptions` table in `sessions/queue.sqlite` and indexes; columns: `id INTEGER PRIMARY KEY AUTOINCREMENT`, `path TEXT NOT NULL`, `topic TEXT NOT NULL DEFAULT '_default'`, `filter TEXT NULL`, `activated_at TEXT NOT NULL`, `updated_at TEXT NOT NULL`, plus a unique index on `(topic, path)`; add CRUD/query helpers for insert, delete-by-path/topic, list with optional topic filter, and point updates to `updated_at`.
- `src/subscription.rs` (new): add the V1 subscription domain layer: `SubscriptionFilter` enum (`Full`, `Lines`, `Regex`, `Head`, `Tail`, `Jq`) serialized into the store `filter` column, `SubscriptionRecord`, `SubscriptionContentBlock`, path normalization to absolute lexical paths, file mtime -> fixed-width UTC timestamp formatting with subsecond precision, filter parsing/validation, content loading, filter application, effective timestamp calculation as `max(activated_at, updated_at)`, list formatting, and token utilization estimation over rendered content blocks.
- `src/main.rs`: add `sub` CLI surface with `add/remove/list`; `sub add` defaults topic to `_default` and accepts exactly one of `--lines/--regex/--head/--tail/--jq`; `sub remove` normalizes the incoming path and deletes only the matching `(topic, path)` row; `sub list` shows all rows unless `--topic` is supplied and prints topic/path/filter/effective timestamp; `add/remove` both print total current subscription token utilization after the mutation.
- `src/main.rs`: keep `auth`, `serve`, queue flow, and REPL path untouched; the new `sub` branch should open the existing SQLite store directly and not initialize agent/session/provider machinery.
- `src/config.rs`, `agents.toml`, `src/session.rs`, `src/context.rs`, `src/turn.rs`, `src/agent.rs`, and `src/server.rs`: no functional changes in 2a; explicitly leave context assembly and topic activation out of scope until 2b/2c.

3. What tests to write

- `src/store.rs`: schema creation is idempotent and preserves the existing queue tables while adding `subscriptions`.
- `src/store.rs`: inserting a subscription with no topic stores `_default`; duplicate `(topic, path)` inserts fail cleanly.
- `src/store.rs`: remove only deletes the matching `(topic, path)` row and does not touch the same path in a different topic.
- `src/store.rs`: list returns all subscriptions when no topic is supplied and only matching rows when a topic filter is supplied.
- `src/subscription.rs`: path normalization turns relative paths into absolute lexical paths without collapsing symlink intent.
- `src/subscription.rs`: each filter works exactly once its input is valid: `lines` is 1-based inclusive `N-M`, `regex` returns matching lines in original order, `head/tail` return the first/last `N` lines, `jq` evaluates the expression against valid JSON and pretty-prints deterministic output, `full` returns the whole file.
- `src/subscription.rs`: invalid filter inputs are rejected: multiple filter flags, malformed `N-M`, `N < 1`, `M < N`, zero `head/tail`, invalid regex, invalid jq, invalid JSON for jq, unreadable or non-UTF-8 file content.
- `src/subscription.rs`: refreshing a subscription after the file mtime changes moves `updated_at` forward and therefore changes `effective_at = max(activated_at, updated_at)`.
- `src/subscription.rs`: rendered content blocks and utilization use the same rendering path, so token counts match what 2b will later inject.
- `src/main.rs`: clap parsing accepts `sub add/remove/list`, defaults `topic` to `_default` for add/remove, and enforces exactly one filter flag.

4. Order of operations

- Add `src/subscription.rs` first with pure path/filter/content/utilization logic and unit tests; this keeps the riskiest parsing behavior isolated before touching SQLite or CLI dispatch.
- Extend `src/store.rs` next with the `subscriptions` table and CRUD/list/update helpers, plus store-level tests against temp databases; keep queue behavior unchanged.
- Wire `src/main.rs` after the domain/store layer is stable: parse `sub` commands, call the new store/subscription helpers, and print utilization after `add/remove`.
- Only after the CLI works, add list formatting polish and post-mutation utilization warnings for unreadable subscriptions; do not touch `Turn`, `ContextSource`, or session replay.
- Finish with full repo verification: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, and `cargo build --release`.

5. Risk assessment

- `--jq` is the highest implementation risk because native jq semantics are non-trivial; use an in-process dependency and keep tests on a narrow, deterministic subset we explicitly support in V1.
- Timestamp precision matters here more than in the current queue/session code; if `updated_at` is only second-precision, fast edits can fail to bubble forward, so subscription timestamps should use fixed-width fractional seconds.
- Path normalization must not silently canonicalize through symlinks, or remove/list semantics become surprising; normalize lexically to an absolute path and always look up/delete using that same rule.
- Post-mutation utilization can fail if an existing subscription points at a missing or unreadable file; the mutation should still succeed and the CLI should report utilization as unavailable rather than silently lying.
- 2a must stay orthogonal to the known queue/session bugs in `risks.md`; do not couple subscriptions to queue claiming, session append ordering, or context assembly yet.
