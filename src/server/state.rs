use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use reqwest::Client;
use tokio::sync::Mutex;

use crate::{config, store};

#[derive(Clone)]
pub(crate) struct ServerState {
    pub(crate) store: Arc<Mutex<store::Store>>,
    pub(crate) session_locks: Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>,
    pub(crate) sessions_dir: PathBuf,
    pub(crate) api_key: String,
    pub(crate) operator_key: Option<String>,
    pub(crate) config: config::Config,
    pub(crate) http_client: Client,
}

impl ServerState {
    pub(crate) fn new(
        store: Arc<Mutex<store::Store>>,
        session_locks: Arc<StdMutex<HashMap<String, Arc<Mutex<()>>>>>,
        sessions_dir: PathBuf,
        api_key: String,
        operator_key: Option<String>,
        config: config::Config,
        http_client: Client,
    ) -> Self {
        Self {
            store,
            session_locks,
            sessions_dir,
            api_key,
            operator_key,
            config,
            http_client,
        }
    }
}
