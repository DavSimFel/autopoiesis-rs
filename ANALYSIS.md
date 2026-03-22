# Structural Analysis of `autopoiesis-rs`

## 1. Gate Split Analysis

### Overall assessment

The split is directionally correct, but the proposed file move is not a pure mechanical extraction. `src/agent.rs` currently contains three different kinds of logic:

1. turn orchestration and queue handling
2. gate-adjacent output filtering / streaming redaction / output capping
3. CLI presentation code

The split will reduce size and coupling, but there are a few cut lines that need explicit decisions first.

### Current dependency graph pressure points

- `src/turn.rs:5` imports `Guard`, `GuardEvent`, `Severity`, and `Verdict` directly from `crate::guard`, and `src/turn.rs:145-156` constructs concrete guard implementations. After the split, `turn.rs` should depend on `crate::gate` instead of `crate::guard`, but it will still be the composition root unless guard construction also moves behind a helper.
- `src/agent.rs:10` imports `Severity` and `Verdict` and uses them both for approval flow and output filtering. The agent loop is not currently isolated from gate concerns.
- `src/agent.rs:173-190` (`guard_text_output`, `guard_message_output`) are gate adapters even though they live in `agent.rs`. The proposed split list does not mention them, but leaving them behind means `agent.rs` is still not a “pure loop”.
- `src/agent.rs:192-510` contains the streaming secret prefix analyzer and tool output capping. Those are clean candidates for extraction, but the streaming code currently depends on `Turn`.
- `src/main.rs:128-129` depends on `agent::CliTokenSink` and `agent::CliApprovalHandler`. Moving these to `src/cli.rs` is straightforward for the binary, but it is an API-path break for any external consumer using the library export from `src/lib.rs:1-14`.
- `src/server.rs:557-650` already defines its own sink/approval types. The proposed `cli.rs` split only affects `main.rs` directly, not `server.rs`, but `server.rs` still depends on `agent::ApprovalHandler`, `agent::TokenSink`, and `agent::format_denial_message` at `src/server.rs:346-360` and `src/server.rs:483-497`.

### Circular dependency risks

- `gate/mod.rs` can own `Guard`, `Severity`, `Verdict`, and `GuardEvent` without creating a cycle. Those types currently only depend on `crate::llm::{ChatMessage, ToolCall}` in [`src/guard.rs:3-41`](/tmp/aprs-gate-analysis/src/guard.rs#L3).
- `gate/shell_safety.rs` will still depend on `crate::config::ShellPolicy` (`src/guard.rs:3`, `src/guard.rs:135-173`). That is a one-way dependency today, not a cycle, but it means the gate layer is not configuration-agnostic.
- The real cycle risk is `streaming_redact.rs`. `StreamingTextBuffer` currently takes `&Turn` and calls `guard_text_output(turn, ...)` (`src/agent.rs:340-462`), while `Turn` itself depends on the guard abstractions (`src/turn.rs:5`, `src/turn.rs:50-67`). If `gate::streaming_redact` imports `Turn`, you create `turn -> gate` and `gate -> turn`.
- To avoid that cycle, `StreamingTextBuffer` needs a new seam: it should accept either a callback/trait object for “emit this segment through the text guard” or a narrower gate-specific trait, not `&Turn`.

### Visibility changes needed

- If the split is done literally, `Guard`, `Severity`, `Verdict`, and `GuardEvent` probably belong in `src/gate/mod.rs` as `pub(crate)` unless the library intentionally exposes them as stable API. Right now `src/lib.rs:1-14` exports the entire `guard` module publicly, so renaming to `gate` is a public API break.
- The concrete guard types (`SecretRedactor`, `ShellSafety`, `ExfilDetector`) also look internal-only from the current source tree. Their callers are `turn.rs` and tests. They should likely be `pub(crate)` plus selectively re-exported from `gate/mod.rs` if you want a flat internal path.
- `safe_call_id_for_filename` is only used by `cap_tool_output` and tests (`src/agent.rs:465-510`, `src/agent.rs:1889-2016`). It should not be broader than `pub(crate)` and may not need to be exported from `gate/mod.rs` at all.
- `StreamingTextBuffer` is only used inside `run_agent_loop` today (`src/agent.rs:598-605`). It should likely stay module-private inside `gate/streaming_redact.rs` unless tests or future streaming sinks need it directly.
- Moving `CliTokenSink` and `CliApprovalHandler` to `src/cli.rs` also raises the same public API question: `src/main.rs` only needs crate-internal visibility, but `src/lib.rs` currently makes `agent` public.

### Test placement

- The existing `guard.rs` test block (`src/guard.rs:367-675`) should split with the implementations:
  - `SecretRedactor` tests into `gate/secret_redactor.rs`
  - `ShellSafety` tests into `gate/shell_safety.rs`
  - `ExfilDetector` tests into `gate/exfil_detector.rs`
- The streaming redaction tests now living in `src/agent.rs:1471-1751` belong with `gate/streaming_redact.rs`. Keeping them in `agent.rs` defeats the point of making the orchestration file smaller and focused.
- The output cap tests in `src/agent.rs:1815-2016` belong with `gate/output_cap.rs`.
- `turn.rs` should keep verdict-precedence and composition tests (`src/turn.rs:207-351`) because they validate guard orchestration, not individual guard behavior.
- `agent.rs` should keep only loop/queue behavior tests such as `src/agent.rs:1124-1469`, `src/agent.rs:1753-1813` where the interaction between provider, guard pipeline, session persistence, and queue state is the actual subject.

### Cut-line problems to resolve before splitting

- `guard_text_output` / `guard_message_output` are omitted from the proposed move list, but they are gate-related helpers (`src/agent.rs:173-190`). If they remain, `agent.rs` still owns output sanitization logic.
- The streaming secret detector duplicates the secret catalog instead of sharing it with `SecretRedactor`:
  - hardcoded prefixes in `src/agent.rs:199-203`
  - regexes passed into `SecretRedactor::new` in `src/turn.rs:148-152`
  - test helper patterns duplicated again in `src/guard.rs:373-378`
  Splitting files without introducing shared constants will preserve the current drift risk.
- `src/agent.rs:788-804` still writes operational text directly with `eprintln!`. If `agent.rs` is supposed to become a pure orchestration loop, that logging should move outward or be injected.
- `format_denial_message` at `src/agent.rs:169-171` is presentation formatting used by both CLI and server (`src/main.rs:163`, `src/main.rs:180`, `src/server.rs:359`, `src/server.rs:496`). That is another sign the agent module currently mixes loop semantics and UI text.
- `ShellSafety` still needs `ShellPolicy` from config. If the intent is a clean `gate/` subtree, either accept that coupling or move the normalized policy type closer to the gate code.
- `turn.rs` still directly wires the default guard stack (`src/turn.rs:145-156`). The split helps file size, but not the fan-out from `build_default_turn()`. A helper like `gate::default_stack(config)` would reduce that.

### Recommended split sequence

1. Move `CliTokenSink` and `CliApprovalHandler` out first so `agent.rs` drops `std::io` and presentation code.
2. Move `safe_call_id_for_filename` and `cap_tool_output` next; that extraction is independent and low-risk.
3. Move `SecretRedactor`, `ShellSafety`, `ExfilDetector`, and the shared guard enums/traits into `gate/`.
4. Refactor `StreamingTextBuffer` to stop depending on `Turn`, then move it.
5. Only after that call `src/agent.rs` “pure loop”; otherwise the file still owns output sanitization and console logging.

## 2. All Magic Numbers/Strings

Scope note: this excludes literals already declared as `const` and one-off fixture text that is only useful inline inside a single test. Everything below is a literal that should become a named constant or a shared static table.

### `src/template.rs`

- `src/template.rs:7` — `"{{{{{}}}}}"` — `TEMPLATE_TOKEN_FORMAT`

### `src/identity.rs`

- `src/identity.rs:16` — `"constitution.md"` — `CONSTITUTION_FILE_NAME`
- `src/identity.rs:17` — `"identity.md"` — `IDENTITY_FILE_NAME`
- `src/identity.rs:18` — `"context.md"` — `CONTEXT_FILE_NAME`
- `src/identity.rs:26` — `"\n\n"` — `SYSTEM_PROMPT_SECTION_SEPARATOR`

### `src/util.rs`

- `src/util.rs:22,24,84` — `719_468` — `UNIX_EPOCH_DAY_OFFSET`
- `src/util.rs:23,26,28,30` — `146_097` — `DAYS_PER_400_YEAR_ERA`
- `src/util.rs:28,31` — `146_096` — `LAST_DAY_OF_400_YEAR_ERA`
- `src/util.rs:31` — `1460` — `DAYS_PER_4_YEAR_CYCLE`
- `src/util.rs:31,33,83` — `365` — `DAYS_PER_COMMON_YEAR`
- `src/util.rs:31,33` — `36_524` — `DAYS_PER_100_YEAR_CYCLE_MINUS_ONE`
- `src/util.rs:32,79,80` — `400` — `YEARS_PER_400_YEAR_ERA`
- `src/util.rs:34,35,82` — `153` — `DAYS_PER_5_MONTH_CYCLE`
- `src/util.rs:40` — `"{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z"` — `UTC_TIMESTAMP_FORMAT`

### `src/config.rs`

- `src/config.rs:31` — `"gpt-5.4"` — `DEFAULT_MODEL`
- `src/config.rs:32` — `"You are a direct and capable coding agent. Execute tasks efficiently."` — `DEFAULT_SYSTEM_PROMPT`
- `src/config.rs:34` — `"https://chatgpt.com/backend-api/codex/responses"` — `DEFAULT_RESPONSES_BASE_URL`
- `src/config.rs:82` — `"AUTOPOIESIS_OPERATOR_KEY"` — `OPERATOR_KEY_ENV_VAR`
- `src/config.rs:258` — `"approve"` — `DEFAULT_SHELL_POLICY_ACTION`
- `src/config.rs:261` — `"medium"` — `DEFAULT_SHELL_POLICY_SEVERITY`

### `src/auth.rs`

- `src/auth.rs:37` — `"/root"` — `FALLBACK_HOME_DIR`
- `src/auth.rs:39` — `".autopoiesis"` — `AUTH_DIR_NAME`
- `src/auth.rs:40,366` — `"auth.json"` — `AUTH_FILE_NAME`
- `src/auth.rs:72` — `5` — `DEFAULT_DEVICE_AUTH_POLL_INTERVAL_SECONDS`
- `src/auth.rs:72` — `1` — `MIN_DEVICE_AUTH_POLL_INTERVAL_SECONDS`
- `src/auth.rs:96,226` — `"grant_type"` — `OAUTH_GRANT_TYPE_FIELD`
- `src/auth.rs:96` — `"refresh_token"` — `OAUTH_GRANT_TYPE_REFRESH_TOKEN`
- `src/auth.rs:97,227` — `"client_id"` — `OAUTH_CLIENT_ID_FIELD`
- `src/auth.rs:98` — `"refresh_token"` — `OAUTH_REFRESH_TOKEN_FIELD`
- `src/auth.rs:131` — `15 * 60` — `DEVICE_AUTH_TIMEOUT_SECONDS`
- `src/auth.rs:142` — `"device_auth_id"` — `DEVICE_AUTH_ID_FIELD`
- `src/auth.rs:143,315` — `"user_code"` — `USER_CODE_FIELD`
- `src/auth.rs:155` — `"."` — `DEVICE_AUTH_PROGRESS_MARKER`
- `src/auth.rs:175,196,305,394` — `"<failed to read response body>"` — `HTTP_BODY_READ_FALLBACK`
- `src/auth.rs:226` — `"authorization_code"` — `OAUTH_GRANT_TYPE_AUTHORIZATION_CODE`
- `src/auth.rs:228` — `"code"` — `OAUTH_CODE_FIELD`
- `src/auth.rs:229` — `"code_verifier"` — `OAUTH_CODE_VERIFIER_FIELD`
- `src/auth.rs:231` — `"redirect_uri"` — `OAUTH_REDIRECT_URI_FIELD`
- `src/auth.rs:232` — `"https://auth.openai.com/deviceauth/callback"` — `DEVICE_AUTH_CALLBACK_URL`
- `src/auth.rs:260` — `0o700` — `AUTH_DIR_PERMISSIONS`
- `src/auth.rs:268,275,399` — `0o600` — `AUTH_FILE_PERMISSIONS`
- `src/auth.rs:281` — `8` — `DEVICE_USER_CODE_LEN`
- `src/auth.rs:282` — `4` — `DEVICE_USER_CODE_GROUP_LEN`

### `src/context.rs`

- `src/context.rs:57` — `"identity"` — `IDENTITY_CONTEXT_SOURCE_NAME`
- `src/context.rs:127` — `"history"` — `HISTORY_CONTEXT_SOURCE_NAME`

### `src/turn.rs`

- `src/turn.rs:138` — `","` — `TOOL_NAME_SEPARATOR`
- `src/turn.rs:141` — `"model"` — `IDENTITY_VAR_MODEL`
- `src/turn.rs:142` — `"cwd"` — `IDENTITY_VAR_CWD`
- `src/turn.rs:143` — `"tools"` — `IDENTITY_VAR_TOOLS`
- `src/turn.rs:146` — `"identity"` — `DEFAULT_IDENTITY_DIR`
- `src/turn.rs:149` — `r"sk-[a-zA-Z0-9_-]{20,}"` — `OPENAI_SECRET_REGEX`
- `src/turn.rs:150` — `r"ghp_[a-zA-Z0-9]{36}"` — `GITHUB_PAT_REGEX`
- `src/turn.rs:151` — `r"AKIA[0-9A-Z]{16}"` — `AWS_ACCESS_KEY_REGEX`

### `src/tool.rs`

- `src/tool.rs:29` — `512` — `RLIMIT_NPROC_LIMIT`
- `src/tool.rs:30` — `16 * 1024 * 1024` — `RLIMIT_FSIZE_BYTES`
- `src/tool.rs:31` — `30` — `RLIMIT_CPU_SECONDS`
- `src/tool.rs:95,100,263` — `"execute"` — `SHELL_TOOL_NAME`
- `src/tool.rs:101` — `"Execute a shell command with optional timeout"` — `SHELL_TOOL_DESCRIPTION`
- `src/tool.rs:107` — `"Command to execute with sh -lc"` — `COMMAND_ARG_DESCRIPTION`
- `src/tool.rs:109,133` — `"timeout_ms"` — `TOOL_TIMEOUT_MS_FIELD`
- `src/tool.rs:111` — `"Optional timeout in milliseconds"` — `TIMEOUT_ARG_DESCRIPTION`
- `src/tool.rs:125,270,275` — `"command"` — `TOOL_COMMAND_FIELD`
- `src/tool.rs:128,130,288` — `"tool call requires a non-empty 'command' argument"` — `MISSING_COMMAND_ARG_ERROR`
- `src/tool.rs:135` — `1000` — `MILLIS_PER_SECOND`
- `src/tool.rs:146` — `"stdout:\n"` — `TOOL_STDOUT_PREFIX`
- `src/tool.rs:150` — `"\nstderr:\n"` — `TOOL_STDERR_PREFIX`
- `src/tool.rs:154` — `"\nexit_code="` — `TOOL_EXIT_CODE_PREFIX`
- `src/tool.rs:157` — `"signal"` — `SIGNAL_EXIT_CODE_LABEL`
- `src/tool.rs:194` — `"sh"` — `SHELL_BINARY`
- `src/tool.rs:195` — `"-lc"` — `SHELL_EXEC_ARGS`

### `src/guard.rs`

- `src/guard.rs:58` — `"secret-redactor"` — `SECRET_REDACTOR_ID`
- `src/guard.rs:68,433,449,465,666` — `"[REDACTED]"` — `REDACTION_MARKER`
- `src/guard.rs:154` — `"allow"` — `SHELL_POLICY_ACTION_ALLOW`
- `src/guard.rs:159` — `"low"` — `SEVERITY_LOW_STR`
- `src/guard.rs:161` — `"high"` — `SEVERITY_HIGH_STR`
- `src/guard.rs:168` — `"shell-policy"` — `SHELL_POLICY_GUARD_ID`
- `src/guard.rs:178,293` — `"command"` — `TOOL_COMMAND_FIELD`
- `src/guard.rs:230` — `"shell command did not match any allowlist pattern"` — `ALLOWLIST_MISS_REASON`
- `src/guard.rs:286` — `"exfiltration-detector"` — `EXFIL_DETECTOR_ID`
- `src/guard.rs:300` — `"/etc/passwd"` — `SENSITIVE_PASSWD_PATH`
- `src/guard.rs:301` — `"~/.ssh"` — `SENSITIVE_SSH_FRAGMENT`
- `src/guard.rs:302` — `".env"` — `SENSITIVE_ENV_FILE_FRAGMENT`
- `src/guard.rs:303` — `"auth.json"` — `SENSITIVE_AUTH_FILE_NAME`
- `src/guard.rs:308` — `"/dev/tcp"` — `TCP_DEVICE_PATH`
- `src/guard.rs:309` — `" curl "` — `CURL_INFIX_TOKEN`
- `src/guard.rs:310` — `"curl "` — `CURL_PREFIX_TOKEN`
- `src/guard.rs:311` — `" curl"` — `CURL_SUFFIX_TOKEN`
- `src/guard.rs:312` — `" wget "` — `WGET_INFIX_TOKEN`
- `src/guard.rs:313` — `"wget "` — `WGET_PREFIX_TOKEN`
- `src/guard.rs:314` — `" wget"` — `WGET_SUFFIX_TOKEN`
- `src/guard.rs:315` — `" nc "` — `NC_INFIX_TOKEN`
- `src/guard.rs:316` — `"nc "` — `NC_PREFIX_TOKEN`
- `src/guard.rs:317` — `" nc"` — `NC_SUFFIX_TOKEN`
- `src/guard.rs:353` — `"possible read-and-send exfiltration sequence detected across tool calls"` — `EXFIL_SEQUENCE_APPROVAL_REASON`

### `src/main.rs`

- `src/main.rs:12` — `"autopoiesis"` — `CLI_BINARY_NAME`
- `src/main.rs:12` — `"MVP Rust agent runtime"` — `CLI_ABOUT`
- `src/main.rs:35` — `8423` — `DEFAULT_SERVER_PORT`
- `src/main.rs:50,211` — `"default"` — `DEFAULT_SESSION_ID`
- `src/main.rs:69` — `"Run: autopoiesis auth login"` — `AUTH_LOGIN_HINT`
- `src/main.rs:96` — `"agents.toml"` — `DEFAULT_CONFIG_PATH`
- `src/main.rs:105` — `"sessions/queue.sqlite"` — `DEFAULT_QUEUE_DB_PATH`
- `src/main.rs:106` — `r#"{"source":"cli"}"#` — `CLI_SESSION_METADATA_JSON`
- `src/main.rs:136` — `"> "` — `REPL_PROMPT`
- `src/main.rs:146` — `"exit"` — `REPL_EXIT_COMMAND`
- `src/main.rs:146` — `"quit"` — `REPL_QUIT_COMMAND`
- `src/main.rs:150,168` — `"user"` — `QUEUE_ROLE_USER`
- `src/main.rs:150,168` — `"cli"` — `CLI_MESSAGE_SOURCE`

### `src/session.rs`

- `src/session.rs:60` — `100_000` — `DEFAULT_MAX_CONTEXT_TOKENS`
- `src/session.rs:71,153,324` — `"system"` — `ROLE_SYSTEM`
- `src/session.rs:72,154` — `"user"` — `ROLE_USER`
- `src/session.rs:73,155` — `"assistant"` — `ROLE_ASSISTANT`
- `src/session.rs:74,170` — `"tool"` — `ROLE_TOOL`
- `src/session.rs:86,226` — `"\n"` — `SESSION_CONTENT_SEPARATOR`
- `src/session.rs:208` — `"jsonl"` — `SESSION_FILE_EXTENSION`
- `src/session.rs:318` — `"{}.jsonl"` — `SESSION_FILE_NAME_FORMAT`
- `src/session.rs:318` — `10` — `UTC_DATE_PREFIX_LEN`

### `src/store.rs`

- `src/store.rs:37-54` — SQLite schema SQL blob — `INIT_SCHEMA_SQL`
- `src/store.rs:62` — `"{}"` — `EMPTY_SESSION_METADATA_JSON`
- `src/store.rs:65` — `"INSERT OR IGNORE INTO sessions (id, created_at, metadata) VALUES (?1, ?2, ?3)"` — `INSERT_SESSION_SQL`
- `src/store.rs:75` — `"SELECT id FROM sessions ORDER BY created_at ASC, id ASC"` — `LIST_SESSIONS_SQL`
- `src/store.rs:95` — `"INSERT INTO messages (session_id, role, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)"` — `ENQUEUE_MESSAGE_SQL`
- `src/store.rs:110-114` — pending-message dequeue SQL — `DEQUEUE_PENDING_MESSAGE_SQL`
- `src/store.rs:112,177` — `"pending"` — `MESSAGE_STATUS_PENDING`
- `src/store.rs:134,177` — `"processing"` — `MESSAGE_STATUS_PROCESSING`
- `src/store.rs:154` — `"processed"` — `MESSAGE_STATUS_PROCESSED`
- `src/store.rs:164` — `"failed"` — `MESSAGE_STATUS_FAILED`
- `src/store.rs:134` — `"UPDATE messages SET status = 'processing' WHERE id = ?1"` — `MARK_PROCESSING_SQL`
- `src/store.rs:154` — `"UPDATE messages SET status = 'processed' WHERE id = ?1"` — `MARK_PROCESSED_SQL`
- `src/store.rs:164` — `"UPDATE messages SET status = 'failed' WHERE id = ?1"` — `MARK_FAILED_SQL`
- `src/store.rs:177` — `"UPDATE messages SET status = 'pending' WHERE status = 'processing'"` — `RECOVER_STALE_MESSAGES_SQL`

### `src/server.rs`

- `src/server.rs:50,51,290` — `"user"` — `QUEUE_ROLE_USER`
- `src/server.rs:57` — `"operator"` — `OPERATOR_SOURCE_SUFFIX`
- `src/server.rs:58` — `"user"` — `USER_SOURCE_SUFFIX`
- `src/server.rs:60` — `"{transport}-{suffix}"` — `TRANSPORT_SOURCE_FORMAT`
- `src/server.rs:119` — `"agents.toml"` — `DEFAULT_CONFIG_PATH`
- `src/server.rs:120,121` — `"AUTOPOIESIS_API_KEY"` — `API_KEY_ENV_VAR`
- `src/server.rs:124` — `"sessions/queue.sqlite"` — `DEFAULT_QUEUE_DB_PATH`
- `src/server.rs:137` — `"sessions"` — `SESSIONS_DIR_NAME`
- `src/server.rs:145` — `[0, 0, 0, 0]` — `BIND_ALL_IPV4_ADDR`
- `src/server.rs:156` — `"/api/health"` — `HEALTH_ROUTE`
- `src/server.rs:157,158` — `"/api/sessions"` — `SESSIONS_ROUTE`
- `src/server.rs:159` — `"/api/sessions/:id/messages"` — `SESSION_MESSAGES_ROUTE`
- `src/server.rs:160` — `"/api/ws/:session_id"` — `WS_SESSION_ROUTE`
- `src/server.rs:169` — `"ok"` — `HEALTH_STATUS_OK`
- `src/server.rs:201,228` — `"invalid session id"` — `INVALID_SESSION_ID_MESSAGE`
- `src/server.rs:210` — `"http"` — `HTTP_TRANSPORT`
- `src/server.rs:249` — `r#"{{"op":"error","data":"{error}"}}"#` — `WS_ERROR_FRAME_FALLBACK_TEMPLATE`
- `src/server.rs:270` — `"invalid websocket frame"` — `INVALID_WS_FRAME_MESSAGE`
- `src/server.rs:289` — `"ws"` — `WS_TRANSPORT`
- `src/server.rs:331,339,368` — `"error: {error}"` — `WS_GENERIC_ERROR_PREFIX`
- `src/server.rs:388` — `"session-{now}"` — `GENERATED_SESSION_ID_FORMAT`
- `src/server.rs:394` — `128` — `MAX_SESSION_ID_LEN`
- `src/server.rs:414` — `"/api/ws/"` — `WS_PATH_PREFIX`
- `src/server.rs:419` — `"api_key"` — `WS_API_KEY_QUERY_PARAM`
- `src/server.rs:428` — `"missing or invalid api key"` — `INVALID_API_KEY_MESSAGE`
- `src/server.rs:515` — `"approval"` — `WS_OP_APPROVAL`
- `src/server.rs:517,522,536` — `"data"` — `WS_DATA_FIELD`
- `src/server.rs:518` — `"request_id"` — `WS_REQUEST_ID_FIELD`
- `src/server.rs:523` — `"approved"` — `WS_APPROVED_FIELD`
- `src/server.rs:537,539` — `"content"` — `WS_CONTENT_FIELD`
- `src/server.rs:603` — `1` — `INITIAL_APPROVAL_REQUEST_ID`
- `src/server.rs:647` — `"low"` — `SEVERITY_LOW_LABEL`
- `src/server.rs:648` — `"medium"` — `SEVERITY_MEDIUM_LABEL`
- `src/server.rs:649` — `"high"` — `SEVERITY_HIGH_LABEL`

### `src/llm/openai.rs`

- `src/llm/openai.rs:77` — `"\n"` — `SYSTEM_INSTRUCTIONS_JOINER`
- `src/llm/openai.rs:84,93,105` — `"role"` — `RESPONSES_ROLE_FIELD`
- `src/llm/openai.rs:84` — `"system"` — `RESPONSES_ROLE_SYSTEM`
- `src/llm/openai.rs:85,94,106` — `"content"` — `RESPONSES_CONTENT_FIELD`
- `src/llm/openai.rs:93` — `"user"` — `RESPONSES_ROLE_USER`
- `src/llm/openai.rs:105` — `"assistant"` — `RESPONSES_ROLE_ASSISTANT`
- `src/llm/openai.rs:111,126,145,191` — `"type"` — `RESPONSES_TYPE_FIELD`
- `src/llm/openai.rs:111,228` — `"function_call"` — `RESPONSES_TYPE_FUNCTION_CALL`
- `src/llm/openai.rs:112,127,200,214,234` — `"call_id"` — `RESPONSES_CALL_ID_FIELD`
- `src/llm/openai.rs:113,146,204,218,239` — `"name"` — `RESPONSES_NAME_FIELD`
- `src/llm/openai.rs:114,222,243` — `"arguments"` — `RESPONSES_ARGUMENTS_FIELD`
- `src/llm/openai.rs:126` — `"function_call_output"` — `RESPONSES_TYPE_FUNCTION_CALL_OUTPUT`
- `src/llm/openai.rs:128` — `"output"` — `RESPONSES_OUTPUT_FIELD`
- `src/llm/openai.rs:145` — `"function"` — `RESPONSES_TOOL_TYPE_FUNCTION`
- `src/llm/openai.rs:148` — `"parameters"` — `RESPONSES_PARAMETERS_FIELD`
- `src/llm/openai.rs:184` — `"data: [DONE]"` — `SSE_DONE_LINE`
- `src/llm/openai.rs:188` — `"data: "` — `SSE_DATA_PREFIX`
- `src/llm/openai.rs:194` — `"response.output_text.delta"` — `EVENT_OUTPUT_TEXT_DELTA`
- `src/llm/openai.rs:195,208` — `"delta"` — `EVENT_DELTA_FIELD`
- `src/llm/openai.rs:198` — `"response.function_call_arguments.delta"` — `EVENT_FUNCTION_CALL_ARGUMENTS_DELTA`
- `src/llm/openai.rs:212` — `"response.function_call_arguments.done"` — `EVENT_FUNCTION_CALL_ARGUMENTS_DONE`
- `src/llm/openai.rs:226` — `"response.output_item.done"` — `EVENT_OUTPUT_ITEM_DONE`
- `src/llm/openai.rs:227` — `"item"` — `EVENT_ITEM_FIELD`
- `src/llm/openai.rs:235` — `"id"` — `EVENT_ITEM_ID_FIELD`
- `src/llm/openai.rs:248` — `"response.completed"` — `EVENT_RESPONSE_COMPLETED`
- `src/llm/openai.rs:249` — `"response"` — `EVENT_RESPONSE_FIELD`
- `src/llm/openai.rs:250` — `"usage"` — `EVENT_USAGE_FIELD`
- `src/llm/openai.rs:254,362` — `"model"` — `MODEL_FIELD`
- `src/llm/openai.rs:258` — `"input_tokens"` — `INPUT_TOKENS_FIELD`
- `src/llm/openai.rs:260` — `"output_tokens"` — `OUTPUT_TOKENS_FIELD`
- `src/llm/openai.rs:262` — `"reasoning_tokens"` — `REASONING_TOKENS_FIELD`
- `src/llm/openai.rs:362` — `"input"` — `REQUEST_INPUT_FIELD`
- `src/llm/openai.rs:364` — `"stream"` — `REQUEST_STREAM_FIELD`
- `src/llm/openai.rs:365` — `"store"` — `REQUEST_STORE_FIELD`
- `src/llm/openai.rs:369` — `"instructions"` — `REQUEST_INSTRUCTIONS_FIELD`
- `src/llm/openai.rs:373` — `"tools"` — `REQUEST_TOOLS_FIELD`
- `src/llm/openai.rs:377` — `"reasoning"` — `REQUEST_REASONING_FIELD`
- `src/llm/openai.rs:377` — `"effort"` — `REQUEST_REASONING_EFFORT_FIELD`
- `src/llm/openai.rs:383` — `"Authorization"` — `AUTHORIZATION_HEADER`
- `src/llm/openai.rs:383` — `"Bearer {}"` — `BEARER_TOKEN_FORMAT`
- `src/llm/openai.rs:394` — `"<failed to read response body>"` — `HTTP_BODY_READ_FALLBACK`

### `src/agent.rs`

- `src/agent.rs:95` — `"⚠️"` — `LOW_SEVERITY_ICON`
- `src/agent.rs:96` — `"🟡"` — `MEDIUM_SEVERITY_ICON`
- `src/agent.rs:97` — `"🔴"` — `HIGH_SEVERITY_ICON`
- `src/agent.rs:100` — `"\n{prefix} {reason}"` — `APPROVAL_PROMPT_HEADER`
- `src/agent.rs:101` — `"  Command: {command}"` — `APPROVAL_PROMPT_COMMAND_LINE`
- `src/agent.rs:102` — `"  Approve? [y/n]: "` — `APPROVAL_PROMPT_INPUT`
- `src/agent.rs:109` — `"y"` — `APPROVAL_YES_RESPONSE`
- `src/agent.rs:141` — `"Tool execution rejected by user: {reason}. Command: {command}"` — `APPROVAL_DENIED_MESSAGE`
- `src/agent.rs:149` — `"Tool execution hard-denied by {by}: {reason}"` — `HARD_DENIAL_MESSAGE`
- `src/agent.rs:159` — `"stopped after {} denied actions this turn; last denial by {gate_id}: {reason}"` — `MAX_DENIALS_REACHED_MESSAGE`
- `src/agent.rs:170` — `"Command hard-denied by {gate_id}: {reason}"` — `FORMAT_DENIAL_MESSAGE`
- `src/agent.rs:252` — `20` — `SK_SECRET_MIN_SUFFIX_LEN`
- `src/agent.rs:256,264,270,275,281,392` — `3` — `SK_PREFIX_LEN`
- `src/agent.rs:395,402` — `4` — `FIXED_SECRET_PREFIX_LEN`
- `src/agent.rs:397` — `36` — `GITHUB_PAT_SUFFIX_LEN`
- `src/agent.rs:404` — `16` — `AWS_ACCESS_KEY_SUFFIX_LEN`
- `src/agent.rs:421` — `"[REDACTED]"` — `REDACTION_MARKER`
- `src/agent.rs:468,476` — `"call_"` — `SANITIZED_CALL_ID_PREFIX`
- `src/agent.rs:472` — `"_{:02X}"` — `CALL_ID_ESCAPE_FORMAT`
- `src/agent.rs:477` — `"empty"` — `EMPTY_CALL_ID_SUFFIX`
- `src/agent.rs:489,1881,1899,1927,2001,2013` — `"results"` — `RESULTS_DIR_NAME`
- `src/agent.rs:497,1882,1928,2002,2014` — `"{}.txt"` — `RESULT_FILE_NAME_FORMAT`
- `src/agent.rs:506` — `1024` — `BYTES_PER_KIB`
- `src/agent.rs:509` — `"[output exceeded inline limit ({line_count} lines, {size_kb} KB) -> {path_display}]\nTo read: cat {path_display}\nTo read specific lines: sed -n '10,20p' {path_display}"` — `OUTPUT_CAP_NOTICE_FORMAT`
- `src/agent.rs:529` — `"[{}] {}"` — `TIMESTAMPED_PROMPT_FORMAT`
- `src/agent.rs:562` — `"Message hard-denied by {gate_id}: {reason}"` — `INBOUND_DENIAL_MESSAGE`
- `src/agent.rs:580` — `"<inbound message>"` — `INBOUND_MESSAGE_PLACEHOLDER`
- `src/agent.rs:631,666` — `"<command unavailable>"` — `COMMAND_UNAVAILABLE_PLACEHOLDER`
- `src/agent.rs:682` — `r#"{{"error": "{err}"}}"#` — `TOOL_ERROR_JSON_FORMAT`
- `src/agent.rs:729` — `"user"` — `QUEUE_ROLE_USER`
- `src/agent.rs:740` — `"system"` — `QUEUE_ROLE_SYSTEM`
- `src/agent.rs:744` — `"assistant"` — `QUEUE_ROLE_ASSISTANT`
- `src/agent.rs:789` — `"Command approved by user and executed."` — `APPROVED_EXECUTION_LOG`
- `src/agent.rs:801` — `"unsupported queued role '{role}' for message {}"` — `UNSUPPORTED_ROLE_LOG_FORMAT`

## 3. Other Pattern Improvements

### Correctness / robustness

- `src/llm/openai.rs:505-518`: the trailing-buffer path only replays final text deltas. Final `function_call_arguments.done`, `response.completed`, and `[DONE]` events still bypass the normal handler path.
- `src/session.rs:149-187`: replay silently drops unknown roles and malformed tool rows. That is corruption-masking behavior, not recoverable parsing.
- `src/tool.rs:201-205`: `libc::setpgid(0, 0)` is called and its return value is ignored. If it fails, timeout cleanup can stop killing descendants while the code still assumes process-group isolation exists.
- `src/store.rs:118-140`: `dequeue_next_message()` returns a `QueuedMessage` whose `status` field is still `"pending"` even after the row was updated to `"processing"`. That field is stale at the moment it is handed to callers.
- `src/server.rs:383-389`: `generate_session_id()` is timestamp-only. `session-{as_nanos}` is not a strong uniqueness boundary across concurrent processes or clock anomalies.
- `src/context.rs:38-49`: `Identity::load_prompt()` panics in strict mode instead of returning `Result`. That is inconsistent with the repo-wide `anyhow::Result` convention and makes failure handling asymmetric.
- `src/agent.rs:529`: user prompts are rewritten as `"[timestamp] prompt"` before persistence and model submission. Timestamping belongs in metadata, not message content.
- `src/session.rs:126-129`, `src/session.rs:248-259`, `src/session.rs:391-423`: trimming mixes provider-wide usage totals with per-message context cost. `message_tokens` is storing completion accounting, not the actual marginal context weight of each retained message.

### Design / abstraction

- `src/context.rs:86-161`, `src/turn.rs:145-156`: `History` is a heavily tested abstraction that is not part of the real turn builder. Either wire it into `build_default_turn()` or delete it; the current middle state is dead complexity.
- `src/context.rs:7-9`, `src/guard.rs:39-41`: both `ContextSource::name()` and `Guard::name()` are effectively unused in production. They are API surface without a real consumer.
- `src/agent.rs:130-136`, `src/guard.rs:175-181`, `src/guard.rs:290-296`, `src/tool.rs:123-135`: tool-call JSON is reparsed in four places just to extract `"command"`/`"timeout_ms"`. This should be one shared typed parser.
- `src/agent.rs:199-203`, `src/turn.rs:148-152`, `src/guard.rs:373-378`: secret prefixes and regexes are duplicated across streaming redaction, default guard wiring, and tests. This catalog should exist once.
- `src/main.rs:108-126`, `src/server.rs:303-322`, `src/server.rs:446-465`: OpenAI provider-factory construction is duplicated across CLI, WS, and HTTP execution paths.
- `src/server.rs:283-374`, `src/server.rs:443-505`: WS and HTTP queue-drain paths duplicate the same turn/session/provider bootstrap and differ mainly in sink/approval types. That should be a single helper with injected transport behavior.
- `src/server.rs:245-274`, `src/server.rs:589-642`: WebSocket approval flow mixes `std::sync::mpsc`, thread-blocking waits, and `block_in_place()` into otherwise async code. The protocol wants an async request/response primitive.
- `src/store.rs:12-20`, `src/session.rs:21-40`: roles, statuses, and persisted entry kinds are stringly typed. These should be enums with explicit serialization, not open-ended `String` fields.
- `src/session.rs:192-214`, `src/session.rs:281-318`: `load_today()` is misnamed. It loads every `*.jsonl` file in the session directory, not just today’s file.

### API surface / file size / test structure

- `src/lib.rs:1-14`: the crate publicly re-exports every module, but many look internal-only.
  Candidates: `src/template.rs:3`, `src/identity.rs:9`, `src/server.rs:31,153`, `src/agent.rs:118-124,514-758`, `src/store.rs:12-19`.
- `src/server.rs:1-1110`, `src/session.rs:1-956`, `src/llm/openai.rs:1-948`, `src/guard.rs:1-675`: these are all too large even before touching `agent.rs`. Each mixes multiple responsibilities plus large co-located test blocks.
- `src/config.rs:98-109`, `src/identity.rs:48-57`, `src/context.rs:188-203`, `src/auth.rs:357-367`, `src/store.rs:191-199`, `src/session.rs:443-460`, `src/server.rs:665-698`, `src/agent.rs:1086-1108`: temp-dir/test-fixture helpers are duplicated all over the tree. They should move to one `#[cfg(test)]` support module.
- `src/auth.rs:339-342`: `code_challenge` is carried with `#[allow(dead_code)]` instead of either being validated or removed from the model type.

## 4. Recommended `src/` Layout

Assumption: keep unit tests adjacent to the code they validate, so the line-count estimates below are total file size including likely in-file test modules.

```text
src/
├── lib.rs                              (~20)
├── main.rs                             (~140)
├── config.rs                           (~260)
├── auth.rs                             (~380)
├── util.rs                             (~110)
├── turn.rs                             (~230)
├── prompt/
│   ├── mod.rs                          (~10)
│   ├── loader.rs                       (~120)
│   └── template.rs                     (~70)
├── context/
│   ├── mod.rs                          (~15)
│   ├── identity.rs                     (~140)
│   └── history.rs                      (~220)
├── gate/
│   ├── mod.rs                          (~20)
│   ├── types.rs                        (~60)
│   ├── secret_patterns.rs              (~50)
│   ├── secret_redactor.rs              (~220)
│   ├── shell_safety.rs                 (~240)
│   ├── exfil_detector.rs               (~170)
│   ├── output_cap.rs                   (~220)
│   └── streaming_redact.rs             (~430)
├── tool/
│   ├── mod.rs                          (~10)
│   └── shell.rs                        (~320)
├── llm/
│   ├── mod.rs                          (~170)
│   └── openai/
│       ├── mod.rs                      (~15)
│       ├── protocol.rs                 (~120)
│       ├── request.rs                  (~170)
│       ├── sse.rs                      (~360)
│       └── provider.rs                 (~220)
├── session/
│   ├── mod.rs                          (~15)
│   ├── types.rs                        (~120)
│   ├── persistence.rs                  (~220)
│   ├── replay.rs                       (~220)
│   └── trim.rs                         (~320)
├── store/
│   ├── mod.rs                          (~10)
│   └── sqlite.rs                       (~290)
├── agent/
│   ├── mod.rs                          (~15)
│   ├── loop.rs                         (~420)
│   ├── queue.rs                        (~160)
│   └── cli.rs                          (~120)
└── server/
    ├── mod.rs                          (~15)
    ├── state.rs                        (~50)
    ├── auth.rs                         (~90)
    ├── http.rs                         (~230)
    └── ws.rs                           (~420)
```

Layout intent:

- `prompt/`, `context/`, and `gate/` isolate prompt assembly, replay policy, and guard logic so `turn.rs` becomes composition-only.
- `llm/openai/` separates wire-format constants, request shaping, SSE parsing, and provider orchestration.
- `session/` separates persistence, replay, and trim/token policy so corruption handling and context trimming can evolve independently.
- `agent/` keeps only loop/queue/CLI concerns; streaming redaction and tool-output capping move out.
- `server/` splits HTTP routes, WS protocol, auth middleware, and state instead of keeping transport code in one 1.1K-line file.
