//! Child session spawning and completion propagation helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::json;

use crate::config::Config;
use crate::llm::{ChatRole, MessageContent};
use crate::principal::Principal;
use crate::session::Session;
use crate::store::Store;

/// Parameters for creating a child session.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub parent_session_id: String,
    pub task: String,
    pub model_override: Option<String>,
    pub reasoning_override: Option<String>,
}

/// Result returned after a child session is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub child_session_id: String,
}

static NEXT_CHILD_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Create a child session and enqueue its initial task in the shared store.
pub fn spawn_child(
    store: &mut Store,
    config: &Config,
    request: SpawnRequest,
) -> Result<SpawnResult> {
    let child_session_id = generate_child_session_id();
    let metadata = json!({
        "parent_session_id": request.parent_session_id,
        "task": request.task,
        "model_override": request.model_override,
        "reasoning_override": request.reasoning_override,
        "active_agent": config.active_agent,
        "default_model": config.model,
    })
    .to_string();

    store
        .create_child_session_with_task(
            &request.parent_session_id,
            &child_session_id,
            Some(metadata.as_str()),
            &request.task,
            &format!("agent-{}", request.parent_session_id),
        )
        .context("failed to create child session")?;

    Ok(SpawnResult { child_session_id })
}

pub(crate) fn enqueue_child_completion(
    store: &mut Store,
    child_session_id: &str,
    session: &Session,
) -> Result<bool> {
    let Some(parent_session_id) = store.get_parent_session(child_session_id)? else {
        return Ok(false);
    };

    let completion = build_completion_message(child_session_id, session);
    store
        .enqueue_message(
            &parent_session_id,
            "user",
            &completion,
            &format!("agent-{child_session_id}"),
        )
        .context("failed to enqueue child completion message")?;

    Ok(true)
}

pub(crate) fn should_enqueue_child_completion(processed_any: bool) -> bool {
    processed_any
}

fn build_completion_message(child_session_id: &str, session: &Session) -> String {
    let response = latest_assistant_response(session)
        .unwrap_or_else(|| "No assistant response was produced.".to_string());

    format!("Child session {child_session_id} completed.\n\n{response}")
}

pub(crate) fn latest_assistant_response(session: &Session) -> Option<String> {
    session.history().iter().rev().find_map(|message| {
        if message.role != ChatRole::Assistant || message.principal != Principal::Agent {
            return None;
        }

        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        Some(text)
    })
}

fn generate_child_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_CHILD_SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let process_id = std::process::id();
    format!("child-session-{nanos}-{process_id}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use crate::llm::{ChatMessage, ChatRole, MessageContent};
    use crate::principal::Principal;

    fn temp_root(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autopoiesis_spawn_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_config() -> Config {
        Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: None,
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            queue: crate::config::QueueConfig::default(),
            identity_files: Vec::new(),
            agents: crate::config::AgentsConfig::default(),
            models: crate::config::ModelsConfig::default(),
            domains: crate::config::DomainsConfig::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    fn assistant_message(text: &str, principal: Principal) -> ChatMessage {
        let mut message =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(principal));
        message.content.push(MessageContent::text(text));
        message
    }

    #[test]
    fn spawn_child_creates_child_session_and_queues_task() {
        let root = temp_root("spawn_child");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect the tree".to_string(),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
        };

        let result = spawn_child(&mut store, &test_config(), request).unwrap();
        assert!(!result.child_session_id.is_empty());
        assert_eq!(
            store.get_parent_session(&result.child_session_id).unwrap(),
            Some("parent".to_string())
        );
        assert_eq!(
            store.list_child_sessions("parent").unwrap(),
            vec![result.child_session_id.clone()]
        );

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let (role, content, source): (String, String, String) = conn
            .query_row(
                "SELECT role, content, source FROM messages WHERE session_id = ?1",
                [&result.child_session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(role, "user");
        assert_eq!(content, "inspect the tree");
        assert!(source.starts_with("agent-"));
        assert_eq!(Principal::from_source(&source), Principal::Agent);

        let metadata: String = conn
            .query_row(
                "SELECT metadata FROM sessions WHERE id = ?1",
                [&result.child_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(metadata.contains(r#""parent_session_id":"parent""#));
        assert!(metadata.contains(r#""model_override":"gpt-child""#));
        assert!(metadata.contains(r#""reasoning_override":"high""#));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_rejects_missing_parent() {
        let root = temp_root("spawn_child_missing_parent");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();

        let request = SpawnRequest {
            parent_session_id: "missing".to_string(),
            task: "do work".to_string(),
            model_override: None,
            reasoning_override: None,
        };

        let err = spawn_child(&mut store, &test_config(), request)
            .expect_err("missing parent should fail");
        assert!(err.to_string().contains("failed to create child session"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enqueue_child_completion_uses_latest_assistant_response() {
        let root = temp_root("child_completion");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();
        store
            .create_child_session("parent", "child", Some(r#"{"task":"demo"}"#))
            .unwrap();

        let mut session = Session::new(&sessions_dir).unwrap();
        session
            .append(assistant_message("primary answer", Principal::Agent), None)
            .unwrap();
        session
            .append(
                ChatMessage::system_with_principal("audit note", Some(Principal::System)),
                None,
            )
            .unwrap();

        assert!(enqueue_child_completion(&mut store, "child", &session).unwrap());

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let (role, content, source): (String, String, String) = conn
            .query_row(
                "SELECT role, content, source FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                ["parent"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(role, "user");
        assert_eq!(source, "agent-child");
        assert!(content.contains("Child session child completed."));
        assert!(content.contains("primary answer"));
        assert!(!content.contains("audit note"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn enqueue_child_completion_uses_fallback_when_no_assistant_response_exists() {
        let root = temp_root("child_completion_fallback");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();
        store
            .create_child_session("parent", "child", Some(r#"{"task":"demo"}"#))
            .unwrap();

        let session = Session::new(&sessions_dir).unwrap();
        assert!(enqueue_child_completion(&mut store, "child", &session).unwrap());

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let content: String = conn
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                ["parent"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(content.contains("No assistant response was produced."));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn should_enqueue_child_completion_requires_processed_messages() {
        assert!(should_enqueue_child_completion(true));
        assert!(!should_enqueue_child_completion(false));
    }

    #[test]
    fn enqueue_child_completion_is_noop_without_parent() {
        let root = temp_root("child_completion_no_parent");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("child", None).unwrap();

        let mut session = Session::new(&sessions_dir).unwrap();
        session
            .append(assistant_message("only response", Principal::Agent), None)
            .unwrap();

        assert!(!enqueue_child_completion(&mut store, "child", &session).unwrap());

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let _ = fs::remove_dir_all(&root);
    }
}
