//! HTTP and WebSocket server for queue-driven agent execution.

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::{
    Router,
    routing::{get, post},
};
use reqwest::Client;

use crate::{config, context::SessionManifest, plan, session_registry::SessionRegistry, store};
use tracing::{info, warn};

mod auth;
mod http;
mod queue;
pub(crate) mod queue_worker;
pub(crate) mod session_lock;
pub(crate) mod state;
mod ws;

pub use http::HttpError;
pub(crate) use state::ServerState;

fn bootstrap_registry_sessions(store: &mut store::Store, registry: &SessionRegistry) -> Result<()> {
    for spec in registry.always_on_sessions() {
        store
            .ensure_session_row(&spec.session_id)
            .with_context(|| format!("failed to bootstrap session {}", spec.session_id))?;
    }
    Ok(())
}

pub async fn run(port: u16) -> Result<()> {
    let config = config::Config::load("agents.toml").context("failed to load configuration")?;
    let api_key = std::env::var("AUTOPOIESIS_API_KEY")
        .context("set AUTOPOIESIS_API_KEY before running serve")?;
    let sessions_dir = std::path::PathBuf::from("sessions");

    let mut store = store::Store::new(sessions_dir.join("queue.sqlite"))
        .context("failed to open session store")?;
    match store.recover_stale_messages(config.queue.stale_processing_timeout_secs) {
        Ok(recovered) if recovered > 0 => {
            info!(recovered, "recovered stale messages from previous crash");
        }
        Ok(_) => {}
        Err(error) => {
            warn!(%error, "failed to recover stale messages");
        }
    }
    match plan::recover_crashed_plans(
        &mut store,
        sessions_dir.as_path(),
        config.queue.stale_processing_timeout_secs,
    ) {
        Ok(recovered) if recovered > 0 => {
            info!(recovered, "recovered crashed plan runs from previous crash");
        }
        Ok(_) => {}
        Err(error) => {
            warn!(%error, "failed to recover crashed plan runs");
        }
    }
    let registry =
        SessionRegistry::from_config(&config).context("failed to build session registry")?;
    bootstrap_registry_sessions(&mut store, &registry)?;
    let state = ServerState::new(
        std::sync::Arc::new(tokio::sync::Mutex::new(store)),
        std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        sessions_dir,
        state::ServerStateInit {
            api_key,
            operator_key: config.operator_key.clone(),
            config,
            registry,
            http_client: Client::new(),
        },
    );
    let session_manifest = SessionManifest::from_registry(&state.registry);
    for spec in state.registry.always_on_sessions() {
        let handle = queue_worker::spawn_background_queue_worker(
            state.clone(),
            spec.session_id.clone(),
            spec.config.clone(),
            Some(session_manifest.clone()),
        );
        state
            .always_on_workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(spec.session_id.clone(), handle);
    }

    let always_on_workers = state.always_on_workers.clone();
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .context("failed to bind server socket")?;
    info!(%port, "server bound");
    let server_result = axum::serve(listener, app)
        .await
        .context("server exited unexpectedly");
    for handle in always_on_workers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(_, handle)| handle)
    {
        handle.abort();
    }
    server_result
}

pub(crate) fn router(state: ServerState) -> Router {
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

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;

    use crate::config::{
        AgentDefinition, AgentTierConfig, Config, DomainsConfig, ModelsConfig, QueueConfig,
        ReadToolConfig, ShellPolicy, SubscriptionsConfig,
    };
    use crate::identity;
    use crate::session_registry::SessionRegistry;
    use crate::skills::SkillCatalog;
    use crate::test_support::new_test_store;
    use std::path::PathBuf;

    fn registry_config() -> Config {
        let mut agents = crate::config::AgentsConfig::default();
        agents.entries.insert(
            "silas".to_string(),
            AgentDefinition {
                identity: Some("silas".to_string()),
                tier: None,
                model: Some("gpt-5.4-mini".to_string()),
                base_url: None,
                system_prompt: None,
                session_name: None,
                reasoning_effort: None,
                t1: AgentTierConfig {
                    model: Some("gpt-5.4-mini".to_string()),
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning: None,
                    reasoning_effort: None,
                    delegation_token_threshold: None,
                    delegation_tool_depth: None,
                },
                t2: AgentTierConfig {
                    model: Some("o3".to_string()),
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
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
            skills_dir: PathBuf::from("skills"),
            skills_dir_resolved: PathBuf::from("skills"),
            skills: SkillCatalog::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    #[test]
    fn bootstrap_registry_sessions_is_idempotent() {
        let (mut store, root) = new_test_store("bootstrap_registry_sessions");
        let config = registry_config();
        let registry = SessionRegistry::from_config(&config).unwrap();

        bootstrap_registry_sessions(&mut store, &registry).unwrap();
        bootstrap_registry_sessions(&mut store, &registry).unwrap();

        assert_eq!(
            store.list_sessions().unwrap(),
            vec!["silas-t1".to_string(), "silas-t2".to_string()]
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
