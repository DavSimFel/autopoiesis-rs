use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::{Session, SessionEntry};
use crate::llm::{ChatMessage, ChatRole, MessageContent, TurnMeta};
use crate::principal::Principal;

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("writer lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn temp_sessions_dir(prefix: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aprs_session_test_{prefix}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn assistant_message(content: &str) -> ChatMessage {
    let mut message =
        ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
    message.content.push(MessageContent::text(content));
    message
}

fn assistant_with_blocks(blocks: Vec<MessageContent>) -> ChatMessage {
    ChatMessage {
        role: ChatRole::Assistant,
        principal: Principal::Agent,
        content: blocks,
    }
}

fn write_entries(path: &std::path::Path, entries: &[SessionEntry]) {
    let mut file = File::create(path).unwrap();
    for entry in entries {
        writeln!(file, "{}", serde_json::to_string(entry).unwrap()).unwrap();
    }
}

fn capture_warnings<F>(f: F) -> String
where
    F: FnOnce(),
{
    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let output = output.clone();
            move || SharedWriter(output.clone())
        })
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    f();

    String::from_utf8(output.lock().expect("writer lock").clone()).expect("utf8")
}

#[test]
fn append_user_message_writes_jsonl_line() {
    let dir = temp_sessions_dir("user_msg");
    let mut session = Session::new(&dir).unwrap();

    session.add_user_message("hello").unwrap();

    let path = session.today_path();
    assert!(path.exists(), "JSONL file should be created");

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);

    let entry: SessionEntry = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry.role, "user");
    assert!(entry.content.contains("hello"));
    assert!(!entry.ts.is_empty());
    assert_eq!(entry.principal, Some(Principal::Operator));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn append_assistant_message_includes_meta() {
    let dir = temp_sessions_dir("asst_meta");
    let mut session = Session::new(&dir).unwrap();

    let meta = TurnMeta {
        model: Some("gpt-5.3".to_string()),
        input_tokens: Some(50),
        output_tokens: Some(10),
        reasoning_tokens: Some(100),
        reasoning_trace: Some("I thought about it".to_string()),
    };

    session.append(ChatMessage::user("hi"), None).unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(meta),
        )
        .unwrap();

    let content = fs::read_to_string(session.today_path()).unwrap();
    let last_line = content.lines().last().unwrap();
    let entry: SessionEntry = serde_json::from_str(last_line).unwrap();

    assert_eq!(entry.role, "assistant");
    assert_eq!(entry.principal, Some(Principal::Agent));
    let m = entry.meta.expect("assistant message should have meta");
    assert_eq!(m.model, Some("gpt-5.3".to_string()));
    assert_eq!(m.input_tokens, Some(50));
    assert_eq!(m.output_tokens, Some(10));
    assert_eq!(m.reasoning_tokens, Some(100));
    assert!(m.reasoning_trace.is_some());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_preserves_mixed_assistant_block_order() {
    let dir = temp_sessions_dir("mixed_blocks");

    {
        let mut session = Session::new(&dir).unwrap();
        let assistant = assistant_with_blocks(vec![
            MessageContent::text("alpha"),
            MessageContent::ToolCall {
                call: crate::llm::ToolCall {
                    id: "call-1".to_string(),
                    name: "execute".to_string(),
                    arguments: "{\"command\":\"echo hi\"}".to_string(),
                },
            },
            MessageContent::text("omega"),
            MessageContent::ToolCall {
                call: crate::llm::ToolCall {
                    id: "call-2".to_string(),
                    name: "execute".to_string(),
                    arguments: "{\"command\":\"echo bye\"}".to_string(),
                },
            },
        ]);

        session
            .append(
                assistant,
                Some(TurnMeta {
                    model: Some("gpt-5.4".to_string()),
                    input_tokens: Some(7),
                    output_tokens: Some(3),
                    reasoning_tokens: None,
                    reasoning_trace: None,
                }),
            )
            .unwrap();
    }

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    let history = session.history();
    assert_eq!(history.len(), 1);
    assert!(matches!(
        &history[0].content[..],
        [
            MessageContent::Text { text },
            MessageContent::ToolCall { call },
            MessageContent::Text { text: tail },
            MessageContent::ToolCall { call: second_call },
        ] if text == "alpha"
            && call.id == "call-1"
            && tail == "omega"
            && second_call.id == "call-2"
    ));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_replays_legacy_assistant_tool_calls_without_blocks() {
    let dir = temp_sessions_dir("legacy_assistant_tool_calls");
    let path = dir.join("2026-03-19.jsonl");
    write_entries(
        &path,
        &[SessionEntry {
            role: "assistant".to_string(),
            content: "alpha".to_string(),
            blocks: Vec::new(),
            ts: "2026-03-19T00:00:00Z".to_string(),
            meta: None,
            principal: Some(Principal::Agent),
            call_id: None,
            tool_name: None,
            tool_calls: Some(vec![crate::llm::ToolCall {
                id: "call-1".to_string(),
                name: "execute".to_string(),
                arguments: "{\"command\":\"echo hi\"}".to_string(),
            }]),
        }],
    );

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    let history = session.history();
    assert_eq!(history.len(), 1);
    assert!(matches!(
        &history[0].content[..],
        [
            MessageContent::Text { text },
            MessageContent::ToolCall { call },
        ] if text == "alpha" && call.id == "call-1"
    ));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn today_path_is_date_based_jsonl() {
    let dir = temp_sessions_dir("path_format");
    let session = Session::new(&dir).unwrap();
    let path = session.today_path();

    let filename = path.file_name().unwrap().to_str().unwrap();
    assert!(filename.ends_with(".jsonl"));
    assert_eq!(filename.len(), 16);
    assert_eq!(&filename[4..5], "-");
    assert_eq!(&filename[7..8], "-");

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn sessions_dir_getter_returns_correct_path() {
    let dir = temp_sessions_dir("sessions_dir_getter");
    let session = Session::new(&dir).unwrap();

    assert_eq!(session.sessions_dir(), dir.as_path());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_restores_messages() {
    let dir = temp_sessions_dir("load_restore");

    {
        let mut session = Session::new(&dir).unwrap();
        session.add_user_message("first message").unwrap();
        session
            .append(ChatMessage::user("second message"), None)
            .unwrap();
    }

    {
        let mut session = Session::new(&dir).unwrap();
        session.load_today().unwrap();

        let history = session.history();
        assert!(history.len() >= 2);
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_replays_previous_day_files() {
    let dir = temp_sessions_dir("load_multiple_days");
    let yesterday = dir.join("2026-03-18.jsonl");
    let today = dir.join("2026-03-19.jsonl");
    write_entries(
        &yesterday,
        &[SessionEntry {
            role: "user".to_string(),
            content: "yesterday".to_string(),
            blocks: Vec::new(),
            ts: "2026-03-18T00:00:00Z".to_string(),
            meta: None,
            principal: Some(Principal::User),
            call_id: None,
            tool_name: None,
            tool_calls: None,
        }],
    );
    write_entries(
        &today,
        &[SessionEntry {
            role: "assistant".to_string(),
            content: "today".to_string(),
            blocks: Vec::new(),
            ts: "2026-03-19T00:00:00Z".to_string(),
            meta: None,
            principal: Some(Principal::Agent),
            call_id: None,
            tool_name: None,
            tool_calls: None,
        }],
    );

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    assert_eq!(session.history().len(), 2);
    assert!(matches!(session.history()[0].role, ChatRole::User));
    assert!(matches!(session.history()[1].role, ChatRole::Assistant));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_preserves_system_entries() {
    let dir = temp_sessions_dir("load_system_entries");

    {
        let mut session = Session::new(&dir).unwrap();
        session
            .append(ChatMessage::system("system note"), None)
            .unwrap();
        session.add_user_message("hello").unwrap();
    }

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    assert_eq!(session.history().len(), 2);
    assert!(matches!(session.history()[0].role, ChatRole::System));
    assert!(matches!(session.history()[1].role, ChatRole::User));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn legacy_entries_without_principal_deserialize_as_none() {
    let entry: SessionEntry =
        serde_json::from_str(r#"{"role":"user","content":"legacy","ts":"2026-03-19T00:00:00Z"}"#)
            .unwrap();

    assert_eq!(entry.principal, None);
}

#[test]
fn load_today_maps_legacy_entries_to_conservative_principals() {
    let dir = temp_sessions_dir("load_legacy_principals");
    let path = dir.join("2026-03-19.jsonl");
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        r#"{{"role":"user","content":"legacy user","ts":"2026-03-19T00:00:00Z"}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"role":"system","content":"legacy system","ts":"2026-03-19T00:00:01Z"}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"role":"assistant","content":"legacy assistant","ts":"2026-03-19T00:00:02Z"}}"#
    )
    .unwrap();
    writeln!(
        file,
        r#"{{"role":"tool","content":"stdout:\nok","ts":"2026-03-19T00:00:03Z","call_id":"call-1","tool_name":"execute"}}"#
    )
    .unwrap();

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    let history = session.history();
    assert_eq!(history.len(), 4);
    assert_eq!(history[0].principal, Principal::User);
    assert_eq!(history[1].principal, Principal::System);
    assert_eq!(history[2].principal, Principal::Agent);
    assert_eq!(history[3].principal, Principal::System);

    let turn = crate::turn::Turn::new();
    let mut messages = history.to_vec();
    let _ = turn.check_inbound(&mut messages, None);
    assert!(turn.is_tainted());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_drops_unknown_role_entries_with_warning() {
    let dir = temp_sessions_dir("load_unknown_role");
    let path = dir.join("2026-03-19.jsonl");
    write_entries(
        &path,
        &[
            SessionEntry {
                role: "user".to_string(),
                content: "kept".to_string(),
                blocks: Vec::new(),
                ts: "2026-03-19T00:00:00Z".to_string(),
                meta: None,
                principal: Some(Principal::User),
                call_id: None,
                tool_name: None,
                tool_calls: None,
            },
            SessionEntry {
                role: "mystery".to_string(),
                content: "dropped".to_string(),
                blocks: Vec::new(),
                ts: "2026-03-19T00:00:01Z".to_string(),
                meta: None,
                principal: None,
                call_id: None,
                tool_name: None,
                tool_calls: None,
            },
        ],
    );

    let mut session = Session::new(&dir).unwrap();
    let warnings = capture_warnings(|| {
        session.load_today().unwrap();
    });

    let history = session.history();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].role, ChatRole::User);
    assert!(warnings.contains("dropping session entry with unknown role"));
    assert!(warnings.contains("mystery"));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_drops_malformed_tool_entries_with_warning() {
    let dir = temp_sessions_dir("load_bad_tool");
    let path = dir.join("2026-03-19.jsonl");
    write_entries(
        &path,
        &[SessionEntry {
            role: "tool".to_string(),
            content: "stdout:\nok".to_string(),
            blocks: Vec::new(),
            ts: "2026-03-19T00:00:00Z".to_string(),
            meta: None,
            principal: Some(Principal::System),
            call_id: Some("call-1".to_string()),
            tool_name: None,
            tool_calls: None,
        }],
    );

    let mut session = Session::new(&dir).unwrap();
    let warnings = capture_warnings(|| {
        session.load_today().unwrap();
    });

    assert!(session.history().is_empty());
    assert!(warnings.contains("dropping tool entry with missing call_id or tool_name"));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_with_no_file_is_ok() {
    let dir = temp_sessions_dir("no_file");
    let mut session = Session::new(&dir).unwrap();

    session.load_today().unwrap();
    assert_eq!(session.history().len(), 0);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn load_today_trims_context_after_restore() {
    let dir = temp_sessions_dir("load_trim");

    let mut seed = Session::new(&dir).unwrap();
    let meta = TurnMeta {
        input_tokens: Some(50),
        output_tokens: Some(50),
        ..Default::default()
    };

    for _ in 0..2 {
        seed.append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(meta.clone()),
        )
        .unwrap();
    }

    let mut session = Session::new(&dir).unwrap();
    session.max_context_tokens = 50;
    session.load_today().unwrap();

    assert!(session.history().len() <= 1);
    assert!(session.total_tokens() <= 100);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn total_tokens_accumulates_from_meta() {
    let dir = temp_sessions_dir("token_count");
    let mut session = Session::new(&dir).unwrap();

    let meta1 = TurnMeta {
        input_tokens: Some(50),
        output_tokens: Some(10),
        ..Default::default()
    };
    let meta2 = TurnMeta {
        input_tokens: Some(100),
        output_tokens: Some(20),
        ..Default::default()
    };

    session.add_user_message("q1").unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(meta1),
        )
        .unwrap();
    session.add_user_message("q2").unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(meta2),
        )
        .unwrap();

    assert_eq!(session.total_tokens(), 180);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn budget_snapshot_reads_turn_session_and_day_totals() {
    let dir = temp_sessions_dir("budget_snapshot");
    let session = Session::new(&dir).unwrap();
    let yesterday = dir.join("2026-03-18.jsonl");
    let today = session.today_path();

    write_entries(
        &yesterday,
        &[SessionEntry {
            role: "assistant".to_string(),
            content: "old".to_string(),
            blocks: Vec::new(),
            ts: "2026-03-18T00:00:00Z".to_string(),
            meta: Some(TurnMeta {
                input_tokens: Some(50),
                output_tokens: Some(10),
                ..Default::default()
            }),
            principal: Some(Principal::Agent),
            call_id: None,
            tool_name: None,
            tool_calls: None,
        }],
    );
    write_entries(
        &today,
        &[
            SessionEntry {
                role: "user".to_string(),
                content: "prompt".to_string(),
                blocks: Vec::new(),
                ts: "2026-03-19T00:00:00Z".to_string(),
                meta: None,
                principal: Some(Principal::User),
                call_id: None,
                tool_name: None,
                tool_calls: None,
            },
            SessionEntry {
                role: "assistant".to_string(),
                content: "new".to_string(),
                blocks: Vec::new(),
                ts: "2026-03-19T00:01:00Z".to_string(),
                meta: Some(TurnMeta {
                    input_tokens: Some(20),
                    output_tokens: Some(5),
                    ..Default::default()
                }),
                principal: Some(Principal::Agent),
                call_id: None,
                tool_name: None,
                tool_calls: None,
            },
        ],
    );

    let mut session = Session::new(&dir).unwrap();
    session.load_today().unwrap();

    let snapshot = session.budget_snapshot().unwrap();
    assert_eq!(snapshot.turn_tokens, 25);
    assert_eq!(snapshot.session_tokens, 85);
    assert_eq!(snapshot.day_tokens, 25);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn budget_snapshot_keeps_session_total_after_trim_again() {
    let dir = temp_sessions_dir("budget_snapshot_trim");
    let mut session = Session::new(&dir).unwrap();
    session.set_max_context_tokens(1);

    session
        .append(
            assistant_message("first"),
            Some(TurnMeta {
                input_tokens: Some(40),
                output_tokens: Some(10),
                ..Default::default()
            }),
        )
        .unwrap();
    session
        .append(
            assistant_message("second"),
            Some(TurnMeta {
                input_tokens: Some(30),
                output_tokens: Some(5),
                ..Default::default()
            }),
        )
        .unwrap();

    let snapshot = session.budget_snapshot().unwrap();
    assert_eq!(snapshot.session_tokens, 85);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn budget_snapshot_ignores_trailing_assistant_audit_note() {
    let dir = temp_sessions_dir("budget_snapshot_audit");
    let mut session = Session::new(&dir).unwrap();

    session
        .append(
            assistant_message("real turn"),
            Some(TurnMeta {
                input_tokens: Some(12),
                output_tokens: Some(8),
                ..Default::default()
            }),
        )
        .unwrap();
    let mut audit_note =
        ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::System));
    audit_note.content.push(MessageContent::text("audit note"));
    session.append(audit_note, None).unwrap();

    let snapshot = session.budget_snapshot().unwrap();
    assert_eq!(snapshot.turn_tokens, 20);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn budget_snapshot_uses_latest_assistant_turn_even_without_metadata() {
    let dir = temp_sessions_dir("budget_snapshot_missing_meta");
    let mut session = Session::new(&dir).unwrap();

    session
        .append(
            assistant_message("real turn"),
            Some(TurnMeta {
                input_tokens: Some(12),
                output_tokens: Some(8),
                ..Default::default()
            }),
        )
        .unwrap();
    session
        .append(assistant_message("missing meta turn"), None)
        .unwrap();

    let snapshot = session.budget_snapshot().unwrap();
    assert_eq!(snapshot.turn_tokens, 20);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn budget_snapshot_counts_all_assistant_batches_in_latest_turn() {
    let dir = temp_sessions_dir("budget_snapshot_multi_batch");
    let mut session = Session::new(&dir).unwrap();

    session.add_user_message("first turn").unwrap();
    session
        .append(
            assistant_message("batch one"),
            Some(TurnMeta {
                input_tokens: Some(5),
                output_tokens: Some(5),
                ..Default::default()
            }),
        )
        .unwrap();
    session
        .append(
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            None,
        )
        .unwrap();
    session
        .append(
            assistant_message("batch two"),
            Some(TurnMeta {
                input_tokens: Some(7),
                output_tokens: Some(7),
                ..Default::default()
            }),
        )
        .unwrap();

    let snapshot = session.budget_snapshot().unwrap();
    assert_eq!(snapshot.turn_tokens, 24);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn estimate_tokens_returns_nonzero() {
    let dir = temp_sessions_dir("estimate_nonzero");
    let mut session = Session::new(&dir).unwrap();

    session.add_user_message("hello world").unwrap();
    assert!(session.estimate_context_tokens() > 0);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn estimate_tokens_counts_tool_call_arguments() {
    let assistant = assistant_with_blocks(vec![MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: "{\"command\":\"echo hi\"}".to_string(),
        },
    }]);

    assert!(Session::estimate_message_tokens(&assistant) > 0);
}

#[test]
fn trim_uses_estimation_when_no_metadata() {
    let dir = temp_sessions_dir("trim_estimate");
    let mut session = Session::new(&dir).unwrap();
    session.set_max_context_tokens(1);

    session.add_user_message("one").unwrap();
    session.add_user_message("two").unwrap();
    session.add_user_message("three").unwrap();
    session.trim_context();

    assert_eq!(session.history().len(), 1);

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn estimation_is_roughly_accurate() {
    let dir = temp_sessions_dir("estimate_rough");
    let mut session = Session::new(&dir).unwrap();

    session.add_user_message("hello world").unwrap();
    let tokens = session.estimate_context_tokens();
    assert!(
        (2..=4).contains(&tokens),
        "expected roughly 2-4 tokens, got {tokens}"
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn trim_drops_oldest_non_system_messages() {
    let dir = temp_sessions_dir("trim");
    let mut session = Session::new(&dir).unwrap();
    session.max_context_tokens = 50;

    let big_meta = TurnMeta {
        input_tokens: Some(30),
        output_tokens: Some(30),
        ..Default::default()
    };

    session
        .append(ChatMessage::system("instructions"), None)
        .unwrap();
    session
        .append(ChatMessage::user("old question"), None)
        .unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(big_meta.clone()),
        )
        .unwrap();
    session
        .append(ChatMessage::user("new question"), None)
        .unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(big_meta),
        )
        .unwrap();

    let history = session.history();
    assert!(history.len() < 5, "should have trimmed some messages");
    assert!(matches!(
        history.first().map(|message| &message.role),
        Some(ChatRole::System)
    ));

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn trim_does_not_pin_oldest_user_when_history_has_no_system_message() {
    let dir = temp_sessions_dir("trim_no_system_pin");
    let mut session = Session::new(&dir).unwrap();
    session.max_context_tokens = 50;

    let big_meta = TurnMeta {
        input_tokens: Some(30),
        output_tokens: Some(30),
        ..Default::default()
    };

    session
        .append(ChatMessage::user("oldest user"), None)
        .unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(big_meta.clone()),
        )
        .unwrap();
    session
        .append(ChatMessage::user("newer user"), None)
        .unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(big_meta),
        )
        .unwrap();

    assert!(
        session
            .history()
            .iter()
            .all(|message| match &message.content[..] {
                [MessageContent::Text { text }] => text != "oldest user",
                _ => true,
            }),
        "oldest user message should not be pinned forever"
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn trim_keeps_assistant_tool_roundtrip_intact() {
    let dir = temp_sessions_dir("trim_tool_roundtrip");
    let mut session = Session::new(&dir).unwrap();
    session.max_context_tokens = 50;

    let big_meta = TurnMeta {
        input_tokens: Some(30),
        output_tokens: Some(30),
        ..Default::default()
    };

    session
        .append(ChatMessage::system("instructions"), None)
        .unwrap();
    session.append(ChatMessage::user("first"), None).unwrap();
    session
        .append(
            ChatMessage {
                role: ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::ToolCall {
                    call: crate::llm::ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hi\"}".to_string(),
                    },
                }],
            },
            Some(big_meta.clone()),
        )
        .unwrap();
    session
        .append(
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "stdout:\nhi",
                Some(Principal::System),
            ),
            None,
        )
        .unwrap();
    session.append(ChatMessage::user("second"), None).unwrap();
    session
        .append(
            ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::Agent),
            ),
            Some(big_meta),
        )
        .unwrap();

    for (index, message) in session.history().iter().enumerate() {
        if message.role != ChatRole::Tool {
            continue;
        }

        assert!(
            index > 0,
            "tool result cannot be the first retained message"
        );
        let previous = &session.history()[index - 1];
        assert!(matches!(previous.role, ChatRole::Assistant));
        let tool_call_id = match &message.content[0] {
            MessageContent::ToolResult { result } => result.tool_call_id.as_str(),
            _ => panic!("expected tool result"),
        };
        let has_matching_call = previous.content.iter().any(|block| match block {
            MessageContent::ToolCall { call } => call.id == tool_call_id,
            _ => false,
        });
        assert!(
            has_matching_call,
            "tool result must retain its assistant tool call"
        );
    }

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reasoning_trace_saved_but_not_in_loaded_context() {
    let dir = temp_sessions_dir("reasoning");

    {
        let mut session = Session::new(&dir).unwrap();
        session.add_user_message("think hard").unwrap();
        session
            .append(
                ChatMessage::with_role_with_principal(
                    crate::llm::ChatRole::Assistant,
                    Some(Principal::Agent),
                ),
                Some(TurnMeta {
                    reasoning_trace: Some("deep thoughts here".to_string()),
                    ..Default::default()
                }),
            )
            .unwrap();
    }

    let content = fs::read_to_string({
        let s = Session::new(&dir).unwrap();
        s.today_path()
    })
    .unwrap();
    assert!(content.contains("deep thoughts here"));

    {
        let mut session = Session::new(&dir).unwrap();
        session.load_today().unwrap();

        for msg in session.history() {
            for block in &msg.content {
                if let crate::llm::MessageContent::Text { text } = block {
                    assert!(
                        !text.contains("deep thoughts here"),
                        "reasoning trace must not leak into context"
                    );
                }
            }
        }
    }

    fs::remove_dir_all(&dir).unwrap();
}

mod append_failure_regressions {
    use super::*;

    #[test]
    fn append_failure_keeps_memory_and_persistence_separate() {
        let dir = temp_sessions_dir("read_only_path_collision");
        let mut session = Session::new(&dir).unwrap();
        let today_path = session.today_path().to_path_buf();
        std::fs::create_dir_all(&today_path).unwrap();

        let assistant = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::text("persist me")],
        };

        let meta = TurnMeta {
            model: Some("gpt-test".to_string()),
            input_tokens: Some(11),
            output_tokens: Some(7),
            reasoning_tokens: None,
            reasoning_trace: None,
        };

        let error = session
            .append(assistant, Some(meta))
            .expect_err("append should fail when the target path is a directory");
        let error_text = error.to_string();
        assert!(!error_text.is_empty());
        assert!(session.history().is_empty());

        std::fs::remove_dir_all(&today_path).unwrap();
        let live_snapshot = session.budget_snapshot().unwrap();
        let mut reloaded = Session::new(&dir).unwrap();
        reloaded.load_today().unwrap();
        assert!(reloaded.history().is_empty());
        assert_eq!(live_snapshot, reloaded.budget_snapshot().unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
