use axum::{
    body::Body,
    extract::State,
    http::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::principal::Principal;

use super::{HttpError, ServerState};

const API_KEY_HEADER: &str = "x-api-key";

#[tracing::instrument(level = "debug", skip(state, request, next))]
pub(super) async fn authenticate(
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
        if let Some(principal) = from_query
            .and_then(percent_decode_component)
            .and_then(|value| principal_for_token(&state, &value))
        {
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

fn percent_decode_component(input: &str) -> Option<String> {
    let mut output = Vec::with_capacity(input.len());
    let mut bytes = input.as_bytes().iter().copied();

    while let Some(byte) = bytes.next() {
        match byte {
            b'%' => {
                let hi = decode_hex_digit(bytes.next()?)?;
                let lo = decode_hex_digit(bytes.next()?)?;
                output.push((hi << 4) | lo);
            }
            b'+' => output.push(b'+'),
            other => output.push(other),
        }
    }

    String::from_utf8(output).ok()
}

fn decode_hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::Extension,
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    use reqwest::Client;
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    use crate::{config, store};

    fn test_state() -> ServerState {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_server_auth_test_{}",
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
                identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
                skills_dir: std::path::PathBuf::from("skills"),
                skills_dir_resolved: std::path::PathBuf::from("skills"),
                skills: crate::skills::SkillCatalog::default(),
                agents: config::AgentsConfig::default(),
                models: config::ModelsConfig::default(),
                domains: config::DomainsConfig::default(),
                active_agent: None,
            },
            http_client: Client::new(),
        }
    }

    async fn whoami(Extension(principal): Extension<Principal>) -> impl IntoResponse {
        principal.source_for_transport("ws")
    }

    #[tokio::test]
    async fn principal_for_token_distinguishes_user_and_operator() {
        let state = test_state();
        assert_eq!(
            principal_for_token(&state, "test-key"),
            Some(Principal::User)
        );
        assert_eq!(
            principal_for_token(&state, "operator-key"),
            Some(Principal::Operator)
        );
        assert_eq!(principal_for_token(&state, "wrong"), None);
    }

    #[tokio::test]
    async fn websocket_query_api_key_is_accepted_by_auth_middleware() {
        let state = test_state();
        let app = Router::new()
            .route("/api/ws/:session_id", get(whoami))
            .with_state(state.clone())
            .route_layer(axum::middleware::from_fn_with_state(state, authenticate));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/ws/session-1?api_key=test-key")
                    .method("GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "ws-user");
    }

    #[tokio::test]
    async fn websocket_query_api_key_supports_percent_encoded_values() {
        let mut state = test_state();
        state.api_key = "key+/:%".to_string();
        let app = Router::new()
            .route("/api/ws/:session_id", get(whoami))
            .with_state(state.clone())
            .route_layer(axum::middleware::from_fn_with_state(state, authenticate));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/ws/session-1?api_key=key%2B%2F%3A%25")
                    .method("GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(body.to_vec()).unwrap(), "ws-user");
    }
}
