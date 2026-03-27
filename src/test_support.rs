use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use reqwest::Client;
use tokio::sync::Mutex;

use crate::{config, identity, server, skills, store};

fn test_root(prefix: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "autopoiesis_{prefix}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn test_config() -> config::Config {
    config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: None,
        session_name: None,
        operator_key: Some("test-operator-key".to_string()),
        shell_policy: config::ShellPolicy::default(),
        budget: None,
        read: config::ReadToolConfig::default(),
        subscriptions: config::SubscriptionsConfig::default(),
        queue: config::QueueConfig::default(),
        identity_files: identity::t1_identity_files("identity-templates", "silas"),
        skills_dir: PathBuf::from("skills"),
        skills_dir_resolved: PathBuf::from("skills"),
        skills: skills::SkillCatalog::default(),
        agents: config::AgentsConfig::default(),
        models: config::ModelsConfig::default(),
        domains: config::DomainsConfig::default(),
        active_agent: None,
    }
}

pub(crate) fn new_test_store(prefix: &str) -> (store::Store, PathBuf) {
    let root = test_root(prefix);
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let store = store::Store::new(root.join("queue.sqlite")).unwrap();
    (store, root)
}

pub(crate) fn new_test_server_state(prefix: &str) -> (server::ServerState, PathBuf) {
    let root = test_root(prefix);
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let store = store::Store::new(root.join("queue.sqlite")).unwrap();
    let state = server::ServerState::new(
        Arc::new(Mutex::new(store)),
        Arc::new(StdMutex::new(HashMap::new())),
        sessions_dir,
        "mock-api-key".to_string(),
        Some("test-operator-key".to_string()),
        test_config(),
        Client::new(),
    );
    (state, root)
}
