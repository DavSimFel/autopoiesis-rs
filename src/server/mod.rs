//! HTTP and WebSocket server for queue-driven agent execution.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use reqwest::Client;
use tokio::sync::Mutex;

use crate::{config, store};
use tracing::{info, warn};

mod auth;
mod http;
mod queue;
mod ws;

pub use http::HttpError;

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
        .route("/api/health", get(http::health_check))
        .route("/api/sessions", post(http::create_session))
        .route("/api/sessions", get(http::list_sessions))
        .route("/api/sessions/:id/messages", post(http::enqueue_message))
        .route("/api/ws/:session_id", get(ws::ws_session))
        .with_state(state)
        .route_layer(axum::middleware::from_fn_with_state(
            middleware_state.clone(),
            auth::authenticate,
        ))
}

pub(super) fn generate_session_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("session-{now}")
}

/// Reject session IDs containing path traversal or unsafe characters.
pub(super) fn validate_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
