use crate::agent::tests::common::*;
use crate::llm::{ChatRole, StreamedTurn};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct ApprovalProbeGuard;

impl Guard for ApprovalProbeGuard {
    fn name(&self) -> &str {
        "approval-probe"
    }

    fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
        match event {
            GuardEvent::Inbound(_) => Verdict::Approve {
                reason: "probe".to_string(),
                gate_id: "approval-probe".to_string(),
                severity: crate::gate::Severity::High,
            },
            _ => Verdict::Allow,
        }
    }
}

#[derive(Clone)]
struct PanicProvider;

impl crate::llm::LlmProvider for PanicProvider {
    async fn stream_completion(
        &self,
        _messages: &[crate::llm::ChatMessage],
        _tools: &[crate::llm::FunctionTool],
        _on_token: &mut (dyn FnMut(String) + Send),
    ) -> anyhow::Result<StreamedTurn> {
        panic!("provider should not be called when inbound approval is denied");
    }
}

#[tokio::test]
async fn inbound_approval_prompt_forwards_user_message_text() {
    let dir = temp_sessions_dir("inbound_approval_prompt");
    let mut session = crate::session::Session::new(&dir).unwrap();
    session
        .append(
            ChatMessage {
                role: ChatRole::System,
                principal: Principal::System,
                content: vec![MessageContent::text("system prompt sentinel")],
            },
            None,
        )
        .unwrap();

    let turn = Turn::new().guard(ApprovalProbeGuard);
    let seen_command = Arc::new(Mutex::new(None));
    let mut make_provider = || async { Ok::<_, anyhow::Error>(PanicProvider) };
    let mut token_sink = |_token: String| {};
    let seen_command_for_handler = seen_command.clone();
    let mut approval_handler = move |_severity: &Severity, _reason: &str, command: &str| {
        *seen_command_for_handler.lock().unwrap() = Some(command.to_string());
        false
    };

    let verdict = run_agent_loop(
        &mut make_provider,
        &mut session,
        "actual user message".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert!(matches!(verdict, TurnVerdict::Denied { .. }));
    let command = seen_command.lock().unwrap().clone().unwrap();
    assert!(command.ends_with("actual user message"));
    assert!(!command.contains("system prompt sentinel"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn budget_ceiling_is_enforced_on_the_same_turn() {
    let dir = temp_sessions_dir("budget_next_turn");
    let config_path = dir.join("agents.toml");
    std::fs::write(
        &config_path,
        r#"
[agents.silas]
identity = "silas"

[agents.silas.t1]
model = "gpt-budget"

[budget]
max_tokens_per_turn = 10
max_tokens_per_session = 10
max_tokens_per_day = 10
"#,
    )
    .unwrap();

    let config = crate::config::Config::load(&config_path).unwrap();
    let turn = crate::turn::build_turn_for_config(&config).unwrap();
    let mut session = crate::session::Session::new(&dir).unwrap();
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider_turn = StreamedTurn {
        assistant_message: crate::llm::ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![crate::llm::MessageContent::text("ok")],
        },
        tool_calls: Vec::new(),
        meta: Some(crate::llm::TurnMeta {
            model: Some("gpt-budget".to_string()),
            input_tokens: Some(1),
            output_tokens: Some(20),
            reasoning_tokens: None,
            reasoning_trace: None,
        }),
        stop_reason: crate::llm::StopReason::Stop,
    };

    #[derive(Clone)]
    struct CountingProvider {
        calls: Arc<AtomicUsize>,
        turn: StreamedTurn,
    }

    impl crate::llm::LlmProvider for CountingProvider {
        async fn stream_completion(
            &self,
            _messages: &[crate::llm::ChatMessage],
            _tools: &[crate::llm::FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> anyhow::Result<StreamedTurn> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.turn.clone())
        }
    }

    let provider = CountingProvider {
        calls: provider_calls.clone(),
        turn: provider_turn,
    };
    let mut make_provider = {
        let provider = provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let first = run_agent_loop(
        &mut make_provider,
        &mut session,
        "first turn".to_string(),
        Principal::Operator,
        &turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();
    assert!(matches!(
        first,
        TurnVerdict::Denied {
            gate_id,
            ..
        } if gate_id == "budget"
    ));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.history().len(), 2);
    assert!(session.budget_snapshot().unwrap().turn_tokens > 10);

    std::fs::remove_dir_all(&dir).unwrap();
}
