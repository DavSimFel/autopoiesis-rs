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

#[tokio::test]
async fn drain_queue_does_not_enqueue_completion_for_bookkeeping_rows_only() {
    let root = temp_queue_root("bookkeeping_rows_only");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let session_id = "worker";
    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();
    store
        .create_child_session("parent", session_id, None)
        .unwrap();
    let system_id = store
        .enqueue_message(session_id, "system", "bookkeeping note", "cli")
        .unwrap();
    let assistant_id = store
        .enqueue_message(session_id, "assistant", "cached assistant output", "cli")
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
                        content: vec![MessageContent::text("unused")],
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
        0
    );

    let conn = Connection::open(&queue_path).unwrap();
    let parent_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 'parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent_count, 0);
    for message_id in [system_id, assistant_id] {
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
async fn drain_queue_continues_after_denial_and_processes_later_rows() {
    let root = temp_queue_root("denial_continues");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let session_id = "worker";
    let mut store = Store::new(&queue_path).unwrap();
    store.create_session(session_id, None).unwrap();
    let denied_id = store
        .enqueue_message(session_id, "user", "blocked prompt", "cli")
        .unwrap();
    let stored_id = store
        .enqueue_message(session_id, "system", "follow-up bookkeeping", "cli")
        .unwrap();

    let mut session = Session::new(&sessions_dir).unwrap();
    let turn = Turn::new().guard(InboundDenyGuard);
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

    let verdict = drain_queue(
        &mut store,
        session_id,
        &mut session,
        &turn,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, Some(TurnVerdict::Denied { .. })));
    assert_eq!(
        *provider_calls
            .lock()
            .expect("provider call counter mutex poisoned"),
        0
    );

    let conn = Connection::open(&queue_path).unwrap();
    for message_id in [denied_id, stored_id] {
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
async fn drain_queue_enqueues_completion_after_later_success_despite_earlier_denial() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let root = temp_queue_root("denial_followed_by_success");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    struct DenyFirstInbound {
        calls: Arc<AtomicUsize>,
    }

    impl Guard for DenyFirstInbound {
        fn name(&self) -> &str {
            "deny-first-inbound"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::Inbound(_) => {
                    let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
                    if call_index == 0 {
                        Verdict::Deny {
                            reason: "blocked first inbound message".to_string(),
                            gate_id: self.name().to_string(),
                        }
                    } else {
                        Verdict::Allow
                    }
                }
                _ => Verdict::Allow,
            }
        }
    }

    let session_id = "worker";
    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();
    store
        .create_child_session("parent", session_id, None)
        .unwrap();
    let denied_id = store
        .enqueue_message(session_id, "user", "blocked prompt", "cli")
        .unwrap();
    let allowed_id = store
        .enqueue_message(session_id, "user", "allowed prompt", "cli")
        .unwrap();

    let mut session = Session::new(&sessions_dir).unwrap();
    let turn = Turn::new().guard(DenyFirstInbound {
        calls: Arc::new(AtomicUsize::new(0)),
    });
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

    let verdict = drain_queue(
        &mut store,
        session_id,
        &mut session,
        &turn,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(verdict.is_none());
    assert_eq!(
        *provider_calls
            .lock()
            .expect("provider call counter mutex poisoned"),
        1
    );

    let conn = Connection::open(&queue_path).unwrap();
    let parent_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id = 'parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent_count, 1);
    for message_id in [denied_id, allowed_id] {
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
