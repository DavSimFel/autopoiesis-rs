use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use reqwest::Client;
use tokio::sync::Mutex;

use crate::{config, session_registry, store};

pub(crate) struct ServerStateInit {
    pub(crate) api_key: String,
    pub(crate) operator_key: Option<String>,
    pub(crate) config: config::Config,
    pub(crate) registry: session_registry::SessionRegistry,
    pub(crate) http_client: Client,
}

#[derive(Clone)]
pub(crate) struct ServerState {
    pub(crate) store: Arc<Mutex<store::Store>>,
    pub(crate) session_locks: Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>,
    pub(crate) sessions_dir: PathBuf,
    pub(crate) api_key: String,
    pub(crate) operator_key: Option<String>,
    pub(crate) config: config::Config,
    pub(crate) registry: session_registry::SessionRegistry,
    pub(crate) always_on_workers: Arc<StdMutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pub(crate) always_on_websocket_counts: Arc<StdMutex<HashMap<String, usize>>>,
    pub(crate) http_client: Client,
}

impl ServerState {
    pub(crate) fn new(
        store: Arc<Mutex<store::Store>>,
        session_locks: Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>,
        sessions_dir: PathBuf,
        init: ServerStateInit,
    ) -> Self {
        Self {
            store,
            session_locks,
            sessions_dir,
            api_key: init.api_key,
            operator_key: init.operator_key,
            config: init.config,
            registry: init.registry,
            always_on_workers: Arc::new(StdMutex::new(HashMap::new())),
            always_on_websocket_counts: Arc::new(StdMutex::new(HashMap::new())),
            http_client: init.http_client,
        }
    }

    pub(crate) fn increment_always_on_websocket_count(&self, session_id: &str) {
        let mut counts = self
            .always_on_websocket_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *counts.entry(session_id.to_string()).or_insert(0) += 1;
    }

    pub(crate) fn decrement_always_on_websocket_count(&self, session_id: &str) {
        let mut counts = self
            .always_on_websocket_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(session_id);
            }
        }
    }

    pub(crate) fn always_on_websocket_count(&self, session_id: &str) -> usize {
        self.always_on_websocket_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }
}
