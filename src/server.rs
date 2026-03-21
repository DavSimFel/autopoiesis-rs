//! HTTP and WebSocket server for queue-driven agent execution.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State, WebSocketUpgrade},
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::{agent, auth, config, llm, session, store, turn};

const API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone)]
pub struct ServerState {
    store: Arc<Mutex<store::Store>>,
    worker_lock: Arc<Mutex<()>>,
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
    Approval { data: WsApprovalRequest },
    Error { data: String },
    Done,
}

#[derive(Debug, Serialize)]
struct WsApprovalRequest {
    request_id: u64,
    severity: &'static str,
    reason: String,
    command: String,
}

#[derive(Debug)]
struct WsApprovalDecision {
    request_id: u64,
    approved: bool,
}

pub async fn run(port: u16) -> Result<()> {
    let config = config::Config::load("agents.toml").context("failed to load configuration")?;
    let api_key = std::env::var("AUTOPOIESIS_API_KEY")
        .context("set AUTOPOIESIS_API_KEY before running serve")?;

    let mut store =
        store::Store::new("sessions/queue.sqlite").context("failed to open session store")?;
    match store.recover_stale_messages() {
        Ok(recovered) if recovered > 0 => {
            eprintln!("recovered {recovered} stale messages from previous crash");
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("warning: failed to recover stale messages: {error}");
        }
    }
    let state = ServerState {
        store: Arc::new(Mutex::new(store)),
        worker_lock: Arc::new(Mutex::new(())),
        sessions_dir: PathBuf::from("sessions"),
        api_key,
        config,
        http_client: Client::new(),
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .context("failed to bind server socket")?;
    axum::serve(listener, app)
        .await
        .context("server exited unexpectedly")
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
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn list_sessions(State(state): State<ServerState>) -> impl IntoResponse {
    let store = state.store.lock().await;
    match store.list_sessions() {
        Ok(sessions) => Json(SessionListResponse { sessions }).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn enqueue_message(
    State(state): State<ServerState>,
    Path(session_id): Path<String>,
    Json(payload): Json<EnqueueMessageRequest>,
) -> impl IntoResponse {
    if !validate_session_id(&session_id) {
        return (StatusCode::BAD_REQUEST, "invalid session id").into_response();
    }

    let mut store = state.store.lock().await;
    if let Err(error) = store.create_session(&session_id, None) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }

    let source = payload.source.unwrap_or_else(|| "http".to_string());
    match store.enqueue_message(&session_id, &payload.role, &payload.content, &source) {
        Ok(message_id) => {
            drop(store);
            spawn_http_queue_worker(state.clone(), session_id.clone());
            (StatusCode::OK, Json(EnqueueMessageResponse { message_id })).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn ws_session(
    State(state): State<ServerState>,
    Path(session_id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if !validate_session_id(&session_id) {
        return (StatusCode::BAD_REQUEST, "invalid session id").into_response();
    }
    ws.on_upgrade(move |socket| websocket_session(state, session_id, socket))
        .into_response()
}

async fn websocket_session(state: ServerState, session_id: String, socket: WebSocket) {
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
        let _ = store.create_session(&session_id, None);
    }

    let turn = turn::build_default_turn(&state.config);
    let mut approval_handler = WsApprovalHandler::new(tx.clone(), approval_rx);

    while let Some(content) = prompt_rx.recv().await {
        {
            let mut store = state.store.lock().await;
            match store.enqueue_message(&session_id, "user", &content, "ws") {
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

        let _worker_guard = state.worker_lock.lock().await;
        let mut history = match session::Session::new(state.sessions_dir.join(&session_id))
            .with_context(|| format!("failed to open session {session_id}"))
        {
            Ok(history) => history,
            Err(error) => {
                let _ = tx.send(WsFrame::Error {
                    data: format!("error: {error}"),
                });
                let _ = tx.send(WsFrame::Done);
                continue;
            }
        };
        if let Err(error) = history.load_today() {
            let _ = tx.send(WsFrame::Error {
                data: format!("error: {error}"),
            });
            let _ = tx.send(WsFrame::Done);
            continue;
        }

        let mut store = state.store.lock().await;
        if let Err(error) = agent::drain_queue(
            &mut store,
            &session_id,
            &mut history,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            let _ = tx.send(WsFrame::Error {
                data: format!("error: {error}"),
            });
        }

        let _ = tx.send(WsFrame::Done);
    }

    drop(tx);
    let _ = writer.await;
    let _ = reader.await;
}

fn generate_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("session-{now}")
}

/// Reject session IDs containing path traversal or unsafe characters.
fn validate_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn authenticate(
    State(state): State<ServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let from_header = request
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|value| value.to_str().ok());
    if from_header.is_some_and(|value| value == state.api_key) {
        return next.run(request).await;
    }

    let is_ws_path = request.uri().path().contains("/api/ws/");
    if is_ws_path {
        let from_query = request.uri().query().and_then(|q| {
            q.split('&').find_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                if key == "api_key" { Some(value) } else { None }
            })
        });
        if from_query.is_some_and(|value| value == state.api_key) {
            return next.run(request).await;
        }
    }

    (StatusCode::UNAUTHORIZED, "missing or invalid api key").into_response()
}

fn spawn_http_queue_worker(state: ServerState, session_id: String) {
    tokio::spawn(async move {
        let turn = turn::build_default_turn(&state.config);
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
        let mut token_sink = NoopTokenSink;
        let mut approval_handler = RejectApprovalHandler;
        let _worker_guard = state.worker_lock.lock().await;
        let mut history = match session::Session::new(state.sessions_dir.join(&session_id))
            .with_context(|| format!("failed to open session {session_id}"))
        {
            Ok(history) => history,
            Err(error) => {
                eprintln!("failed to drain queued HTTP messages for {session_id}: {error}");
                return;
            }
        };
        if let Err(error) = history.load_today() {
            eprintln!("failed to drain queued HTTP messages for {session_id}: {error}");
            return;
        }
        let mut store = state.store.lock().await;
        if let Err(error) = agent::drain_queue(
            &mut store,
            &session_id,
            &mut history,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            eprintln!("failed to drain queued HTTP messages for {session_id}: {error}");
        }
    });
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

struct NoopTokenSink;

impl agent::TokenSink for NoopTokenSink {
    fn on_token(&mut self, _token: String) {}
}

struct RejectApprovalHandler;

impl agent::ApprovalHandler for RejectApprovalHandler {
    fn request_approval(
        &mut self,
        _severity: &crate::guard::Severity,
        _reason: &str,
        _command: &str,
    ) -> bool {
        false
    }
}

struct WsApprovalHandler {
    tx: mpsc::UnboundedSender<WsFrame>,
    responses: std_mpsc::Receiver<WsApprovalDecision>,
    next_request_id: u64,
}

impl WsApprovalHandler {
    fn new(
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
        severity: &crate::guard::Severity,
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

fn severity_label(severity: crate::guard::Severity) -> &'static str {
    match severity {
        crate::guard::Severity::Low => "low",
        crate::guard::Severity::Medium => "medium",
        crate::guard::Severity::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    use crate::agent::ApprovalHandler;
    use crate::guard::{Severity, Verdict};
    use crate::llm::{ChatMessage, FunctionTool, StopReason, StreamedTurn};

    fn test_state() -> (ServerState, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_server_test_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
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
                store: Arc::new(Mutex::new(store)),
                worker_lock: Arc::new(Mutex::new(())),
                sessions_dir,
                api_key: "test-key".to_string(),
                config: config::Config {
                    model: "gpt-test".to_string(),
                    system_prompt: "system".to_string(),
                    base_url: "https://example.test/api".to_string(),
                    reasoning_effort: None,
                },
                http_client: Client::new(),
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

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let (state, _queue_path) = test_state();
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
        let mut history = session::Session::new(state.sessions_dir.join(session_id)).unwrap();
        history.load_today().unwrap();
        let mut store = state.store.lock().await;

        agent::drain_queue(
            &mut store,
            session_id,
            &mut history,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

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
    async fn drain_queue_uses_supplied_approval_handler() {
        let (state, _queue_path) = test_state();
        let session_id = "approval-session";
        {
            let mut store = state.store.lock().await;
            store.create_session(session_id, None).unwrap();
            store
                .enqueue_message(session_id, "user", "run risky command", "ws")
                .unwrap();
        }

        struct NeedsApproval;

        impl crate::guard::Guard for NeedsApproval {
            fn name(&self) -> &str {
                "needs-approval"
            }

            fn check(&self, event: &mut crate::guard::GuardEvent) -> Verdict {
                match event {
                    crate::guard::GuardEvent::ToolCall(_) => Verdict::Approve {
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
        let mut history = session::Session::new(state.sessions_dir.join(session_id)).unwrap();
        history.load_today().unwrap();
        let mut store = state.store.lock().await;

        agent::drain_queue(
            &mut store,
            session_id,
            &mut history,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(
            *approvals.lock().expect("approval counter mutex poisoned"),
            1
        );
    }

    #[tokio::test]
    async fn ws_approval_handler_waits_for_client_response() {
        let (frame_tx, mut frame_rx) = mpsc::unbounded_channel();
        let (approval_tx, approval_rx) = std_mpsc::channel();
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
}
