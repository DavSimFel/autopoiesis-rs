use std::sync::mpsc as std_mpsc;

use anyhow::{Context, Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use axum::{
    extract::{Extension, Path, State, WebSocketUpgrade},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::warn;

use crate::agent;
use crate::context::SessionManifest;
use crate::gate::Severity;
use crate::principal::Principal;

use super::{HttpError, ServerState, queue_worker, validate_session_id};

#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub(super) enum WsFrame {
    Token { data: String },
    Approval { data: WsApprovalRequest },
    Error { data: String },
    Done,
}

#[derive(Debug, Serialize)]
pub(super) struct WsApprovalRequest {
    pub(super) request_id: u64,
    pub(super) severity: &'static str,
    pub(super) reason: String,
    pub(super) command: String,
}

#[derive(Debug)]
pub(super) struct WsApprovalDecision {
    pub(super) request_id: u64,
    pub(super) approved: bool,
}

#[tracing::instrument(level = "info", skip(state, ws), fields(session_id = %session_id, principal = ?principal))]
pub(super) async fn ws_session(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, HttpError> {
    if !validate_session_id(&session_id) {
        return Err(HttpError::bad_request("invalid session id"));
    }
    Ok(ws.on_upgrade(move |socket| websocket_session(state, session_id, principal, socket)))
}

#[tracing::instrument(level = "debug", skip(state, socket), fields(session_id = %session_id, principal = ?principal))]
async fn websocket_session(
    state: ServerState,
    session_id: String,
    principal: Principal,
    socket: WebSocket,
) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsFrame>();
    let (prompt_tx, mut prompt_rx) = mpsc::unbounded_channel::<String>();
    let (approval_tx, approval_rx) = std_mpsc::channel::<WsApprovalDecision>();

    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let payload = match serde_json::to_string(&frame) {
                Ok(payload) => payload,
                Err(error) => format!(r#"{{"op":"error","data":"{error}"}}"#),
            };

            if sender.send(Message::Text(payload)).await.is_err() {
                break;
            }
        }
    });

    let reader_tx = tx.clone();
    let reader = tokio::spawn(async move {
        while let Some(message) = receiver.next().await {
            let message = match message {
                Ok(Message::Text(text)) => text.to_string(),
                Ok(Message::Close(_)) => break,
                Err(_) => break,
                _ => continue,
            };

            if route_ws_client_message(&message, &prompt_tx, &approval_tx).is_err() {
                let _ = reader_tx.send(WsFrame::Error {
                    data: "invalid websocket frame".to_string(),
                });
            }
        }
    });

    let registry_spec = state.registry.get(&session_id).cloned();
    let session_manifest = registry_spec
        .as_ref()
        .map(|_| SessionManifest::from_registry(&state.registry));
    let always_on = registry_spec.as_ref().is_some_and(|spec| spec.always_on);
    if always_on {
        state.increment_always_on_websocket_count(&session_id);
    }

    {
        let mut store = state.store.lock().await;
        if registry_spec.is_some() {
            if let Err(error) = store.ensure_session_row(&session_id) {
                warn!(%session_id, %error, "failed to ensure websocket session");
            }
        } else if let Err(error) = store.create_session(&session_id, None) {
            warn!(%session_id, %error, "failed to create websocket session");
        }
    }

    let mut approval_handler = WsApprovalHandler::new(tx.clone(), approval_rx);

    while let Some(content) = prompt_rx.recv().await {
        if let Err(error) = handle_ws_prompt(
            WsPromptContext {
                state: state.clone(),
                session_id: session_id.clone(),
                principal,
                registry_spec: registry_spec.clone(),
                session_manifest: session_manifest.clone(),
                tx: tx.clone(),
            },
            content,
            &mut approval_handler,
        )
        .await
        {
            let _ = tx.send(WsFrame::Error {
                data: format!("error: {error}"),
            });
            let _ = tx.send(WsFrame::Done);
        }
    }

    if always_on {
        state.decrement_always_on_websocket_count(&session_id);
    }

    drop(approval_handler);
    drop(tx);
    reader.abort();
    let _ = writer.await;
    let _ = reader.await;
}

struct WsPromptContext {
    state: ServerState,
    session_id: String,
    principal: Principal,
    registry_spec: Option<crate::session_registry::SessionSpec>,
    session_manifest: Option<SessionManifest>,
    tx: mpsc::UnboundedSender<WsFrame>,
}

async fn handle_ws_prompt(
    context: WsPromptContext,
    content: String,
    approval_handler: &mut WsApprovalHandler,
) -> anyhow::Result<()> {
    let session_lock = context.state.session_lock(&context.session_id);
    let _session_guard = session_lock.lock().await;
    {
        let mut store = context.state.store.lock().await;
        let source = context.principal.source_for_transport("ws");
        store
            .enqueue_message(&context.session_id, "user", &content, &source)
            .map_err(|error| anyhow::anyhow!("failed to enqueue websocket message: {error}"))?;
    }

    let drain_config = context
        .registry_spec
        .as_ref()
        .map(|spec| spec.config.clone())
        .unwrap_or_else(|| context.state.config.clone());
    let mut token_sink = WsTokenSink::new(context.tx.clone());
    match queue_worker::drain_session_queue_with_subscriptions_locked(
        context.state,
        context.session_id,
        drain_config,
        context.session_manifest,
        &mut token_sink,
        approval_handler,
    )
    .await
    {
        Ok((Some(verdict), _processed_any)) => match verdict {
            agent::TurnVerdict::Denied { reason, gate_id } => {
                warn!(%gate_id, "websocket turn denied");
                send_ws_terminal_denial(&context.tx, &reason);
            }
            _ => unreachable!("drain_queue only returns denial verdicts"),
        },
        Ok((None, _processed_any)) => {}
        Err(error) => {
            let _ = context.tx.send(WsFrame::Error {
                data: format!("error: {error}"),
            });
        }
    }

    let _ = context.tx.send(WsFrame::Done);
    Ok(())
}

fn route_ws_client_message(
    message: &str,
    prompt_tx: &mpsc::UnboundedSender<String>,
    approval_tx: &std_mpsc::Sender<WsApprovalDecision>,
) -> Result<()> {
    let parsed: Value = serde_json::from_str(message).context("failed to parse message")?;

    if parsed.get("op").and_then(Value::as_str) == Some("approval") {
        let request_id = parsed
            .get("data")
            .and_then(|data| data.get("request_id"))
            .and_then(Value::as_u64)
            .context("approval response missing request_id")?;
        let approved = parsed
            .get("data")
            .and_then(|data| data.get("approved"))
            .and_then(Value::as_bool)
            .context("approval response missing approved")?;
        approval_tx
            .send(WsApprovalDecision {
                request_id,
                approved,
            })
            .context("failed to queue approval response")?;
        return Ok(());
    }

    let content = parsed
        .get("data")
        .and_then(|data| data.get("content"))
        .and_then(Value::as_str)
        .or_else(|| parsed.get("content").and_then(Value::as_str))
        .context("prompt missing content")?;
    prompt_tx
        .send(content.to_string())
        .map_err(|_| anyhow!("failed to queue websocket prompt"))?;
    Ok(())
}

pub(super) struct WsTokenSink {
    tx: mpsc::UnboundedSender<WsFrame>,
}

impl WsTokenSink {
    fn new(tx: mpsc::UnboundedSender<WsFrame>) -> Self {
        Self { tx }
    }
}

impl agent::TokenSink for WsTokenSink {
    fn on_token(&mut self, token: String) {
        let _ = self.tx.send(WsFrame::Token { data: token });
    }
}

pub(super) fn send_ws_terminal_denial(tx: &mpsc::UnboundedSender<WsFrame>, reason: &str) {
    let _ = tx.send(WsFrame::Error {
        data: reason.to_string(),
    });
    let _ = tx.send(WsFrame::Done);
}

pub(super) struct WsApprovalHandler {
    tx: mpsc::UnboundedSender<WsFrame>,
    responses: std_mpsc::Receiver<WsApprovalDecision>,
    next_request_id: u64,
}

impl WsApprovalHandler {
    pub(super) fn new(
        tx: mpsc::UnboundedSender<WsFrame>,
        responses: std_mpsc::Receiver<WsApprovalDecision>,
    ) -> Self {
        Self {
            tx,
            responses,
            next_request_id: 1,
        }
    }

    fn wait_for_response(&self, request_id: u64) -> bool {
        loop {
            match self.responses.recv() {
                Ok(response) if response.request_id == request_id => return response.approved,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    }
}

impl agent::ApprovalHandler for WsApprovalHandler {
    fn request_approval(
        &mut self,
        severity: &crate::gate::Severity,
        reason: &str,
        command: &str,
    ) -> bool {
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        let _ = self.tx.send(WsFrame::Approval {
            data: WsApprovalRequest {
                request_id,
                severity: severity_label(*severity),
                reason: reason.to_string(),
                command: command.to_string(),
            },
        });

        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.wait_for_response(request_id))
        } else {
            self.wait_for_response(request_id)
        }
    }
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Low => "low",
        Severity::Medium => "medium",
        Severity::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentDefinition, AgentTierConfig, Config, DomainsConfig, ModelsConfig, QueueConfig,
        ReadToolConfig, ShellPolicy, SubscriptionsConfig,
    };
    use crate::identity;
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    use crate::agent::ApprovalHandler;
    use crate::gate::Severity;
    use crate::principal::Principal;
    use crate::session_registry::SessionRegistry;
    use crate::test_support::new_test_server_state;

    fn registry_config(base_url: String) -> Config {
        let mut agents = crate::config::AgentsConfig::default();
        agents.entries.insert(
            "silas".to_string(),
            AgentDefinition {
                identity: Some("silas".to_string()),
                tier: None,
                model: Some("gpt-5.4-mini".to_string()),
                base_url: Some(base_url.clone()),
                system_prompt: None,
                session_name: None,
                reasoning_effort: None,
                t1: AgentTierConfig {
                    model: Some("gpt-5.4-mini".to_string()),
                    base_url: Some(base_url.clone()),
                    system_prompt: None,
                    session_name: Some("silas-t1".to_string()),
                    reasoning: None,
                    reasoning_effort: None,
                    delegation_token_threshold: None,
                    delegation_tool_depth: None,
                },
                t2: AgentTierConfig {
                    model: Some("o3".to_string()),
                    base_url: Some(base_url),
                    system_prompt: None,
                    session_name: Some("silas-t2".to_string()),
                    reasoning: None,
                    reasoning_effort: None,
                    delegation_token_threshold: None,
                    delegation_tool_depth: None,
                },
            },
        );

        Config {
            model: "gpt-5.4-mini".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: None,
            session_name: None,
            operator_key: None,
            shell_policy: ShellPolicy::default(),
            budget: None,
            read: ReadToolConfig::default(),
            subscriptions: SubscriptionsConfig::default(),
            queue: QueueConfig::default(),
            identity_files: identity::t1_identity_files("identity-templates", "silas"),
            agents,
            models: ModelsConfig::default(),
            domains: DomainsConfig::default(),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    #[tokio::test]
    async fn ws_approval_handler_waits_for_client_response() {
        let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
        let (approval_tx, approval_rx) = std_mpsc::channel::<WsApprovalDecision>();
        let handle = std::thread::spawn(move || {
            let mut handler = WsApprovalHandler::new(frame_tx, approval_rx);
            handler.request_approval(&Severity::High, "risky", "rm -rf /tmp/demo")
        });

        let frame = frame_rx.recv().await.unwrap();
        let request_id = match frame {
            WsFrame::Approval { data } => {
                assert_eq!(data.severity, "high");
                assert_eq!(data.reason, "risky");
                assert_eq!(data.command, "rm -rf /tmp/demo");
                data.request_id
            }
            _ => panic!("expected approval frame"),
        };
        approval_tx
            .send(WsApprovalDecision {
                request_id,
                approved: false,
            })
            .unwrap();

        assert!(!handle.join().unwrap());
    }

    #[tokio::test]
    async fn ws_terminal_denial_emits_error_then_done() {
        let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
        send_ws_terminal_denial(&frame_tx, "denied by policy");

        match frame_rx.recv().await.unwrap() {
            WsFrame::Error { data } => {
                assert_eq!(data, "denied by policy");
            }
            other => panic!("expected error frame, got {other:?}"),
        }

        assert!(matches!(frame_rx.recv().await.unwrap(), WsFrame::Done));
    }

    #[tokio::test]
    async fn websocket_always_on_prompt_drains_inline_while_worker_is_paused() {
        let (state, root) = new_test_server_state("ws_queue_lock_regression");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut state = state;
        let registry =
            SessionRegistry::from_config(&registry_config(format!("http://{}", addr))).unwrap();
        let session_id = registry.sessions()[0].session_id.clone();
        state.registry = registry;
        let registry_spec = state.registry.get(&session_id).cloned();

        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(|| async {
                    (
                        [
                            ("content-type", "text/event-stream"),
                            ("cache-control", "no-cache"),
                        ],
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n\
                         data: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-5.4-mini\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
                    )
                }),
            )
            .route("/api/ws/:session_id", axum::routing::get(super::ws_session))
            .layer(axum::extract::Extension(Principal::Operator))
            .with_state(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        {
            let mut store = state.store.lock().await;
            store.ensure_session_row(&session_id).unwrap();
        }

        let session_manifest = Some(SessionManifest::from_registry(&state.registry));
        let background_worker = super::queue_worker::spawn_background_queue_worker(
            state.clone(),
            session_id.clone(),
            registry_spec
                .as_ref()
                .expect("test registry should include the requested session")
                .config
                .clone(),
            session_manifest.clone(),
        );
        state
            .always_on_workers
            .lock()
            .expect("always-on worker map should be available")
            .insert(session_id.clone(), background_worker);

        let ws_url = format!("ws://127.0.0.1:{}/api/ws/{}", addr.port(), session_id);
        let (mut websocket, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .expect("websocket should connect");
        websocket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                r#"{"content":"hello world"}"#.to_string(),
            ))
            .await
            .unwrap();

        let mut saw_done = false;
        while let Some(frame) = timeout(Duration::from_secs(1), websocket.next())
            .await
            .unwrap()
        {
            let frame = frame.expect("websocket frame should arrive cleanly");
            match frame {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    if text.contains(r#""op":"done""#) {
                        saw_done = true;
                        break;
                    }
                }
                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                _ => {}
            }
        }
        assert!(saw_done, "prompt should finish with a Done frame");

        drop(websocket);
        if let Some(handle) = state
            .always_on_workers
            .lock()
            .expect("always-on worker map should be available")
            .remove(&session_id)
        {
            handle.abort();
        }
        server.abort();
        let _ = std::fs::remove_dir_all(root);
    }
}
