//! HTTP and WebSocket server for queue-driven agent execution.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::extract::ws::{Message, WebSocket};
use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, Path, State, WebSocketUpgrade},
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};

use crate::auth as root_auth;
use crate::principal::Principal;
use crate::{agent, config, llm, session, store, turn};
use tracing::{error, info, warn};

const API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone)]
pub struct ServerState {
    store: Arc<Mutex<store::Store>>,
    session_locks: Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>,
    sessions_dir: PathBuf,
    api_key: String,
    operator_key: Option<String>,
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
    role: Option<String>,
    content: String,
}

#[derive(Serialize)]
struct EnqueueMessageResponse {
    message_id: i64,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("{0}")]
    BadRequest(&'static str),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl HttpError {
    pub fn bad_request(message: &'static str) -> Self {
        Self::BadRequest(message)
    }

    pub fn unauthorized(message: &'static str) -> Self {
        Self::Unauthorized(message)
    }
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (
                StatusCode::BAD_REQUEST,
                Json(ErrorBody {
                    error: message.to_string(),
                }),
            )
                .into_response(),
            Self::Unauthorized(message) => (
                StatusCode::UNAUTHORIZED,
                Json(ErrorBody {
                    error: message.to_string(),
                }),
            )
                .into_response(),
            Self::Internal(error) => {
                error!(%error, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorBody {
                        error: "internal server error".to_string(),
                    }),
                )
                    .into_response()
            }
        }
    }
}

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

#[tracing::instrument(level = "debug", skip(state, turn, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
async fn drain_session_queue<F, Fut, P, TS, AH>(
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
    let mut processed_any = false;

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
        processed_any = true;

        let outcome = agent::process_queued_message(
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
                        return Ok(Some(agent::TurnVerdict::Denied { reason, gate_id }));
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

    if crate::spawn::should_enqueue_child_completion(processed_any) {
        let mut store = state.store.lock().await;
        let _ = crate::spawn::enqueue_child_completion(&mut store, &session_id, &history)?;
    }

    Ok(None)
}

pub async fn run(port: u16) -> Result<()> {
    let config = config::Config::load("agents.toml").context("failed to load configuration")?;
    let api_key = std::env::var("AUTOPOIESIS_API_KEY")
        .context("set AUTOPOIESIS_API_KEY before running serve")?;

    let mut store =
        store::Store::new("sessions/queue.sqlite").context("failed to open session store")?;
    match store.recover_stale_messages(config.queue.stale_processing_timeout_secs) {
        Ok(recovered) if recovered > 0 => {
            info!(recovered, "recovered stale messages from previous crash");
        }
        Ok(_) => {}
        Err(error) => {
            warn!(%error, "failed to recover stale messages");
        }
    }
    let state = ServerState {
        store: Arc::new(Mutex::new(store)),
        session_locks: Arc::new(StdMutex::new(HashMap::new())),
        sessions_dir: PathBuf::from("sessions"),
        api_key,
        operator_key: config.operator_key.clone(),
        config,
        http_client: Client::new(),
    };

    let app = router(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .context("failed to bind server socket")?;
    info!(%port, "server bound");
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

#[tracing::instrument(level = "debug")]
async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[tracing::instrument(level = "info", skip(state, payload))]
async fn create_session(
    State(state): State<ServerState>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, HttpError> {
    let session_id = generate_session_id();
    let metadata = payload.metadata.unwrap_or_else(|| json!({})).to_string();

    let mut store = state.store.lock().await;
    match store.create_session(&session_id, Some(&metadata)) {
        Ok(()) => Ok((StatusCode::OK, Json(CreateSessionResponse { session_id }))),
        Err(error) => Err(HttpError::Internal(error)),
    }
}

#[tracing::instrument(level = "debug", skip(state))]
async fn list_sessions(State(state): State<ServerState>) -> Result<impl IntoResponse, HttpError> {
    let store = state.store.lock().await;
    match store.list_sessions() {
        Ok(sessions) => Ok(Json(SessionListResponse { sessions })),
        Err(error) => Err(HttpError::Internal(error)),
    }
}

#[tracing::instrument(level = "info", skip(state, payload), fields(session_id = %session_id))]
async fn enqueue_message(
    State(state): State<ServerState>,
    Extension(principal): Extension<Principal>,
    Path(session_id): Path<String>,
    Json(payload): Json<EnqueueMessageRequest>,
) -> Result<impl IntoResponse, HttpError> {
    if !validate_session_id(&session_id) {
        return Err(HttpError::bad_request("invalid session id"));
    }

    let mut store = state.store.lock().await;
    if let Err(error) = store.create_session(&session_id, None) {
        return Err(HttpError::Internal(error));
    }

    let role = principal.role_for_request(payload.role.as_deref());
    let source = principal.source_for_transport("http");
    match store.enqueue_message(&session_id, role, &payload.content, &source) {
        Ok(message_id) => {
            drop(store);
            spawn_http_queue_worker(state.clone(), session_id.clone());
            Ok((StatusCode::OK, Json(EnqueueMessageResponse { message_id })))
        }
        Err(error) => Err(HttpError::Internal(error)),
    }
}

#[tracing::instrument(level = "info", skip(state, ws), fields(session_id = %session_id, principal = ?principal))]
async fn ws_session(
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

        match drain_session_queue(
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

#[tracing::instrument(level = "debug", skip(state, request, next))]
async fn authenticate(
    State(state): State<ServerState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let from_header = request
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|value| value.to_str().ok());
    if let Some(principal) = from_header.and_then(|value| principal_for_token(&state, value)) {
        request.extensions_mut().insert(principal);
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
        if let Some(principal) = from_query.and_then(|value| principal_for_token(&state, value)) {
            request.extensions_mut().insert(principal);
            return next.run(request).await;
        }
    }

    HttpError::unauthorized("missing or invalid api key").into_response()
}

fn principal_for_token(state: &ServerState, token: &str) -> Option<Principal> {
    if state
        .operator_key
        .as_deref()
        .is_some_and(|operator_key| operator_key == token)
    {
        return Some(Principal::Operator);
    }

    (token == state.api_key).then_some(Principal::User)
}

fn spawn_http_queue_worker(state: ServerState, session_id: String) {
    tokio::spawn(async move {
        let turn = turn::build_turn_for_config(&state.config);
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
        match drain_session_queue(
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
        _severity: &crate::gate::Severity,
        _reason: &str,
        _command: &str,
    ) -> bool {
        false
    }
}

fn send_ws_terminal_denial(tx: &mpsc::UnboundedSender<WsFrame>, reason: &str) {
    let _ = tx.send(WsFrame::Error {
        data: reason.to_string(),
    });
    let _ = tx.send(WsFrame::Done);
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

fn severity_label(severity: crate::gate::Severity) -> &'static str {
    match severity {
        crate::gate::Severity::Low => "low",
        crate::gate::Severity::Medium => "medium",
        crate::gate::Severity::High => "high",
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
    use crate::gate::{Guard, GuardEvent, Severity, Verdict};
    use crate::llm::{ChatMessage, FunctionTool, StopReason, StreamedTurn};
    use crate::principal::Principal;

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
                session_locks: Arc::new(StdMutex::new(HashMap::new())),
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
                    queue: config::QueueConfig::default(),
                    identity_files: crate::identity::t1_identity_files(
                        "identity-templates",
                        "silas",
                    ),
                    skills_dir: PathBuf::from("skills"),
                    skills_dir_resolved: PathBuf::from("skills"),
                    skills: crate::skills::SkillCatalog::default(),
                    agents: config::AgentsConfig::default(),
                    models: config::ModelsConfig::default(),
                    domains: config::DomainsConfig::default(),
                    active_agent: None,
                },
                http_client: Client::new(),
            },
            queue_path,
        )
    }

    async fn enqueue_message_via_http(
        app: Router,
        api_key: &str,
        session_id: &str,
        payload: Value,
    ) -> Response {
        app.oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{session_id}/messages"))
                .method("POST")
                .header("content-type", "application/json")
                .header("x-api-key", api_key)
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    async fn response_message_id(response: Response) -> i64 {
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        payload["message_id"]
            .as_i64()
            .expect("response should contain message_id")
    }

    fn load_queued_message(queue_path: &PathBuf, message_id: i64) -> (String, String) {
        let conn = rusqlite::Connection::open(queue_path).unwrap();
        conn.query_row(
            "SELECT role, source FROM messages WHERE id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
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
    async fn enqueue_with_user_api_key_forces_role_to_user() {
        let (state, queue_path) = test_state();
        let app = router(state);

        let response = enqueue_message_via_http(
            app,
            "test-key",
            "role-user-session",
            serde_json::json!({
                "role": "system",
                "content": "injected system prompt",
                "source": "spoofed",
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let message_id = response_message_id(response).await;
        let (role, source) = load_queued_message(&queue_path, message_id);
        assert_eq!(role, "user");
        assert_eq!(source, "http-user");
    }

    #[tokio::test]
    async fn enqueue_with_operator_key_keeps_requested_role() {
        let (state, queue_path) = test_state();
        let app = router(state);

        let response = enqueue_message_via_http(
            app,
            "operator-key",
            "role-operator-session",
            serde_json::json!({
                "role": "system",
                "content": "operator note",
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let message_id = response_message_id(response).await;
        let (role, source) = load_queued_message(&queue_path, message_id);
        assert_eq!(role, "system");
        assert_eq!(source, "http-operator");
    }

    #[tokio::test]
    async fn enqueue_without_role_defaults_to_user() {
        let (state, queue_path) = test_state();
        let app = router(state);

        let response = enqueue_message_via_http(
            app,
            "operator-key",
            "default-role-session",
            serde_json::json!({
                "content": "no explicit role",
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let message_id = response_message_id(response).await;
        let (role, source) = load_queued_message(&queue_path, message_id);
        assert_eq!(role, "user");
        assert_eq!(source, "http-operator");
    }

    #[tokio::test]
    async fn invalid_api_key_returns_unauthorized() {
        let (state, _queue_path) = test_state();
        let app = router(state);

        let response = enqueue_message_via_http(
            app,
            "wrong-key",
            "unauthorized-session",
            serde_json::json!({
                "role": "system",
                "content": "blocked",
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        let mut history = session::Session::new(state.sessions_dir.join(session_id)).unwrap();
        history.load_today().unwrap();
        let mut store = state.store.lock().await;

        assert!(
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
        let mut history = session::Session::new(state.sessions_dir.join(session_id)).unwrap();
        history.load_today().unwrap();
        let mut store = state.store.lock().await;

        let denial = agent::drain_queue(
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
    async fn http_error_bad_request_maps_to_400_with_json_body() {
        let response = HttpError::bad_request("invalid session id").into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            r#"{"error":"invalid session id"}"#
        );
    }

    #[tokio::test]
    async fn http_error_unauthorized_maps_to_401_with_json_body() {
        let response = HttpError::unauthorized("missing or invalid api key").into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            r#"{"error":"missing or invalid api key"}"#
        );
    }

    #[tokio::test]
    async fn http_error_internal_maps_to_500_with_json_body() {
        let response = HttpError::from(anyhow::anyhow!("boom")).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(body.to_vec()).unwrap(),
            r#"{"error":"internal server error"}"#
        );
    }
}
