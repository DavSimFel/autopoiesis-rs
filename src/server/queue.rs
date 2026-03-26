use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::auth as root_auth;
use crate::{agent, llm, session, turn};

use super::ServerState;

impl ServerState {
    fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .session_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

struct SessionLockLease {
    state: ServerState,
    session_id: String,
    lock: std::sync::Weak<Mutex<()>>,
}

impl Drop for SessionLockLease {
    fn drop(&mut self) {
        let mut locks = self
            .state
            .session_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(current) = locks.get(&self.session_id)
            && Arc::as_ptr(current) == self.lock.as_ptr()
            && Arc::strong_count(current) == 2
        {
            locks.remove(&self.session_id);
        }
    }
}

#[cfg(test)]
#[tracing::instrument(level = "debug", skip(state, turn, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
pub(super) async fn drain_session_queue<F, Fut, P, TS, AH>(
    state: ServerState,
    session_id: String,
    turn: &turn::Turn,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<Option<agent::TurnVerdict>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: llm::LlmProvider,
    TS: agent::TokenSink + Send + ?Sized,
    AH: agent::ApprovalHandler + ?Sized,
{
    let session_lock = state.session_lock(&session_id);
    let _session_lock_lease = SessionLockLease {
        state: state.clone(),
        session_id: session_id.clone(),
        lock: Arc::downgrade(&session_lock),
    };
    let _session_guard = session_lock.lock().await;
    let mut completed_agent_turn = false;
    let mut first_denial: Option<agent::TurnVerdict> = None;
    let mut last_assistant_response = None;

    let mut history = session::Session::new(state.sessions_dir.join(&session_id))
        .with_context(|| format!("failed to open session {session_id}"))?;
    history.load_today()?;
    loop {
        let message = {
            let mut store = state.store.lock().await;
            store.dequeue_next_message(&session_id)?
        };

        let Some(message) = message else {
            break;
        };
        let outcome = agent::process_message(
            &message,
            &mut history,
            turn,
            make_provider,
            token_sink,
            approval_handler,
        )
        .await;

        match outcome {
            Ok(agent::QueueOutcome::Agent(verdict)) => {
                {
                    let mut store = state.store.lock().await;
                    store.mark_processed(message.id)?;
                }

                if !matches!(verdict, agent::TurnVerdict::Denied { .. }) {
                    last_assistant_response = crate::spawn::latest_assistant_response(&history);
                    completed_agent_turn = true;
                }

                match verdict {
                    agent::TurnVerdict::Executed(_) => {}
                    agent::TurnVerdict::Approved { .. } => {
                        info!(
                            message_id = message.id,
                            "command approved by user and executed"
                        );
                    }
                    agent::TurnVerdict::Denied { reason, gate_id } => {
                        warn!(message_id = message.id, %gate_id, "turn denied");
                        if first_denial.is_none() {
                            first_denial = Some(agent::TurnVerdict::Denied { reason, gate_id });
                        }
                    }
                }
            }
            Ok(agent::QueueOutcome::Stored) => {
                let mut store = state.store.lock().await;
                store.mark_processed(message.id)?;
            }
            Ok(agent::QueueOutcome::UnsupportedRole(role)) => {
                warn!(message_id = message.id, %role, "unsupported queued role");
                let mut store = state.store.lock().await;
                store.mark_processed(message.id)?;
            }
            Err(error) => {
                let mut store = state.store.lock().await;
                store.mark_failed(message.id)?;
                return Err(error);
            }
        }
    }

    if crate::spawn::should_enqueue_child_completion(completed_agent_turn) {
        let mut store = state.store.lock().await;
        let _ = crate::spawn::enqueue_child_completion(
            &mut store,
            &session_id,
            &history,
            last_assistant_response.as_deref(),
        )?;
    }

    if completed_agent_turn {
        Ok(None)
    } else {
        Ok(first_denial)
    }
}

#[tracing::instrument(level = "debug", skip(state, turn_builder, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
pub(super) async fn drain_session_queue_with_turn_builder<F, Fut, P, TS, AH, TB>(
    state: ServerState,
    session_id: String,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<Option<agent::TurnVerdict>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: llm::LlmProvider,
    TS: agent::TokenSink + Send + ?Sized,
    AH: agent::ApprovalHandler + ?Sized,
    TB: FnMut() -> Result<turn::Turn>,
{
    let session_lock = state.session_lock(&session_id);
    let _session_lock_lease = SessionLockLease {
        state: state.clone(),
        session_id: session_id.clone(),
        lock: Arc::downgrade(&session_lock),
    };
    let _session_guard = session_lock.lock().await;

    let mut history = session::Session::new(state.sessions_dir.join(&session_id))
        .with_context(|| format!("failed to open session {session_id}"))?;
    history.load_today()?;

    let mut completed_agent_turn = false;
    let mut first_denial: Option<agent::TurnVerdict> = None;
    let mut last_assistant_response = None;

    loop {
        let message = {
            let mut store = state.store.lock().await;
            store.dequeue_next_message(&session_id)?
        };

        let Some(message) = message else {
            break;
        };

        let outcome = agent::process_message_with_turn_builder(
            &message,
            &mut history,
            turn_builder,
            make_provider,
            token_sink,
            approval_handler,
        )
        .await;

        match outcome {
            Ok(agent::QueueOutcome::Agent(verdict)) => {
                {
                    let mut store = state.store.lock().await;
                    store.mark_processed(message.id)?;
                }

                if !matches!(verdict, agent::TurnVerdict::Denied { .. }) {
                    last_assistant_response = crate::spawn::latest_assistant_response(&history);
                    completed_agent_turn = true;
                }

                match verdict {
                    agent::TurnVerdict::Executed(_) => {}
                    agent::TurnVerdict::Approved { .. } => {
                        info!(
                            message_id = message.id,
                            "command approved by user and executed"
                        );
                    }
                    agent::TurnVerdict::Denied { reason, gate_id } => {
                        warn!(message_id = message.id, %gate_id, "turn denied");
                        if first_denial.is_none() {
                            first_denial = Some(agent::TurnVerdict::Denied { reason, gate_id });
                        }
                    }
                }
            }
            Ok(agent::QueueOutcome::Stored) => {
                let mut store = state.store.lock().await;
                store.mark_processed(message.id)?;
            }
            Ok(agent::QueueOutcome::UnsupportedRole(role)) => {
                warn!(message_id = message.id, %role, "unsupported queued role");
                let mut store = state.store.lock().await;
                store.mark_processed(message.id)?;
            }
            Err(error) => {
                let mut store = state.store.lock().await;
                store.mark_failed(message.id)?;
                return Err(error);
            }
        }
    }

    if crate::spawn::should_enqueue_child_completion(completed_agent_turn) {
        let mut store = state.store.lock().await;
        let _ = crate::spawn::enqueue_child_completion(
            &mut store,
            &session_id,
            &history,
            last_assistant_response.as_deref(),
        )?;
    }

    if completed_agent_turn {
        Ok(None)
    } else {
        Ok(first_denial)
    }
}

pub(super) fn spawn_http_queue_worker(state: ServerState, session_id: String) {
    tokio::spawn(async move {
        let subscriptions = {
            let store = state.store.lock().await;
            match store.list_subscriptions_for_session(&session_id) {
                Ok(subscriptions) => match subscriptions
                    .into_iter()
                    .map(crate::subscription::SubscriptionRecord::from_row)
                    .collect::<Result<Vec<_>>>()
                {
                    Ok(subscriptions) => subscriptions,
                    Err(error) => {
                        warn!(
                            %session_id,
                            %error,
                            "failed to materialize subscriptions for queue turn"
                        );
                        return;
                    }
                },
                Err(error) => {
                    warn!(%session_id, %error, "failed to load subscriptions for queue turn");
                    return;
                }
            }
        };
        let mut turn_builder = {
            let config = state.config.clone();
            move || turn::build_turn_for_config_with_subscriptions(&config, &subscriptions)
        };
        let mut provider_factory = {
            let client = state.http_client.clone();
            let config = state.config.clone();
            move || {
                let client = client.clone();
                let config = config.clone();
                async move {
                    let api_key = root_auth::get_valid_token().await?;
                    Ok::<llm::openai::OpenAIProvider, anyhow::Error>(
                        llm::openai::OpenAIProvider::with_client(
                            client,
                            api_key,
                            config.base_url,
                            config.model,
                            config.reasoning_effort,
                        ),
                    )
                }
            }
        };
        let mut token_sink = NoopTokenSink;
        let mut approval_handler = RejectApprovalHandler;
        match drain_session_queue_with_turn_builder(
            state.clone(),
            session_id.clone(),
            &mut turn_builder,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            Ok(Some(verdict)) => match verdict {
                agent::TurnVerdict::Denied { reason: _, gate_id } => {
                    warn!(%gate_id, "http turn denied");
                }
                _ => unreachable!("drain_queue only returns denial verdicts"),
            },
            Ok(None) => {}
            Err(error) => {
                warn!(%session_id, %error, "failed to drain queued HTTP messages");
            }
        }
    });
}

struct NoopTokenSink;

impl agent::TokenSink for NoopTokenSink {
    fn on_token(&mut self, _token: String) {}
}

struct RejectApprovalHandler;

impl agent::ApprovalHandler for RejectApprovalHandler {
    fn request_approval(
        &mut self,
        _severity: &crate::gate::Severity,
        _reason: &str,
        _command: &str,
    ) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, anyhow};
    use serde_json::json;
    use std::sync::Arc;

    use crate::gate::{Guard, GuardEvent, Severity, Verdict};
    use crate::llm::{ChatMessage, FunctionTool, StopReason, StreamedTurn};
    use crate::principal::Principal;
    use crate::{agent, config, llm, session, store, turn};

    fn test_state() -> (ServerState, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_server_queue_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let store = store::Store::new(&queue_path).unwrap();

        (
            ServerState {
                store: Arc::new(tokio::sync::Mutex::new(store)),
                session_locks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
                sessions_dir,
                api_key: "test-key".to_string(),
                operator_key: Some("operator-key".to_string()),
                config: config::Config {
                    model: "gpt-test".to_string(),
                    system_prompt: "system".to_string(),
                    base_url: "https://example.test/api".to_string(),
                    reasoning_effort: None,
                    session_name: None,
                    operator_key: Some("operator-key".to_string()),
                    shell_policy: config::ShellPolicy::default(),
                    budget: None,
                    read: config::ReadToolConfig::default(),
                    subscriptions: config::SubscriptionsConfig::default(),
                    queue: config::QueueConfig::default(),
                    identity_files: crate::identity::t1_identity_files(
                        "identity-templates",
                        "silas",
                    ),
                    skills_dir: std::path::PathBuf::from("skills"),
                    skills_dir_resolved: std::path::PathBuf::from("skills"),
                    skills: crate::skills::SkillCatalog::default(),
                    agents: config::AgentsConfig::default(),
                    models: config::ModelsConfig::default(),
                    domains: config::DomainsConfig::default(),
                    active_agent: None,
                },
                http_client: reqwest::Client::new(),
            },
            queue_path,
        )
    }

    #[derive(Clone)]
    struct StaticProvider {
        turn: StreamedTurn,
    }

    impl llm::LlmProvider for StaticProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            Ok(self.turn.clone())
        }
    }

    #[derive(Clone)]
    struct SequenceProvider {
        turns: Arc<std::sync::Mutex<Vec<StreamedTurn>>>,
    }

    impl SequenceProvider {
        fn new(turns: Vec<StreamedTurn>) -> Self {
            Self {
                turns: Arc::new(std::sync::Mutex::new(turns.into_iter().rev().collect())),
            }
        }
    }

    impl llm::LlmProvider for SequenceProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.turns
                .lock()
                .expect("sequence provider mutex poisoned")
                .pop()
                .ok_or_else(|| anyhow!("no more turns"))
        }
    }

    #[derive(Clone)]
    struct BlockingProvider {
        label: &'static str,
        barrier: Arc<tokio::sync::Barrier>,
        starts: tokio::sync::mpsc::UnboundedSender<&'static str>,
        turn: StreamedTurn,
    }

    impl llm::LlmProvider for BlockingProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            let _ = self.starts.send(self.label);
            self.barrier.wait().await;
            Ok(self.turn.clone())
        }
    }

    fn blocking_turn(label: &'static str) -> StreamedTurn {
        StreamedTurn {
            assistant_message: ChatMessage {
                role: llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![llm::MessageContent::text(label)],
            },
            tool_calls: vec![],
            meta: None,
            stop_reason: StopReason::Stop,
        }
    }

    #[tokio::test]
    async fn drain_queue_marks_target_message_processed() {
        let (state, queue_path) = test_state();
        let session_id = "ws-session";
        let message_id = {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "hello", "ws")
                .unwrap()
        };

        let turn = turn::Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![llm::MessageContent::text("ok")],
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
            drain_session_queue(
                state.clone(),
                session_id.to_string(),
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");
    }

    #[tokio::test]
    async fn fresh_turn_builder_is_invoked_for_each_user_message() {
        let (state, queue_path) = test_state();
        let session_id = "fresh-turn-session";
        {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "first", "ws")
                .unwrap();
            store
                .enqueue_message(session_id, "user", "second", "ws")
                .unwrap();
        }

        let builder_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let builder_calls_for_closure = builder_calls.clone();
        let mut turn_builder = move || {
            builder_calls_for_closure.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<_, anyhow::Error>(turn::Turn::new())
        };
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![llm::MessageContent::text("ok")],
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
            drain_session_queue_with_turn_builder(
                state.clone(),
                session_id.to_string(),
                &mut turn_builder,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        assert_eq!(builder_calls.load(std::sync::atomic::Ordering::SeqCst), 2);

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1 AND status = 'processed'",
                [session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn drain_queue_uses_supplied_approval_handler() {
        let (state, queue_path) = test_state();
        let session_id = "approval-session";
        let message_id = {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "run risky command", "ws")
                .unwrap()
        };

        struct NeedsApproval;

        impl Guard for NeedsApproval {
            fn name(&self) -> &str {
                "needs-approval"
            }

            fn check(
                &self,
                event: &mut GuardEvent,
                _context: &crate::gate::GuardContext,
            ) -> Verdict {
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

        let tool_call = llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: json!({ "command": "rm -rf /tmp/demo" }).to_string(),
        };
        let turn = turn::Turn::new()
            .tool(crate::tool::Shell::new())
            .guard(NeedsApproval);
        let provider = SequenceProvider::new(vec![
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![llm::MessageContent::ToolCall {
                        call: tool_call.clone(),
                    }],
                },
                tool_calls: vec![tool_call],
                meta: None,
                stop_reason: StopReason::ToolCalls,
            },
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![llm::MessageContent::text("denied")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        ]);
        let mut provider_factory = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let approvals = Arc::new(std::sync::Mutex::new(0usize));
        let approvals_seen = approvals.clone();
        let mut token_sink = |_token: String| {};
        let mut approval_handler = move |_severity: &Severity, _reason: &str, _command: &str| {
            *approvals_seen
                .lock()
                .expect("approval counter mutex poisoned") += 1;
            false
        };
        let denial = drain_session_queue(
            state.clone(),
            session_id.to_string(),
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            denial,
            Some(agent::TurnVerdict::Denied { reason, gate_id })
                if reason == "danger" && gate_id == "needs-approval"
        ));

        assert_eq!(
            *approvals.lock().expect("approval counter mutex poisoned"),
            1
        );

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");
    }

    #[tokio::test]
    async fn drain_queue_enqueues_child_completion_message_for_parent_session() {
        let (state, queue_path) = test_state();
        let parent_session_id = "parent-session";
        let child_session_id = "child-session";
        {
            let mut store = state.store.lock().await;
            store.create_session(parent_session_id, None).unwrap();
            store
                .create_child_session(parent_session_id, child_session_id, None)
                .unwrap();
            store
                .enqueue_message(
                    child_session_id,
                    "user",
                    "run child task",
                    "agent-parent-session",
                )
                .unwrap();
        }

        let turn = turn::Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![llm::MessageContent::text("child finished")],
                    },
                    tool_calls: vec![],
                    meta: Some(llm::TurnMeta {
                        model: None,
                        input_tokens: None,
                        output_tokens: Some(1),
                        reasoning_tokens: None,
                        reasoning_trace: None,
                    }),
                    stop_reason: StopReason::Stop,
                },
            })
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        assert!(
            drain_session_queue(
                state.clone(),
                child_session_id.to_string(),
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let (role, content, source): (String, String, String) = conn
            .query_row(
                "SELECT role, content, source FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                [parent_session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(role, "user");
        assert_eq!(source, "agent-child-session");
        assert!(content.contains("Child session child-session completed."));
        assert!(content.contains("child finished"));
    }

    #[tokio::test]
    async fn drain_queue_does_not_enqueue_completion_for_empty_child_queue() {
        let (state, queue_path) = test_state();
        let parent_session_id = "parent-empty";
        let child_session_id = "child-empty";
        {
            let mut store = state.store.lock().await;
            store.create_session(parent_session_id, None).unwrap();
            store
                .create_child_session(parent_session_id, child_session_id, None)
                .unwrap();
        }

        let turn = turn::Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![llm::MessageContent::text("unused")],
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
            drain_session_queue(
                state.clone(),
                child_session_id.to_string(),
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                [parent_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn drain_queue_enqueues_completion_when_persisted_history_exists_but_new_assistant_response_is_empty()
     {
        let (state, queue_path) = test_state();
        let parent_session_id = "parent-persisted";
        let child_session_id = "child-persisted";
        {
            let mut store = state.store.lock().await;
            store.create_session(parent_session_id, None).unwrap();
            store
                .create_child_session(parent_session_id, child_session_id, None)
                .unwrap();
            store
                .enqueue_message(
                    child_session_id,
                    "user",
                    "run child task",
                    "agent-parent-persisted",
                )
                .unwrap();
        }

        let mut history = session::Session::new(state.sessions_dir.join(child_session_id)).unwrap();
        history
            .append(
                ChatMessage {
                    role: llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![llm::MessageContent::text("old answer")],
                },
                None,
            )
            .unwrap();

        let turn = turn::Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![llm::MessageContent::text("")],
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
            drain_session_queue(
                state.clone(),
                child_session_id.to_string(),
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let content: String = conn
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                [parent_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(content.contains("Child session child-persisted completed."));
        assert!(!content.contains("old answer"));
    }

    #[tokio::test]
    async fn different_sessions_do_not_block_each_other() {
        let (state, _queue_path) = test_state();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (starts_tx, mut starts_rx) = tokio::sync::mpsc::unbounded_channel();

        for session_id in ["session-a", "session-b"] {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "hello", "ws")
                .unwrap();
        }

        let turn = Arc::new(turn::Turn::new());
        let state_a = state.clone();
        let barrier_a = barrier.clone();
        let starts_a = starts_tx.clone();
        let turn_a = turn.clone();
        let worker_a = tokio::spawn(async move {
            let provider = BlockingProvider {
                label: "session-a",
                barrier: barrier_a,
                starts: starts_a,
                turn: blocking_turn("session-a"),
            };
            let mut provider_factory = move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
            drain_session_queue(
                state_a,
                "session-a".to_string(),
                turn_a.as_ref(),
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
        });

        let state_b = state.clone();
        let barrier_b = barrier.clone();
        let starts_b = starts_tx.clone();
        let turn_b = turn.clone();
        let worker_b = tokio::spawn(async move {
            let provider = BlockingProvider {
                label: "session-b",
                barrier: barrier_b,
                starts: starts_b,
                turn: blocking_turn("session-b"),
            };
            let mut provider_factory = move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
            drain_session_queue(
                state_b,
                "session-b".to_string(),
                turn_b.as_ref(),
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
        });

        let mut started = vec![
            tokio::time::timeout(std::time::Duration::from_secs(2), starts_rx.recv())
                .await
                .expect("first session should start")
                .unwrap(),
            tokio::time::timeout(std::time::Duration::from_secs(2), starts_rx.recv())
                .await
                .expect("second session should start")
                .unwrap(),
        ];
        started.sort_unstable();
        assert_eq!(started, vec!["session-a", "session-b"]);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let (result_a, result_b) = tokio::join!(worker_a, worker_b);
            result_a.expect("session-a worker should complete successfully");
            result_b.expect("session-b worker should complete successfully");
        })
        .await
        .expect("different sessions should not serialize");
    }

    #[tokio::test]
    async fn same_session_processing_is_serialized() {
        let (state, _queue_path) = test_state();
        let session_id = "serialized-session";

        {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "hello", "ws")
                .unwrap();
        }

        #[derive(Clone)]
        struct BlockingProvider {
            first_started: Arc<tokio::sync::Notify>,
            release: Arc<tokio::sync::Notify>,
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl llm::LlmProvider for BlockingProvider {
            async fn stream_completion(
                &self,
                _messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                match self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => {
                        self.first_started.notify_one();
                        self.release.notified().await;
                        Ok(blocking_turn("serialized"))
                    }
                    1 => Ok(blocking_turn("serialized")),
                    other => panic!("unexpected extra provider call: {other}"),
                }
            }
        }

        let release = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let turn = Arc::new(turn::Turn::new());

        let state_a = state.clone();
        let turn_a = turn.clone();
        let provider = BlockingProvider {
            first_started: first_started.clone(),
            release: release.clone(),
            calls: calls.clone(),
        };
        let provider_a = provider.clone();
        let provider_b = provider.clone();
        let worker_a = tokio::spawn(async move {
            let mut provider_factory = move || {
                let provider = provider_a.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
            drain_session_queue(
                state_a,
                session_id.to_string(),
                turn_a.as_ref(),
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), first_started.notified())
            .await
            .expect("first worker should reach provider startup");

        let state_b = state.clone();
        let turn_b = turn.clone();
        let mut worker_b = tokio::spawn(async move {
            let mut provider_factory = move || {
                let provider = provider_b.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
            drain_session_queue(
                state_b,
                session_id.to_string(),
                turn_b.as_ref(),
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
        });

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), &mut worker_b)
                .await
                .is_err(),
            "second drain_session_queue call should stay pending until the first worker releases the session"
        );

        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert!(
                worker_a
                    .await
                    .expect("first worker should complete successfully")
                    .is_ok(),
                "first worker drain should succeed"
            );
            worker_b
                .await
                .expect("second worker should complete successfully")
                .expect("second worker drain should succeed");
        })
        .await
        .expect("both workers should finish after lock release");
    }

    #[tokio::test]
    async fn store_mutex_is_not_held_across_agent_turn() {
        let (state, _queue_path) = test_state();
        let release = Arc::new(tokio::sync::Notify::new());
        let (starts_tx, mut starts_rx) = tokio::sync::mpsc::unbounded_channel();
        let session_id = "store-release-session";

        {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "hello", "ws")
                .unwrap();
        }

        let turn = Arc::new(turn::Turn::new());

        #[derive(Clone)]
        struct NotifyProvider {
            label: &'static str,
            release: Arc<tokio::sync::Notify>,
            starts: tokio::sync::mpsc::UnboundedSender<&'static str>,
            turn: StreamedTurn,
        }

        impl llm::LlmProvider for NotifyProvider {
            async fn stream_completion(
                &self,
                _messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                let _ = self.starts.send(self.label);
                self.release.notified().await;
                Ok(self.turn.clone())
            }
        }

        let state_worker = state.clone();
        let release_worker = release.clone();
        let starts_worker = starts_tx.clone();
        let turn_worker = turn.clone();
        let worker = tokio::spawn(async move {
            let provider = NotifyProvider {
                label: "worker",
                release: release_worker,
                starts: starts_worker,
                turn: blocking_turn("worker"),
            };
            let mut provider_factory = move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
            drain_session_queue(
                state_worker,
                session_id.to_string(),
                turn_worker.as_ref(),
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
        });

        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), starts_rx.recv())
                .await
                .expect("worker should start")
                .unwrap(),
            "worker"
        );

        let store_task = {
            let state = state.clone();
            async move {
                let mut store = state.store.lock().await;
                store.create_session("unblocked", None).unwrap();
            }
        };
        tokio::time::timeout(std::time::Duration::from_millis(200), store_task)
            .await
            .expect("store mutex should be released before provider execution");

        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            worker
                .await
                .expect("worker should finish after barrier release");
        })
        .await
        .expect("worker should finish after barrier release");
    }

    #[tokio::test]
    async fn session_lock_entry_is_evicted_after_drain() {
        let (state, _queue_path) = test_state();
        let session_id = "evict-session";

        {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "hello", "ws")
                .unwrap();
        }

        let turn = Arc::new(turn::Turn::new());
        let provider = StaticProvider {
            turn: blocking_turn("evict-session"),
        };
        let mut provider_factory = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        drain_session_queue(
            state.clone(),
            session_id.to_string(),
            turn.as_ref(),
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(
            state
                .session_locks
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }
}
