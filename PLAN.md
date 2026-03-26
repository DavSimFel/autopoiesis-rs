# Subscription Context Wiring Plan

## 1. Files Read

- Repo/config/docs: `Cargo.toml`, `agents.toml`, `docs/risks.md`, `docs/architecture/overview.md`, `docs/vision.md`
- `src/`: `auth.rs`, `cli.rs`, `config.rs`, `context.rs`, `delegation.rs`, `identity.rs`, `lib.rs`, `main.rs`, `model_selection.rs`, `plan.rs`, `principal.rs`, `read_tool.rs`, `session.rs`, `skills.rs`, `spawn.rs`, `store.rs`, `subscription.rs`, `template.rs`, `tool.rs`, `turn.rs`, `util.rs`
- `src/agent/`: `loop_impl.rs`, `loop_impl/tests.rs`, `mod.rs`, `queue.rs`, `queue/tests.rs`, `shell_execute.rs`, `spawn.rs`, `spawn/tests.rs`, `tests.rs`, `tests/common.rs`, `tests/regression_tests.rs`
- `src/gate/`: `budget.rs`, `exfil_detector.rs`, `mod.rs`, `output_cap.rs`, `secret_patterns.rs`, `secret_redactor.rs`, `shell_safety.rs`, `streaming_redact.rs`
- `src/llm/`: `mod.rs`, `openai.rs`
- `src/plan/`: `executor.rs`, `notify.rs`, `patch.rs`, `recovery.rs`, `runner.rs`
- `src/server/`: `auth.rs`, `http.rs`, `mod.rs`, `queue.rs`, `ws.rs`

## 2. Exact Changes Per File

- `src/store.rs`
  Add `session_id` to `SubscriptionRow` and migrate the `subscriptions` table with a new nullable `session_id` column plus an index that supports session-ordered context loading. Do not silently rehome legacy rows to any hardcoded session. New writes always set `session_id`; legacy rows remain `NULL` and are treated as explicit global-compatibility subscriptions during a transition window, meaning session-aware loaders return matching session rows plus `NULL` rows for every session, with session-specific rows winning on duplicate `(path, filter)` keys. Keep `topic` for compatibility metadata, but stop treating it as the selector for context injection. Add session-aware store APIs such as `create_subscription_for_session`, `delete_subscription_for_session`, `list_subscriptions_for_session`, and a loader that returns rows for one session ordered by effective timestamp. Keep timestamp refresh logic global so existing CLI summary behavior still works.

- `src/subscription.rs`
  Update row decoding for the extended store row shape. Add small helpers that the context layer can reuse instead of re-encoding policy inline: a stable filter label for provenance tags, and optionally a shared materialization helper that reads the file, applies the filter, and returns rendered text plus a best-effort mtime/effective timestamp. Do not add topic logic here.

- `src/context.rs`
  Add `SubscriptionContext`, a new `ContextSource` implementation. It should:
  take `Vec<SubscriptionRecord>` and a token budget,
  load each file, apply `SubscriptionFilter::render`,
  emit one `ChatMessage::system_with_principal(..., Some(Principal::System))` per materialized subscription,
  include provenance tags with path, filter, and mtime,
  sort subscriptions by `effective_at()` before emission,
  respect a local token budget by truncating the current subscription content when it is the last one that fits and skipping the rest,
  warn and skip on missing/unreadable files or filter/render failures,
  insert after the identity system message and before the preexisting history/user messages.
  Add context-level tests here because this is where message ordering, provenance, warnings, and truncation live.

- `src/turn.rs`
  Introduce a session-aware turn builder entry point that accepts preloaded subscriptions, for example `build_turn_for_config_with_subscriptions(config, subscriptions)`, and make the current `build_turn_for_config(config)` delegate to it with `[]` for compatibility. Wire `SubscriptionContext` into the T1 assembly path only, immediately after `Identity` and before any history/user messages are already present. Leave T2/T3 behavior unchanged. Extend turn tests to cover T1 injection, T2/T3 non-injection, provider-input visibility, and taint behavior from subscription system messages.

- `src/agent/queue.rs`
  Change the queue drain path so queued user turns do not share one prebuilt `Turn`. Introduce the turn-factory path in a way that preserves compilation while callers are being moved, for example by adding a parallel helper or compatibility wrapper first and only removing the old single-`&Turn` path after callers are updated. The end state is that `drain_queue`/`drain_queue_with_stats`/`process_queued_message` build a fresh turn for each queued user turn, while system/assistant queue items can continue to be stored without rebuilding. This is required so HTTP/server queue drains observe subscription changes between queued turns.

- `src/agent/queue/tests.rs`
  Update queue tests for the new turn-factory API and add a freshness regression test that proves two queued user turns in the same drain do not share stale subscription state.

- `src/config.rs`
  Add a new config section, e.g. `SubscriptionsConfig`, to `Config` and `RuntimeFileConfig`. Proposed shape:
  `context_token_budget: usize`
  with a default of `4096` and validation `> 0`. Thread it through `Config::load`, `Default`, test fixtures, and all config parsing tests that construct `Config` directly.

- `agents.toml`
  Add a `[subscriptions]` table with `context_token_budget = 4096`.

- `src/main.rs`
  Change subscription CLI commands to resolve the active `session_id` and call the new session-aware store APIs. Keep the existing `--topic` CLI surface as compatibility metadata only; do not make context loading depend on it. In CLI execution, stop building the turn once at process startup; instead, load current session subscriptions and build a fresh T1 turn for each user turn so newly added/updated subscriptions are reflected without restarting the process.

- `src/server/queue.rs`
  Stop passing one prebuilt `Turn` into the whole drain. Instead, provide the session-aware turn factory required by `src/agent/queue.rs`, so each queued user turn in an HTTP drain reloads current subscriptions before provider invocation.

- `src/server/ws.rs`
  Move turn construction into the per-prompt path so each WebSocket turn can reload current session subscriptions. If WebSocket draining continues to reuse the generic queue path, pass the same session-aware turn factory there rather than one prebuilt `Turn`.

- `src/agent/spawn.rs`
  When draining a spawned child, load subscriptions for the child session before constructing the turn. Only T1 children should actually include `SubscriptionContext`; T2/T3 stay unchanged because the task explicitly scopes context wiring to T1.

- `src/turn.rs` test fixtures and all direct `Config { ... }` literals
  Add `subscriptions: SubscriptionContextConfig::default()` everywhere a `Config` is manually constructed. Based on the current tree, that includes at least:
  `src/turn.rs`, `src/config.rs`, `src/spawn.rs`, `src/plan/runner.rs`, `src/agent/tests/common.rs`, `src/agent/spawn/tests.rs`, `src/server/http.rs`, `src/server/queue.rs`, `src/server/auth.rs`.

- `docs/risks.md`
  Remove the current structural-risk note that subscriptions are not injected into turn context once the implementation lands.

- `docs/architecture/overview.md`
  Update the Subscriptions section and turn-assembly description to state that T1 can materialize session-scoped file subscriptions into context with filter application and provenance tags.

- `docs/vision.md`
  Remove or revise “Subscriptions v2 and context wiring” from the “What Still Remains” list so docs stay in sync with `src/`.

## 3. Tests To Write

- `src/context.rs`: `subscription_context_with_no_subscriptions_adds_no_messages`
  Assert input message vector is unchanged.

- `src/context.rs`: `subscription_context_materializes_file_content`
  Create a temp file, build one `SubscriptionRecord`, assemble into a message list that already contains identity/history, and assert one new system message exists with the file content.

- `src/context.rs`: `subscription_context_applies_lines_regex_head_and_tail_filters`
  Table-drive four subscriptions against one temp file and assert the rendered bodies match the expected filtered slices. No new jq test is required here because `subscription.rs` already covers jq parsing/rendering, but the context path should still exercise the handoff.

- `src/context.rs`: `subscription_context_respects_token_budget_and_truncates_last_message`
  Use a large file, a very small budget, and assert:
  a truncation marker is present,
  total emitted subscription tokens stay within the configured limit,
  later subscriptions are skipped once the budget is exhausted.

- `src/context.rs`: `subscription_context_missing_file_warns_and_skips`
  Capture `tracing` warnings, assemble a missing subscription, assert no panic, no injected message, and a warning mentioning the path.

- `src/context.rs`: `subscription_context_skips_bad_subscription_and_keeps_later_valid_ones`
  Build one missing/unreadable subscription followed by one valid subscription, assert the warning is emitted, the bad one is skipped, and the valid subscription still materializes. This is the actual graceful-degradation path the implementation needs.

- `src/context.rs`: `subscription_context_includes_provenance_tags`
  Assert the emitted system text contains the file path, filter label, and mtime/effective timestamp.

- `src/context.rs`: `subscription_context_orders_messages_by_effective_timestamp`
  Build records with distinct `activated_at`/`updated_at` values and assert older effective timestamps appear first.

- `src/turn.rs`: `build_turn_for_config_with_subscriptions_injects_t1_only`
  Assert T1 includes subscription material, while T2/T3 do not.

- `src/turn.rs`: `subscription_messages_taint_the_turn`
  Assert injected subscription system messages use `Principal::System` and cause `turn.is_tainted()` after inbound assembly. This is important because the security model explicitly requires subscription-derived context to retain taint.

- `src/turn.rs`: `provider_input_includes_subscription_context`
  Follow the existing inspecting-provider pattern already in `turn.rs`: create a temp session, add a subscription row for that session, build a T1 turn with loaded subscriptions, run `run_agent_loop`, and assert the provider-observed messages include the subscription system message and provenance tag.

- `src/store.rs`
  Add session-scoping tests:
  `list_subscriptions_for_session_excludes_other_sessions`
  `create_subscription_for_session_preserves_topic_but_loads_by_session`
  `refresh_subscription_timestamps_updates_rows_with_session_id_present`
  `store_new_migrates_legacy_subscriptions_table_without_session_id`
  The migration test is mandatory: create a legacy SQLite DB without the new column, open it through `Store::new`, and assert the old rows still load with the intended compatibility semantics, including visibility as `NULL`-scoped global compatibility rows until they are explicitly migrated.
  `session_subscription_overrides_legacy_null_subscription_on_duplicate_path_and_filter`
  This proves the duplicate-resolution rule instead of leaving it implicit.

- `src/agent/queue/tests.rs`: `queue_drain_rebuilds_turn_per_user_message`
  Enqueue two user turns for the same session, change the session subscriptions between the first and second turn, and assert the second provider invocation sees the updated subscription context. This is the regression test for the stale-`&Turn` bug the plan is fixing.

- `src/server/queue.rs`: `server_queue_drain_rebuilds_turn_per_user_message`
  Cover the separate server drain loop with the same freshness assertion, because `src/server/queue.rs` is not just a thin wrapper over `src/agent/queue.rs`.

- Fixture-update tests
  Any test broken by the new config field should get the minimal default-field fix, not new behavior.

## 4. Order Of Operations

1. Add `SubscriptionsConfig` to `src/config.rs` and fix all `Config { ... }` test fixtures first. This is the lowest-risk mechanical change and keeps later compile errors localized.
2. Extend `src/store.rs` with session-aware subscription storage/query APIs and add store tests for session isolation. Keep the old APIs as thin wrappers until all callers are moved.
3. Implement `SubscriptionContext` in `src/context.rs` with focused unit tests for empty input, filters, truncation, warnings, provenance, and ordering.
4. Add the session-aware turn-builder helper in `src/turn.rs`, wire `SubscriptionContext` into T1 only, and add turn/provider-input tests.
5. Add the queue-side compatibility layer in `src/agent/queue.rs` so a session-aware turn factory can exist without breaking current callers.
6. Move callers onto the new session-aware build path and add the queue freshness regression tests:
   `src/main.rs` for CLI turns,
   `src/server/queue.rs` for HTTP drains,
   `src/server/ws.rs` for WebSocket prompts,
   `src/agent/spawn.rs` for spawned-child drains.
7. Remove the old single-`&Turn` queue path after all callers and tests are green.
8. Update subscription CLI handling in `src/main.rs` to create/list/remove rows against the resolved session id.
9. Sync docs in `docs/risks.md`, `docs/architecture/overview.md`, and `docs/vision.md`.
10. Run the required checks in repo order:
   `cargo fmt --check`
   `cargo clippy -- -D warnings`
   `cargo test`
   `cargo build --release`
   optionally `cargo test --features integration` if auth is available.

## 5. Risk Assessment

- Storage-model mismatch
  The current table is topic-keyed, while the requested behavior is session-keyed and topic-independent. The implementation should add `session_id` and stop using topic as the context selector. Legacy rows should not be silently rebound to a hardcoded session; the compatibility rule should be explicit and global during transition. If legacy topic-labeled duplicates exist inside one session, dedupe by `(path, filter)` at context assembly time and prefer the newest effective timestamp.

- Freshness risk
  CLI, WebSocket, and queued HTTP drains currently build or reuse turns too early. If the queue path is not changed to acquire a fresh turn per queued user turn, subscription updates made between queued turns will still be missed.

- Taint/security risk
  Injected subscription messages must be `Principal::System`. If they are accidentally marked `Agent`, standing approvals and taint-sensitive shell policy behavior will silently weaken.

- Budget interaction risk
  The new subscription budget is additive to identity and replayed history. It bounds subscription growth, but it does not by itself make total provider input context-window-aware. The default should therefore be conservative.

- Provenance/mtime risk
  Filesystem `modified()` can fail or differ from the stored `updated_at` timestamp. Use live mtime when available and fall back to `effective_at()` in the provenance tag so output stays stable and the context builder still degrades gracefully.

- Warning noise risk
  Missing files can become recurring warnings every turn. The task explicitly says warn-and-skip, so the first implementation should do that, but the log volume should be watched after landing.
