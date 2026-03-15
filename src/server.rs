//! HTTP and WebSocket server for queue-driven agent execution.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};

use crate::{
    agent, auth, config, context, guard, llm, session, store, tool, turn,
};
use crate::tool::Tool;

const API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone)]
pub struct ServerState {
    store: Arc<Mutex<store::Store>>,
    sessions_dir: PathBuf,
    api_key: String,
    config: config::Config,
    http_client: Client,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    metadata: Option<Value>,
}

#[derive(Serialize)]
struct CreateSessionResponse {
    session_id: String,
}

#[derive(Serialize)]
struct SessionListResponse {
    sessions: Vec<String>,
}

#[derive(Deserialize)]
struct EnqueueMessageRequest {
    role: String,
    content: String,
    source: Option<String>,
}

#[derive(Serialize)]
struct EnqueueMessageResponse {
    message_id: i64,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum WsFrame {
    Token { data: String },
    ToolCall { data: llm::ToolCall },
    Done,
}

pub async fn run(port: u16) -> Result<()> {
    let config = config::Config::load("agents.toml").context("failed to load configuration")?;
    let api_key = std::env::var("AUTOPOIESIS_API_KEY")
        .context("set AUTOPOIESIS_API_KEY before running serve")?;

    let store = store::Store::new("sessions/queue.sqlite").context("failed to open session store")?;
    let state = ServerState {
        store: Arc::new(Mutex::new(store)),
        sessions_dir: PathBuf::from("sessions"),
        api_key,
        config,
        http_client: Client::new(),
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .context("failed to bind server socket")?;
    axum::serve(listener, app).await.context("server exited unexpectedly")
}

pub fn router(state: ServerState) -> Router {
    let middleware_state = state.clone();
    Router::new()
        .route("/api/health", get(health_check))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/:id/messages", post(enqueue_message))
        .route("/api/ws/:session_id", get(ws_session))
        .with_state(state)
        .route_layer(axum::middleware::from_fn_with_state(
            middleware_state.clone(),
            authenticate,
        ))
}

async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn create_session(
    State(state): State<ServerState>,
    Json(payload): Json<CreateSessionRequest>,
) -> impl IntoResponse {
    let session_id = generate_session_id();
    let metadata = payload.metadata.unwrap_or_else(|| json!({})).to_string();

    let mut store = state.store.lock().await;
    match store.create_session(&session_id, Some(&metadata)) {
        Ok(()) => (StatusCode::OK, Json(CreateSessionResponse { session_id })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
            .into_response(),
    }
}

async fn list_sessions(State(state): State<ServerState>) -> impl IntoResponse {
    let store = state.store.lock().await;
    match store.list_sessions() {
        Ok(sessions) => Json(SessionListResponse { sessions }).into_response(),
        Err(error) => {
            (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
        }
    }
}

async fn enqueue_message(
    State(state): State<ServerState>,
    Path(session_id): Path<String>,
    Json(payload): Json<EnqueueMessageRequest>,
) -> impl IntoResponse {
    let mut store = state.store.lock().await;
    if let Err(error) = store.create_session(&session_id, None) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }

    let source = payload.source.unwrap_or_else(|| "http".to_string());
    match store.enqueue_message(&session_id, &payload.role, &payload.content, &source) {
        Ok(message_id) => (
            StatusCode::OK,
            Json(EnqueueMessageResponse { message_id }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
            .into_response(),
    }
}

async fn ws_session(
    State(state): State<ServerState>,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| websocket_session(state, session_id, socket))
}

async fn websocket_session(state: ServerState, session_id: String, socket: WebSocket) {
    let (mut sender, mut _receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<WsFrame>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let payload = match serde_json::to_string(&frame) {
                Ok(payload) => payload,
                Err(error) => {
                    format!(r#"{{\"op\":\"error\",\"data\":\"{error}\"}}"#)
                }
            };

            if sender.send(Message::Text(payload.into())).await.is_err() {
                break;
            }
        }
    });

    let mut history = match session::Session::new(state.sessions_dir.join(&session_id)) {
        Ok(mut session) => {
            let _ = session.load_today();
            session
        }
        Err(_) => {
            let _ = tx.send(WsFrame::Done);
            drop(tx);
            let _ = writer.await;
            return;
        }
    };

    let turn = server_turn(&state.config);
    let mut provider_factory = {
        let client = state.http_client.clone();
        let config = state.config.clone();
        move || {
            let client = client.clone();
            let config = config.clone();
            async move {
                let api_key = auth::get_valid_token().await?;
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

    loop {
        let message = {
            let mut store = state.store.lock().await;
            store.dequeue_next_message(&session_id)
        };

        let message = match message {
            Ok(Some(message)) => message,
            Ok(None) => break,
            Err(error) => {
                let _ = tx.send(WsFrame::Token {
                    data: format!("{{\"error\":\"{error}\"}}"),
                });
                break;
            }
        };

        if message.role != "user" {
            let mut store = state.store.lock().await;
            let _ = store.mark_processed(message.id);
            continue;
        }

        let mut token_sink = WsTokenSink::new(tx.clone());
        let mut approval_handler = WsAutoApprove;

        let verdict = agent::run_agent_loop(
            &mut provider_factory,
            &mut history,
            message.content,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await;

        {
            let mut store = state.store.lock().await;
            let _ = store.mark_processed(message.id);
        }

        match verdict {
            Ok(agent::TurnVerdict::Executed(tool_calls))
            | Ok(agent::TurnVerdict::Approved { tool_calls }) => {
                for tool_call in tool_calls {
                    let _ = tx.send(WsFrame::ToolCall { data: tool_call });
                }
            }
            Ok(agent::TurnVerdict::Denied { .. }) => {}
            Err(error) => {
                let _ = tx.send(WsFrame::Token {
                    data: format!("{{\"error\":\"{error}\"}}"),
                });
                break;
            }
        }
    }

    let _ = tx.send(WsFrame::Done);
    drop(tx);
    let _ = writer.await;
    let _ = _receiver.next().await;
}

fn server_turn(config: &config::Config) -> turn::Turn {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToString::to_string))
        .unwrap_or_else(String::new);
    let tools = vec![tool::Shell::new().definition()];
    let tools_list = tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(",");
    let mut vars = std::collections::HashMap::new();
    vars.insert("model".to_string(), config.model.clone());
    vars.insert("cwd".to_string(), cwd);
    vars.insert("tools".to_string(), tools_list);

    turn::Turn::new()
        .context(context::Identity::new("identity", vars, &config.system_prompt))
        .context(context::History::new(100_000))
        .tool(tool::Shell::new())
        .guard(guard::SecretRedactor::new(&[
            r"sk-[a-zA-Z0-9_-]{20,}",
            r"ghp_[a-zA-Z0-9]{36}",
            r"AKIA[0-9A-Z]{16}",
        ]))
        .guard(guard::ShellSafety::new())
        .guard(guard::ExfilDetector::new())
}

fn generate_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("session-{now}")
}

async fn authenticate(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let provided = request.headers().get(API_KEY_HEADER).and_then(|value| value.to_str().ok());
    if provided.is_some_and(|value| value == state.api_key) {
        return next.run(request).await;
    }

    (StatusCode::UNAUTHORIZED, "missing or invalid api key").into_response()
}

struct WsTokenSink {
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

struct WsAutoApprove;

impl agent::ApprovalHandler for WsAutoApprove {
    fn request_approval(
        &mut self,
        _severity: &crate::guard::Severity,
        _reason: &str,
        _command: &str,
    ) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_state() -> ServerState {
        let path = std::env::temp_dir().join(format!(
            "autopoiesis_server_test_{}.sqlite",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let store = store::Store::new(&path).unwrap();

        ServerState {
            store: Arc::new(Mutex::new(store)),
            sessions_dir: std::env::temp_dir().join("autopoiesis_server_sessions_test"),
            api_key: "test-key".to_string(),
            config: config::Config {
                model: "gpt-test".to_string(),
                system_prompt: "system".to_string(),
                base_url: "https://example.test/api".to_string(),
                reasoning_effort: None,
            },
            http_client: Client::new(),
        }
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let state = test_state();
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .method("GET")
                    .header("x-api-key", "test-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["status"], "ok");
    }
}
