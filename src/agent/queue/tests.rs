use crate::agent::tests::common::*;
#[tokio::test]
async fn drain_queue_processes_user_system_and_unknown_roles() {
    let root = temp_queue_root("mixed_roles");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let session_id = "worker";
    let mut store = Store::new(&queue_path).unwrap();
    store.create_session(session_id, None).unwrap();
    let user_id = store
        .enqueue_message(session_id, "user", "hello", "cli")
        .unwrap();
    let system_id = store
        .enqueue_message(session_id, "system", "operational note", "cli")
        .unwrap();
    let unknown_id = store
        .enqueue_message(session_id, "tool", "orphan tool result", "cli")
        .unwrap();

    let mut session = Session::new(&sessions_dir).unwrap();
    let turn = Turn::new();
    let provider_calls = std::sync::Arc::new(std::sync::Mutex::new(0usize));
    let provider_calls_seen = provider_calls.clone();
    let mut provider_factory = move || {
        let provider_calls_seen = provider_calls_seen.clone();
        async move {
            *provider_calls_seen
                .lock()
                .expect("provider call counter mutex poisoned") += 1;
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: crate::llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![MessageContent::text("ok")],
                    },
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                },
            })
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    assert!(
        drain_queue(
            &mut store,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap()
        .is_none()
    );

    assert_eq!(
        *provider_calls
            .lock()
            .expect("provider call counter mutex poisoned"),
        1
    );
    assert!(session.history().iter().any(|message| {
        matches!(message.role, crate::llm::ChatRole::System)
            && message_text(message) == Some("operational note")
    }));
    assert!(
        !session
            .history()
            .iter()
            .any(|message| { message_text(message) == Some("orphan tool result") })
    );

    let conn = Connection::open(&queue_path).unwrap();
    for message_id in [user_id, system_id, unknown_id] {
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");
    }

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn drain_queue_marks_failed_when_agent_loop_errors() {
    let root = temp_queue_root("failed_marking");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let session_id = "worker";
    let mut store = Store::new(&queue_path).unwrap();
    store.create_session(session_id, None).unwrap();
    let message_id = store
        .enqueue_message(session_id, "user", "run something", "cli")
        .unwrap();

    let mut session = Session::new(&sessions_dir).unwrap();
    let turn = Turn::new();
    let mut provider_factory = || async { Ok::<_, anyhow::Error>(FailingProvider) };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let result = drain_queue(
        &mut store,
        session_id,
        &mut session,
        &turn,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await;

    assert!(result.is_err());

    let conn = Connection::open(&queue_path).unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM messages WHERE id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "failed");

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn drain_queue_does_not_enqueue_completion_when_no_messages_were_processed() {
    let root = temp_queue_root("empty_queue");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();
    store
        .create_child_session("parent", "child", Some(r#"{"task":"noop"}"#))
        .unwrap();

    let mut session = Session::new(&sessions_dir).unwrap();
    let turn = Turn::new();
    let mut provider_factory = || async {
        Ok::<_, anyhow::Error>(StaticProvider {
            turn: StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text("unused")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        })
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    assert!(
        drain_queue(
            &mut store,
            "child",
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap()
        .is_none()
    );

    let conn = Connection::open(&queue_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 'parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    std::fs::remove_dir_all(&root).unwrap();
}
