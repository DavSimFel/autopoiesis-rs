use crate::agent::audit::make_denial_verdict;
use crate::agent::tests::common::*;
use crate::agent::usage::{charged_turn_meta, token_total};
use crate::llm::TurnMeta;

struct TailUserContext;

impl crate::context::ContextSource for TailUserContext {
    fn name(&self) -> &str {
        "tail-user"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        messages.push(ChatMessage::user("tail context user message"));
    }
}
#[tokio::test]
async fn trims_context_before_stream_completion_when_over_estimated_limit() {
    let dir = temp_sessions_dir("pre_call_trim");
    let (provider, observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();
    session.set_max_context_tokens(1);

    session.add_user_message("one").unwrap();
    session.add_user_message("two").unwrap();
    session.add_user_message("three").unwrap();

    let turn = Turn::new()
        .context(History::new(1_000))
        .tool(Shell::new())
        .guard(SecretRedactor::new(&[]).expect("empty secret redaction patterns are valid"));
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
    let _verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "new command".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    let observed = observed_message_counts
        .lock()
        .expect("observed mutex poisoned");
    assert!(
        observed.first().cloned().is_some_and(|count| count <= 3),
        "expected pre-call trimming to run before stream completion"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn delegation_hint_is_appended_after_successful_turn() {
    let dir = temp_sessions_dir("delegation_hint");
    let (provider, _observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .append(
            ChatMessage::user("seed delegation context"),
            Some(crate::llm::TurnMeta {
                input_tokens: Some(8),
                ..Default::default()
            }),
        )
        .unwrap();

    let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
        token_threshold: Some(0),
        tool_depth_threshold: None,
    });
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "please keep this short".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Executed(_)));
    assert_eq!(
        session.delegation_hint().unwrap().as_deref(),
        Some(crate::delegation::DELEGATION_HINT)
    );
    let reloaded_session = crate::session::Session::new(&dir).unwrap();
    assert_eq!(
        reloaded_session.delegation_hint().unwrap().as_deref(),
        Some(crate::delegation::DELEGATION_HINT)
    );
    assert!(!session.history().iter().any(|message| {
            matches!(message.role, crate::llm::ChatRole::System)
                && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
        }));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn delegation_hint_is_retained_when_provider_fails() {
    use std::sync::{Arc, Mutex};

    let dir = temp_sessions_dir("delegation_hint_failed_provider");
    let observed_hint = Arc::new(Mutex::new(false));

    #[derive(Clone)]
    struct FailingProvider {
        observed_hint: Arc<Mutex<bool>>,
    }

    impl crate::llm::LlmProvider for FailingProvider {
        fn stream_completion<'a>(
            &'a self,
            messages: &'a [ChatMessage],
            _tools: &'a [FunctionTool],
            _on_token: &'a mut (dyn FnMut(String) + Send),
        ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
            Box::pin(async move {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Err(anyhow::anyhow!("provider failure"))
            })
        }
    }
    let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
        token_threshold: Some(u64::MAX),
        tool_depth_threshold: None,
    });
    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
        .unwrap();
    let mut make_provider = {
        let provider = FailingProvider {
            observed_hint: observed_hint.clone(),
        };
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let error = match run_agent_loop(
        &mut make_provider,
        &mut session,
        "keep going".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    {
        Ok(_) => panic!("provider failure should bubble up"),
        Err(err) => err,
    };

    assert!(
        *observed_hint
            .lock()
            .expect("hint mutex should not be poisoned")
    );
    assert_eq!(
        session.delegation_hint().unwrap().as_deref(),
        Some(crate::delegation::DELEGATION_HINT)
    );
    assert!(error.to_string().contains("provider failure"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn delegation_hint_is_ignored_when_delegation_is_disabled() {
    use std::sync::{Arc, Mutex};

    let dir = temp_sessions_dir("delegation_hint_disabled");
    let observed_hint = Arc::new(Mutex::new(false));

    #[derive(Clone)]
    struct HintObservingProvider {
        observed_hint: Arc<Mutex<bool>>,
    }

    impl crate::llm::LlmProvider for HintObservingProvider {
        fn stream_completion<'a>(
            &'a self,
            messages: &'a [ChatMessage],
            _tools: &'a [FunctionTool],
            _on_token: &'a mut (dyn FnMut(String) + Send),
        ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
            Box::pin(async move {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Ok(StreamedTurn {
                    assistant_message: ChatMessage::system("ok"),
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                })
            })
        }
    }

    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
        .unwrap();
    let turn = Turn::new();
    let mut make_provider = {
        let provider = HintObservingProvider {
            observed_hint: observed_hint.clone(),
        };
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "keep going".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(
        !*observed_hint
            .lock()
            .expect("hint mutex should not be poisoned")
    );
    assert!(session.delegation_hint().unwrap().is_none());
    let reloaded_session = crate::session::Session::new(&dir).unwrap();
    assert!(reloaded_session.delegation_hint().unwrap().is_none());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn delegation_hint_is_cleared_after_successful_turn_without_new_advice() {
    use std::sync::{Arc, Mutex};

    let dir = temp_sessions_dir("delegation_hint_cleared");
    let observed_hint = Arc::new(Mutex::new(false));

    #[derive(Clone)]
    struct HintObservingProvider {
        observed_hint: Arc<Mutex<bool>>,
    }

    impl crate::llm::LlmProvider for HintObservingProvider {
        fn stream_completion<'a>(
            &'a self,
            messages: &'a [ChatMessage],
            _tools: &'a [FunctionTool],
            _on_token: &'a mut (dyn FnMut(String) + Send),
        ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
            Box::pin(async move {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Ok(StreamedTurn {
                    assistant_message: ChatMessage::system("ok"),
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                })
            })
        }
    }

    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
        .unwrap();
    let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
        token_threshold: Some(u64::MAX),
        tool_depth_threshold: None,
    });
    let mut make_provider = {
        let provider = HintObservingProvider {
            observed_hint: observed_hint.clone(),
        };
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "keep going".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(
        *observed_hint
            .lock()
            .expect("hint mutex should not be poisoned")
    );
    assert!(session.delegation_hint().unwrap().is_none());
    let reloaded_session = crate::session::Session::new(&dir).unwrap();
    assert!(reloaded_session.delegation_hint().unwrap().is_none());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn delegation_hint_accumulates_tool_calls_across_batches() {
    use std::sync::{Arc, Mutex};

    let dir = temp_sessions_dir("delegation_tool_batches");
    let observed_hints = Arc::new(Mutex::new(Vec::new()));

    #[derive(Clone)]
    struct HintObservingSequenceProvider {
        observed_hints: Arc<Mutex<Vec<bool>>>,
        call_index: Arc<Mutex<usize>>,
    }

    impl crate::llm::LlmProvider for HintObservingSequenceProvider {
        fn stream_completion<'a>(
            &'a self,
            messages: &'a [ChatMessage],
            _tools: &'a [FunctionTool],
            _on_token: &'a mut (dyn FnMut(String) + Send),
        ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
            Box::pin(async move {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                self.observed_hints
                    .lock()
                    .expect("hint mutex should not be poisoned")
                    .push(saw_hint);

                let mut call_index = self
                    .call_index
                    .lock()
                    .expect("call index mutex should not be poisoned");
                let turn = match *call_index {
                    0 => StreamedTurn {
                        assistant_message: ChatMessage::system("batch one"),
                        tool_calls: vec![
                            ToolCall {
                                id: "call-1".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                            ToolCall {
                                id: "call-2".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                        ],
                        meta: None,
                        stop_reason: StopReason::ToolCalls,
                    },
                    1 => StreamedTurn {
                        assistant_message: ChatMessage::system("batch two"),
                        tool_calls: vec![
                            ToolCall {
                                id: "call-3".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                            ToolCall {
                                id: "call-4".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                        ],
                        meta: None,
                        stop_reason: StopReason::ToolCalls,
                    },
                    _ => StreamedTurn {
                        assistant_message: ChatMessage::system("done"),
                        tool_calls: vec![],
                        meta: None,
                        stop_reason: StopReason::Stop,
                    },
                };
                *call_index += 1;
                Ok(turn)
            })
        }
    }

    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
        .unwrap();

    let turn = Turn::new()
        .tool(Shell::new())
        .delegation(crate::delegation::DelegationConfig {
            token_threshold: None,
            tool_depth_threshold: Some(3),
        });
    let mut make_provider = {
        let provider = HintObservingSequenceProvider {
            observed_hints: observed_hints.clone(),
            call_index: Arc::new(Mutex::new(0)),
        };
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "run the tools".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Executed(_)));
    assert_eq!(
        observed_hints
            .lock()
            .expect("hint mutex should not be poisoned")
            .as_slice(),
        &[true, true, true]
    );
    assert_eq!(
        session.delegation_hint().unwrap().as_deref(),
        Some(crate::delegation::DELEGATION_HINT)
    );
    let reloaded_session = crate::session::Session::new(&dir).unwrap();
    assert_eq!(
        reloaded_session.delegation_hint().unwrap().as_deref(),
        Some(crate::delegation::DELEGATION_HINT)
    );
    assert!(!session.history().iter().any(|message| {
            matches!(message.role, crate::llm::ChatRole::System)
                && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
        }));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn inbound_redaction_is_persisted_before_session_write() {
    let dir = temp_sessions_dir("redaction_persisted");
    let (provider, _observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();

    let turn = Turn::new().guard(
        SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"])
            .expect("test secret redaction regex should be valid"),
    );
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "please store sk-proj-abcdefghijklmnopqrstuvwxyz012345".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    let session_file = std::fs::read_to_string(session.today_path()).unwrap();
    assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
    assert!(session_file.contains("[REDACTED]"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn inbound_denial_returns_denied_without_looping() {
    let dir = temp_sessions_dir("inbound_denial");
    let (provider, observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();

    let turn = Turn::new().guard(InboundDenyGuard);
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "blocked prompt".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "blocked by test" && gate_id == "inbound-deny"
    ));
    assert!(
        observed_message_counts
            .lock()
            .expect("observed message count mutex poisoned")
            .is_empty()
    );

    let stored = session.history();
    assert_eq!(stored.len(), 2);
    assert!(matches!(stored[1].role, crate::llm::ChatRole::Assistant));
    assert_eq!(stored[1].principal, Principal::System);
    let note = match &stored[1].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert_eq!(note, "Message hard-denied by inbound-deny");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn approval_denial_audit_is_not_system_role_or_raw_command() {
    let dir = temp_sessions_dir("approval_denial_audit");
    let provider = SequenceProvider::new(vec![StreamedTurn {
        assistant_message: ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-1".to_string(),
                    name: "leak".to_string(),
                    arguments: serde_json::json!({"command":"reject marker command"}).to_string(),
                },
            }],
        },
        tool_calls: vec![ToolCall {
            id: "call-1".to_string(),
            name: "leak".to_string(),
            arguments: serde_json::json!({"command":"reject marker command"}).to_string(),
        }],
        meta: None,
        stop_reason: StopReason::ToolCalls,
    }]);
    let mut session = crate::session::Session::new(&dir).unwrap();

    struct RedactingApproval;

    impl Guard for RedactingApproval {
        fn name(&self) -> &str {
            "redacting-approval"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::ToolCall(_) => Verdict::Approve {
                    reason: "danger".to_string(),
                    gate_id: "needs-approval".to_string(),
                    severity: Severity::High,
                },
                _ => Verdict::Allow,
            }
        }
    }

    let turn = Turn::new().guard(RedactingApproval);
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "reject marker command".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "danger" && gate_id == "needs-approval"
    ));

    let stored = session.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    let text = match &stored[1].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert!(text.is_empty());
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    let note = match &stored[2].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert_eq!(
        note,
        "Tool execution rejected after approval by needs-approval"
    );
    assert!(!note.contains("reject marker command"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn context_insertion_does_not_replace_persisted_user_message() {
    let dir = temp_sessions_dir("persist_user_with_context");
    let identity_dir = dir.join("identity");
    std::fs::create_dir_all(identity_dir.join("agents/silas")).unwrap();
    std::fs::write(identity_dir.join("constitution.md"), "constitution").unwrap();
    std::fs::write(identity_dir.join("agents/silas/agent.md"), "You are Silas.").unwrap();
    std::fs::write(identity_dir.join("context.md"), "context").unwrap();

    let (provider, _observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .context(crate::context::Identity::new(
            crate::identity::t1_identity_files(&identity_dir, "silas"),
            std::collections::HashMap::new(),
            "fallback",
        ))
        .context(TailUserContext);
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "store this user prompt".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    let persisted_user = session
        .history()
        .iter()
        .find(|message| matches!(message.role, crate::llm::ChatRole::User))
        .expect("user message should be persisted");
    let content = match &persisted_user.content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text content"),
    };
    assert!(content.contains("store this user prompt"));
    assert_ne!(content, "tail context user message");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn context_insertion_modify_path_persists_indexed_user_message() {
    let dir = temp_sessions_dir("modify_user_with_context");
    let identity_dir = dir.join("identity");
    std::fs::create_dir_all(identity_dir.join("agents/silas")).unwrap();
    std::fs::write(identity_dir.join("constitution.md"), "constitution").unwrap();
    std::fs::write(identity_dir.join("agents/silas/agent.md"), "You are Silas.").unwrap();
    std::fs::write(identity_dir.join("context.md"), "context").unwrap();

    struct ModifyGuard;

    impl Guard for ModifyGuard {
        fn name(&self) -> &str {
            "modify-guard"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::Inbound(messages) => {
                    if let Some(user_message) = messages.iter_mut().rev().find(|message| {
                        message.role == crate::llm::ChatRole::User
                            && message.content.iter().any(|block| {
                                matches!(block, MessageContent::Text { text } if text.contains("modify this user prompt"))
                            })
                    }) {
                        for block in &mut user_message.content {
                            if let MessageContent::Text { text } = block {
                                *text = "modified user prompt".to_string();
                            }
                        }
                    }
                    Verdict::Modify
                }
                _ => Verdict::Allow,
            }
        }
    }

    let (provider, _observed_message_counts) = InspectingProvider::new();
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .context(crate::context::Identity::new(
            crate::identity::t1_identity_files(&identity_dir, "silas"),
            std::collections::HashMap::new(),
            "fallback",
        ))
        .context(TailUserContext)
        .guard(ModifyGuard);
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "modify this user prompt".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    let persisted_user = session
        .history()
        .iter()
        .find(|message| matches!(message.role, crate::llm::ChatRole::User))
        .expect("user message should be persisted");
    let content = match &persisted_user.content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text content"),
    };
    assert_eq!(content, "modified user prompt");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn token_total_does_not_overcharge_partial_metadata() {
    let mut assistant_message = ChatMessage::with_role_with_principal(
        crate::llm::ChatRole::Assistant,
        Some(Principal::Agent),
    );
    assistant_message
        .content
        .push(MessageContent::text("response"));
    let estimated_tokens = crate::session::Session::estimate_message_tokens(&assistant_message);

    let input_only = TurnMeta {
        model: None,
        input_tokens: Some(11),
        output_tokens: None,
        reasoning_tokens: None,
        reasoning_trace: None,
    };
    assert_eq!(
        token_total(Some(&input_only), &assistant_message),
        estimated_tokens.max(11)
    );

    let output_only = TurnMeta {
        model: None,
        input_tokens: None,
        output_tokens: Some(17),
        reasoning_tokens: None,
        reasoning_trace: None,
    };
    assert_eq!(
        token_total(Some(&output_only), &assistant_message),
        estimated_tokens.max(17)
    );
}

#[test]
fn charged_turn_meta_preserves_partial_usage_totals() {
    let mut assistant_message = ChatMessage::with_role_with_principal(
        crate::llm::ChatRole::Assistant,
        Some(Principal::Agent),
    );
    assistant_message
        .content
        .push(MessageContent::text("response"));
    let estimated_tokens = crate::session::Session::estimate_message_tokens(&assistant_message);

    let input_only = TurnMeta {
        model: None,
        input_tokens: Some(11),
        output_tokens: None,
        reasoning_tokens: None,
        reasoning_trace: None,
    };
    let charged = charged_turn_meta(Some(input_only), &assistant_message);
    assert_eq!(charged.input_tokens, Some(11));
    assert_eq!(
        charged.output_tokens,
        Some(estimated_tokens.saturating_sub(11))
    );

    let output_only = TurnMeta {
        model: None,
        input_tokens: None,
        output_tokens: Some(17),
        reasoning_tokens: None,
        reasoning_trace: None,
    };
    let charged = charged_turn_meta(Some(output_only), &assistant_message);
    assert_eq!(
        charged.input_tokens,
        Some(estimated_tokens.saturating_sub(17))
    );
    assert_eq!(charged.output_tokens, Some(17));
}

#[test]
fn max_denial_counter_returns_summary_after_threshold() {
    let mut denial_count = 0usize;

    let first = make_denial_verdict(
        &mut denial_count,
        "guard-1".to_string(),
        "first denial".to_string(),
    );
    assert!(matches!(
        first,
        TurnVerdict::Denied { ref reason, ref gate_id }
            if reason == "first denial" && gate_id == "guard-1"
    ));

    let second = make_denial_verdict(
        &mut denial_count,
        "guard-2".to_string(),
        "second denial".to_string(),
    );
    match second {
        TurnVerdict::Denied { reason, gate_id } => {
            assert_eq!(gate_id, "guard-2");
            assert!(reason.contains("stopped after 2 denied actions this turn"));
            assert!(reason.contains("last denial by guard-2: second denial"));
        }
        _ => panic!("expected denied verdict"),
    }

    assert_eq!(denial_count, 2);
}

#[tokio::test]
async fn inbound_approval_denial_audit_is_not_system_role_or_raw_command() {
    let dir = temp_sessions_dir("inbound_approval_denial_audit");
    let provider = SequenceProvider::new(vec![StreamedTurn {
        assistant_message: ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::text("unused")],
        },
        tool_calls: vec![],
        meta: None,
        stop_reason: StopReason::Stop,
    }]);
    let mut session = crate::session::Session::new(&dir).unwrap();

    struct NeedsApproval;

    impl Guard for NeedsApproval {
        fn name(&self) -> &str {
            "needs-approval"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::Inbound(_) => Verdict::Approve {
                    reason: "danger".to_string(),
                    gate_id: "needs-approval".to_string(),
                    severity: Severity::High,
                },
                _ => Verdict::Allow,
            }
        }
    }

    let turn = Turn::new()
        .guard(NeedsApproval)
        .guard(crate::gate::SecretRedactor::default_catalog());
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "sk-1234567890abcdef1234567890abcdef1234567890abcdef".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "danger" && gate_id == "needs-approval"
    ));

    let stored = session.history();
    assert_eq!(stored.len(), 2);
    let persisted_user = &stored[0];
    assert_eq!(persisted_user.role, crate::llm::ChatRole::User);
    let redacted_text = persisted_user
        .content
        .iter()
        .find_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .expect("expected redacted user text");
    assert!(redacted_text.contains("[REDACTED]"));
    assert!(!redacted_text.contains("sk-"));
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::System);
    let note = match &stored[1].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert_eq!(note, "Message rejected after approval by needs-approval");
    assert!(!note.contains("sk-"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn hard_deny_audit_is_not_system_role_or_raw_command() {
    let dir = temp_sessions_dir("hard_deny_audit");
    let provider = SequenceProvider::new(vec![StreamedTurn {
        assistant_message: ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-1".to_string(),
                    name: "leak".to_string(),
                    arguments: serde_json::json!({"command":"hard deny marker"}).to_string(),
                },
            }],
        },
        tool_calls: vec![ToolCall {
            id: "call-1".to_string(),
            name: "leak".to_string(),
            arguments: serde_json::json!({"command":"hard deny marker"}).to_string(),
        }],
        meta: None,
        stop_reason: StopReason::ToolCalls,
    }]);
    let mut session = crate::session::Session::new(&dir).unwrap();

    struct HardDeny;

    impl Guard for HardDeny {
        fn name(&self) -> &str {
            "hard-deny"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::ToolCall(_) => Verdict::Deny {
                    reason: "blocked".to_string(),
                    gate_id: "hard-deny".to_string(),
                },
                _ => Verdict::Allow,
            }
        }
    }

    let turn = Turn::new().guard(HardDeny);
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "hard deny marker".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "blocked" && gate_id == "hard-deny"
    ));

    let stored = session.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    let text = match &stored[1].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert!(text.is_empty());
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    let note = match &stored[2].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert_eq!(note, "Tool execution hard-denied by hard-deny");
    assert!(!note.contains("hard deny marker"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn denied_tool_calls_are_not_persisted_without_tool_results() {
    let dir = temp_sessions_dir("denied_tool_calls");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        None, "ls /tmp", "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let (tool, executions) = RecordingTool::new("marker-output");
    let turn = Turn::new().tool(tool).guard(ShellSafety::new());
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "deny tool call".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "shell command `ls /tmp` did not match any allowlist pattern"
                && gate_id == "shell-policy"
    ));
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);

    let stored = session.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[0].role, crate::llm::ChatRole::User);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    let text = match &stored[1].content[0] {
        MessageContent::Text { text } => text,
        _ => panic!("expected text audit note"),
    };
    assert!(text.is_empty());
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    assert!(
        stored[2]
            .content
            .iter()
            .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
    );
    assert!(
        !std::fs::read_to_string(session.today_path())
            .unwrap()
            .contains("marker-output")
    );
    assert!(!session.sessions_dir().join("results").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn denied_tool_calls_reload_with_placeholder_and_audit_note() {
    let dir = temp_sessions_dir("denied_tool_calls_reload");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        None, "ls /tmp", "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let (tool, _executions) = RecordingTool::new("marker-output");
    let turn = Turn::new().tool(tool).guard(ShellSafety::new());
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "deny tool call".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Denied { .. }));

    let mut reloaded = crate::session::Session::new(&dir).unwrap();
    reloaded.load_today().unwrap();
    let stored = reloaded.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    assert!(
        stored[2]
            .content
            .iter()
            .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn malformed_execute_arguments_deny_wins_over_batch_approval() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = temp_sessions_dir("malformed_execute_arguments");
    let provider = SequenceProvider::new(vec![crate::llm::StreamedTurn {
        assistant_message: crate::llm::ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![crate::llm::MessageContent::ToolCall {
                call: crate::llm::ToolCall {
                    id: "call-1".to_string(),
                    name: "execute".to_string(),
                    arguments: "not-json".to_string(),
                },
            }],
        },
        tool_calls: vec![crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: "not-json".to_string(),
        }],
        meta: None,
        stop_reason: StopReason::ToolCalls,
    }]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .tool(crate::tool::Shell::new())
        .guard(ShellSafety::new())
        .guard(crate::gate::ExfilDetector::new());
    let approval_count = Arc::new(AtomicUsize::new(0));
    let approval_count_seen = approval_count.clone();
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = move |_severity: &Severity, _reason: &str, _command: &str| {
        approval_count_seen.fetch_add(1, Ordering::SeqCst);
        true
    };

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "malformed execute arguments".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if gate_id == "shell-policy" && reason.contains("malformed JSON")
    ));
    assert_eq!(approval_count.load(Ordering::SeqCst), 0);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn denied_mixed_content_assistant_message_keeps_text_but_drops_tool_calls() {
    let dir = temp_sessions_dir("denied_mixed_content");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        Some("safe assistant text"),
        "ls /tmp",
        "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new().guard(ShellSafety::new());
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "deny mixed content".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "shell command `ls /tmp` did not match any allowlist pattern"
                && gate_id == "shell-policy"
    ));

    let stored = session.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    let text = message_text(&stored[1]).expect("expected persisted assistant text");
    assert_eq!(text, "safe assistant text");
    assert!(
        stored[1]
            .content
            .iter()
            .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
    );
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    let note = message_text(&stored[2]).expect("expected audit note");
    assert_eq!(
        note,
        "Tool execution rejected after approval by shell-policy"
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn protected_path_denial_writes_no_raw_tool_output_to_jsonl_or_results_dir() {
    let dir = temp_sessions_dir("protected_path_no_output");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        None,
        "cat ~/.autopoiesis/auth.json",
        "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let (tool, executions) = RecordingTool::new("marker-output-raw");
    let turn = Turn::new()
        .tool(tool)
        .guard(ShellSafety::with_policy(shell_policy(
            "approve",
            &["cat *"],
            &[],
            &[],
            "medium",
        )));
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "read protected path".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason.contains("reads protected credential path")
                && gate_id == "shell-policy"
    ));
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);

    let session_file = std::fs::read_to_string(session.today_path()).unwrap();
    assert!(!session_file.contains("marker-output-raw"));
    assert!(!session.sessions_dir().join("results").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn protected_path_denial_persists_only_safe_audit_material() {
    let dir = temp_sessions_dir("protected_path_audit");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        Some("safe protected-path assistant text"),
        "cat ~/.autopoiesis/auth.json",
        "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new().guard(ShellSafety::with_policy(shell_policy(
        "approve",
        &["cat *"],
        &[],
        &[],
        "medium",
    )));
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "read protected material".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason.contains("reads protected credential path")
                && gate_id == "shell-policy"
    ));

    let stored = session.history();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[1].principal, Principal::Agent);
    let text = message_text(&stored[1]).expect("expected persisted assistant text");
    assert_eq!(text, "safe protected-path assistant text");
    assert!(
        stored[1]
            .content
            .iter()
            .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
    );
    assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].principal, Principal::System);
    let note = message_text(&stored[2]).expect("expected audit note");
    assert_eq!(note, "Tool execution hard-denied by shell-policy");
    assert!(!note.contains("auth.json"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn metacharacter_smuggling_under_allowlisted_prefix_requires_approval() {
    let dir = temp_sessions_dir("metacharacter_smuggling");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        None,
        "cat /tmp/input.txt; echo smuggled",
        "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let (tool, executions) = RecordingTool::new("marker-output-smuggled");
    let turn = Turn::new()
        .tool(tool)
        .guard(ShellSafety::with_policy(shell_policy(
            "approve",
            &["cat *"],
            &[],
            &[],
            "medium",
        )));
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let approval_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let approval_count_seen = approval_count.clone();
    let mut token_sink = |_token: String| {};
    let mut approval_handler = move |_severity: &Severity, _reason: &str, _command: &str| {
        approval_count_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        false
    };

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "smuggle metacharacter".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { reason, gate_id }
            if reason == "compound shell command requires explicit approval"
                && gate_id == "shell-policy"
    ));
    assert_eq!(approval_count.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(session.history().len(), 3);
    assert!(!session.sessions_dir().join("results").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn truncated_shell_output_remains_explicit_in_session_pointer_and_result_file() {
    let dir = temp_sessions_dir("truncated_shell_output");
    let call_id = "call-truncated";
    let max_output_bytes = 5_000;
    let command = "printf '%9000s' ''";
    let provider = SequenceProvider::new(vec![
        streamed_turn_with_tool_call(None, command, call_id),
        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::text("done")],
            },
            tool_calls: vec![],
            meta: None,
            stop_reason: StopReason::Stop,
        },
    ]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .tool(Shell::with_max_output_bytes(max_output_bytes))
        .guard(ShellSafety::with_policy(shell_policy(
            "allow",
            &[],
            &[],
            &[],
            "medium",
        )));
    let mut make_provider = move || {
        let provider = provider.clone();
        async move { Ok::<_, anyhow::Error>(provider) }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "run bounded output test".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Executed(ref calls) if calls.len() == 1));

    let tool_message = session
        .history()
        .iter()
        .find(|message| message.role == crate::llm::ChatRole::Tool)
        .expect("tool result should be persisted");
    let pointer = tool_message
        .content
        .iter()
        .find_map(|block| match block {
            MessageContent::ToolResult { result } => Some(result.content.as_str()),
            _ => None,
        })
        .expect("tool result should have pointer text");
    let result_path = session.sessions_dir().join("results").join(format!(
        "{}.txt",
        crate::gate::output_cap::safe_call_id_for_filename(call_id)
    ));
    let result_path_str = result_path.display().to_string();

    assert!(pointer.contains("bounded capture"));
    assert!(pointer.contains(&result_path_str));
    assert!(pointer.contains("output exceeded inline limit"));

    let persisted = std::fs::read_to_string(&result_path).unwrap();
    assert!(persisted.contains(&crate::tool::shell_output_truncation_note(max_output_bytes)));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn tool_output_is_redacted_before_persist() {
    let dir = temp_sessions_dir("tool_redaction");
    let provider = SequenceProvider::new(vec![
        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::ToolCall {
                    call: ToolCall {
                        id: "call-1".to_string(),
                        name: "leak".to_string(),
                        arguments: "{}".to_string(),
                    },
                }],
            },
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "leak".to_string(),
                arguments: "{}".to_string(),
            }],
            meta: None,
            stop_reason: StopReason::ToolCalls,
        },
        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::text("done")],
            },
            tool_calls: vec![],
            meta: None,
            stop_reason: StopReason::Stop,
        },
    ]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new().tool(LeakyTool).guard(
        SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"])
            .expect("test secret redaction regex should be valid"),
    );
    let mut make_provider = move || {
        let provider = provider.clone();
        async move { Ok::<_, anyhow::Error>(provider) }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    run_agent_loop(
        &mut make_provider,
        &mut session,
        "use the tool".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    let session_file = std::fs::read_to_string(session.today_path()).unwrap();
    assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
    assert!(session_file.contains("[REDACTED]"));

    let tool_message = session
        .history()
        .iter()
        .find(|message| message.role == crate::llm::ChatRole::Tool)
        .expect("tool message should be persisted");
    assert_eq!(tool_message.principal, Principal::System);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn execute_tool_approval_prompts_once_and_persists_result() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let dir = temp_sessions_dir("execute_approval");
    let provider = SequenceProvider::new(vec![
        streamed_turn_with_tool_call(None, "echo approval", "call-1"),
        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::text("done")],
            },
            tool_calls: vec![],
            meta: None,
            stop_reason: StopReason::Stop,
        },
    ]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .tool(Shell::new())
        .guard(ShellSafety::with_policy(shell_policy(
            "approve",
            &[],
            &[],
            &[],
            "medium",
        )));
    let approvals = Arc::new(AtomicUsize::new(0));
    let approvals_seen = approvals.clone();
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = move |_severity: &Severity, _reason: &str, command: &str| {
        assert_eq!(command, "echo approval");
        approvals_seen.fetch_add(1, Ordering::SeqCst);
        true
    };

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "run execute approval".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Approved { ref tool_calls } if tool_calls.len() == 1));
    assert_eq!(approvals.load(Ordering::SeqCst), 1);

    let stored = session.history();
    assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(stored[2].role, crate::llm::ChatRole::Tool);
    assert!(session.sessions_dir().join("results").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn execute_tool_approval_rejection_leaves_no_result_file() {
    let dir = temp_sessions_dir("execute_denied");
    let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
        None,
        "echo denied",
        "call-1",
    )]);
    let mut session = crate::session::Session::new(&dir).unwrap();
    let turn = Turn::new()
        .tool(Shell::new())
        .guard(ShellSafety::with_policy(shell_policy(
            "approve",
            &[],
            &[],
            &[],
            "medium",
        )));
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "deny execute approval".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        TurnVerdict::Denied { gate_id, .. } if gate_id == "shell-policy"
    ));
    assert_eq!(session.history().len(), 3);
    assert_eq!(session.history()[1].role, crate::llm::ChatRole::Assistant);
    assert_eq!(session.history()[2].role, crate::llm::ChatRole::Assistant);
    assert_eq!(session.history()[2].principal, Principal::System);
    assert!(!session.sessions_dir().join("results").exists());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn replayed_tool_result_marks_followup_turn_tainted() {
    let dir = temp_sessions_dir("replayed_tool_taint");
    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .append(
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "stdout:\nok",
                Some(Principal::System),
            ),
            None,
        )
        .unwrap();

    let turn = Turn::new();
    let mut messages = session.history().to_vec();

    let verdict = turn.check_inbound(&mut messages, None);
    assert!(matches!(verdict, Verdict::Allow | Verdict::Modify));
    assert!(turn.is_tainted());

    std::fs::remove_dir_all(&dir).unwrap();
}
