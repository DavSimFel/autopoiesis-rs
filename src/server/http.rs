use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::principal::Principal;

use super::queue::spawn_http_queue_worker;
use super::{ServerState, generate_session_id, validate_session_id};

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
}

#[derive(Deserialize)]
pub(super) struct CreateSessionRequest {
    metadata: Option<Value>,
}

#[derive(Serialize)]
pub(super) struct CreateSessionResponse {
    session_id: String,
}

#[derive(Serialize)]
pub(super) struct SessionListResponse {
    sessions: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct EnqueueMessageRequest {
    role: Option<String>,
    content: String,
}

#[derive(Serialize)]
pub(super) struct EnqueueMessageResponse {
    message_id: i64,
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
                tracing::error!(%error, "internal server error");
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

#[tracing::instrument(level = "debug")]
pub(super) async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[tracing::instrument(level = "info", skip(state, payload))]
pub(super) async fn create_session(
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
pub(super) async fn list_sessions(
    State(state): State<ServerState>,
) -> Result<impl IntoResponse, HttpError> {
    let store = state.store.lock().await;
    match store.list_sessions() {
        Ok(sessions) => Ok(Json(SessionListResponse { sessions })),
        Err(error) => Err(HttpError::Internal(error)),
    }
}

#[tracing::instrument(level = "info", skip(state, payload), fields(session_id = %session_id))]
pub(super) async fn enqueue_message(
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::{IntoResponse, Response};
    use reqwest::Client;
    use serde_json::Value;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    use crate::{config, store};

    fn test_state() -> (ServerState, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_server_http_test_{}",
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
                store: std::sync::Arc::new(Mutex::new(store)),
                session_locks: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::HashMap::new(),
                )),
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
                    skills_dir: std::path::PathBuf::from("skills"),
                    skills_dir_resolved: std::path::PathBuf::from("skills"),
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
        app: axum::Router,
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

    fn load_queued_message(queue_path: &std::path::PathBuf, message_id: i64) -> (String, String) {
        let conn = rusqlite::Connection::open(queue_path).unwrap();
        conn.query_row(
            "SELECT role, source FROM messages WHERE id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn health_endpoint_returns_ok() {
        let (state, _queue_path) = test_state();
        let app = crate::server::router(state);

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
    async fn create_session_persists_metadata_and_returns_session_id() {
        let (state, queue_path) = test_state();
        let app = crate::server::router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(
                        serde_json::json!({
                            "metadata": {
                                "topic": "notes",
                                "priority": 3
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = payload["session_id"]
            .as_str()
            .expect("response should contain session_id");
        assert!(validate_session_id(session_id));

        let conn = rusqlite::Connection::open(queue_path).unwrap();
        let metadata: String = conn
            .query_row(
                "SELECT metadata FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&metadata).unwrap(),
            serde_json::json!({
                "topic": "notes",
                "priority": 3
            })
        );
    }

    #[tokio::test]
    async fn list_sessions_includes_created_session() {
        let (state, _queue_path) = test_state();
        let app = crate::server::router(state);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
                    .method("POST")
                    .header("content-type", "application/json")
                    .header("x-api-key", "test-key")
                    .body(Body::from(r#"{"metadata":{"label":"list-test"}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(create_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let session_id = payload["session_id"]
            .as_str()
            .expect("response should contain session_id")
            .to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/sessions")
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
        let sessions = payload["sessions"]
            .as_array()
            .expect("response should contain sessions array");
        assert!(
            sessions
                .iter()
                .any(|session| session.as_str() == Some(&session_id)),
            "created session should appear in session list"
        );
    }

    #[tokio::test]
    async fn enqueue_with_user_api_key_forces_role_to_user() {
        let (state, queue_path) = test_state();
        let app = crate::server::router(state);

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
        let app = crate::server::router(state);

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
        let app = crate::server::router(state);

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
        let app = crate::server::router(state);

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
