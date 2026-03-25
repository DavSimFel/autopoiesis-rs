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

use crate::auth as root_auth;
use crate::gate::Severity;
use crate::principal::Principal;
use crate::{agent, llm, turn};

use super::{HttpError, ServerState, queue, validate_session_id};

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

    {
        let mut store = state.store.lock().await;
        if let Err(error) = store.create_session(&session_id, None) {
            warn!(%session_id, %error, "failed to create websocket session");
        }
    }

    let turn = turn::build_turn_for_config(&state.config);
    let mut approval_handler = WsApprovalHandler::new(tx.clone(), approval_rx);

    while let Some(content) = prompt_rx.recv().await {
        {
            let mut store = state.store.lock().await;
            let source = principal.source_for_transport("ws");
            match store.enqueue_message(&session_id, "user", &content, &source) {
                Ok(_) => {}
                Err(error) => {
                    let _ = tx.send(WsFrame::Error {
                        data: format!("failed to enqueue websocket message: {error}"),
                    });
                    let _ = tx.send(WsFrame::Done);
                    continue;
                }
            }
        }

        let mut token_sink = WsTokenSink::new(tx.clone());
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

        match queue::drain_session_queue(
            state.clone(),
            session_id.clone(),
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            Ok(Some(verdict)) => match verdict {
                agent::TurnVerdict::Denied { reason, gate_id } => {
                    warn!(%gate_id, "websocket turn denied");
                    send_ws_terminal_denial(&tx, &reason);
                    break;
                }
                _ => unreachable!("drain_queue only returns denial verdicts"),
            },
            Ok(None) => {}
            Err(error) => {
                let _ = tx.send(WsFrame::Error {
                    data: format!("error: {error}"),
                });
            }
        }

        let _ = tx.send(WsFrame::Done);
    }

    drop(approval_handler);
    drop(tx);
    reader.abort();
    let _ = writer.await;
    let _ = reader.await;
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
    use std::sync::mpsc as std_mpsc;
    use tokio::sync::mpsc;

    use crate::agent::ApprovalHandler;
    use crate::gate::Severity;

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
}
