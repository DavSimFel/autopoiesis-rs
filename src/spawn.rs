//! Child session spawning and completion propagation helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::json;

use crate::config::Config;
use crate::gate::BudgetSnapshot;
use crate::llm::{ChatRole, MessageContent};
use crate::model_selection::ModelSelector;
use crate::principal::Principal;
use crate::session::Session;
use crate::store::Store;

/// Parameters for creating a child session.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub parent_session_id: String,
    pub task: String,
    pub task_kind: Option<String>,
    pub model_override: Option<String>,
    pub reasoning_override: Option<String>,
}

/// Result returned after a child session is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub child_session_id: String,
    pub resolved_model: String,
}

static NEXT_CHILD_SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn validate_spawn_budget(config: &Config, budget: BudgetSnapshot) -> Result<()> {
    let Some(limits) = &config.budget else {
        return Ok(());
    };

    if let Some(limit) = limits.max_tokens_per_session
        && budget.session_tokens >= limit
    {
        return Err(anyhow::anyhow!(
            "spawn rejected: session token ceiling exceeded before spawn (observed {}, limit {})",
            budget.session_tokens,
            limit
        ));
    }

    if let Some(limit) = limits.max_tokens_per_day
        && budget.day_tokens >= limit
    {
        return Err(anyhow::anyhow!(
            "spawn rejected: day token ceiling exceeded before spawn (observed {}, limit {})",
            budget.day_tokens,
            limit
        ));
    }

    Ok(())
}

fn resolve_model<'a>(
    config: &'a Config,
    request: &'a SpawnRequest,
) -> Result<crate::model_selection::SelectedModel<'a>> {
    if let Some(model_key) = request.model_override.as_deref() {
        let definition =
            config.models.catalog.get(model_key).ok_or_else(|| {
                anyhow::anyhow!("model override not found in catalog: {model_key}")
            })?;
        if definition.enabled != Some(true) {
            return Err(anyhow::anyhow!("model override is disabled: {model_key}"));
        }
        return Ok(crate::model_selection::SelectedModel {
            key: model_key,
            definition,
        });
    }

    ModelSelector::new(&config.models).select_model(request.task_kind.as_deref())
}

/// Create a child session and enqueue its initial task in the shared store.
pub fn spawn_child(
    store: &mut Store,
    config: &Config,
    parent_budget: BudgetSnapshot,
    request: SpawnRequest,
) -> Result<SpawnResult> {
    validate_spawn_budget(config, parent_budget)?;
    let selected_model = resolve_model(config, &request)?;
    let child_session_id = generate_child_session_id();
    let metadata = json!({
        "parent_session_id": request.parent_session_id,
        "task": request.task,
        "task_kind": request.task_kind,
        "model_override": request.model_override,
        "reasoning_override": request.reasoning_override,
        "active_agent": config.active_agent,
        "default_model": config.models.default.clone(),
        "resolved_model": selected_model.key,
        "resolved_provider_model": selected_model.definition.model,
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

    Ok(SpawnResult {
        child_session_id,
        resolved_model: selected_model.key.to_string(),
    })
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
        let mut models = crate::config::ModelsConfig::default();
        models.default = Some("gpt-default".to_string());
        models.catalog.insert(
            "gpt-default".to_string(),
            crate::config::ModelDefinition {
                provider: "openai".to_string(),
                model: "gpt-default".to_string(),
                caps: vec!["reasoning".to_string()],
                context_window: Some(128_000),
                cost_tier: Some("cheap".to_string()),
                cost_unit: Some(1),
                enabled: Some(true),
            },
        );
        models.catalog.insert(
            "gpt-child".to_string(),
            crate::config::ModelDefinition {
                provider: "openai".to_string(),
                model: "gpt-child".to_string(),
                caps: vec!["code_review".to_string()],
                context_window: Some(128_000),
                cost_tier: Some("medium".to_string()),
                cost_unit: Some(2),
                enabled: Some(true),
            },
        );
        models.routes.insert(
            "code_review".to_string(),
            crate::config::ModelRoute {
                requires: vec!["code_review".to_string()],
                prefer: vec!["gpt-child".to_string()],
            },
        );

        Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: None,
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: Vec::new(),
            agents: crate::config::AgentsConfig::default(),
            models,
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
            task_kind: Some("code_review".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
        };

        let result = spawn_child(
            &mut store,
            &test_config(),
            BudgetSnapshot::default(),
            request,
        )
        .unwrap();
        assert!(!result.child_session_id.is_empty());
        assert_eq!(result.resolved_model, "gpt-child");
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
        assert!(metadata.contains(r#""task_kind":"code_review""#));
        assert!(metadata.contains(r#""model_override":"gpt-child""#));
        assert!(metadata.contains(r#""reasoning_override":"high""#));
        assert!(metadata.contains(r#""resolved_model":"gpt-child""#));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_uses_default_model_when_task_kind_is_missing() {
        let root = temp_root("spawn_child_default_model");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect the tree".to_string(),
            task_kind: None,
            model_override: None,
            reasoning_override: None,
        };

        let result = spawn_child(
            &mut store,
            &test_config(),
            BudgetSnapshot::default(),
            request,
        )
        .unwrap();
        assert_eq!(result.resolved_model, "gpt-default");

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let metadata: String = conn
            .query_row(
                "SELECT metadata FROM sessions WHERE id = ?1",
                [&result.child_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(metadata.contains(r#""resolved_model":"gpt-default""#));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_rejects_missing_model_override_before_creation() {
        let root = temp_root("spawn_child_missing_model_override");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect the tree".to_string(),
            task_kind: Some("code_review".to_string()),
            model_override: Some("missing".to_string()),
            reasoning_override: None,
        };

        let err = spawn_child(
            &mut store,
            &test_config(),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("missing model override should fail");
        assert!(
            err.to_string()
                .contains("model override not found in catalog")
        );

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id != 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_rejects_disabled_model_override_before_creation() {
        let root = temp_root("spawn_child_disabled_model_override");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let mut config = test_config();
        config.models.catalog.get_mut("gpt-child").unwrap().enabled = Some(false);

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect the tree".to_string(),
            task_kind: Some("code_review".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
        };

        let err = spawn_child(&mut store, &config, BudgetSnapshot::default(), request)
            .expect_err("disabled model override should fail");
        assert!(err.to_string().contains("model override is disabled"));

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id != 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

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
            task_kind: None,
            model_override: None,
            reasoning_override: None,
        };

        let err = spawn_child(
            &mut store,
            &test_config(),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("missing parent should fail");
        assert!(err.to_string().contains("failed to create child session"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_rejects_exhausted_session_budget_before_creation() {
        let root = temp_root("spawn_child_session_budget");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let mut config = test_config();
        config.budget = Some(crate::config::BudgetConfig {
            max_tokens_per_turn: None,
            max_tokens_per_session: Some(10),
            max_tokens_per_day: None,
        });

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            model_override: None,
            reasoning_override: None,
        };

        let err = spawn_child(
            &mut store,
            &config,
            BudgetSnapshot {
                turn_tokens: 0,
                session_tokens: 10,
                day_tokens: 0,
            },
            request,
        )
        .expect_err("exhausted session budget should fail");
        assert!(
            err.to_string()
                .contains("session token ceiling exceeded before spawn")
        );

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id != 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_rejects_exhausted_day_budget_before_creation() {
        let root = temp_root("spawn_child_day_budget");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let mut config = test_config();
        config.budget = Some(crate::config::BudgetConfig {
            max_tokens_per_turn: None,
            max_tokens_per_session: None,
            max_tokens_per_day: Some(10),
        });

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            model_override: None,
            reasoning_override: None,
        };

        let err = spawn_child(
            &mut store,
            &config,
            BudgetSnapshot {
                turn_tokens: 0,
                session_tokens: 0,
                day_tokens: 10,
            },
            request,
        )
        .expect_err("exhausted day budget should fail");
        assert!(
            err.to_string()
                .contains("day token ceiling exceeded before spawn")
        );

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id != 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_child_ignores_turn_budget_for_pre_spawn_gate() {
        let root = temp_root("spawn_child_turn_budget");
        let queue_path = root.join("queue.sqlite");
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let mut config = test_config();
        config.budget = Some(crate::config::BudgetConfig {
            max_tokens_per_turn: Some(1),
            max_tokens_per_session: Some(1_000),
            max_tokens_per_day: Some(1_000),
        });

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            model_override: None,
            reasoning_override: None,
        };

        let result = spawn_child(
            &mut store,
            &config,
            BudgetSnapshot {
                turn_tokens: 999,
                session_tokens: 0,
                day_tokens: 0,
            },
            request,
        )
        .expect("turn budget is ignored for spawn preflight");
        assert_eq!(result.resolved_model, "gpt-child");

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE id != 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

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
