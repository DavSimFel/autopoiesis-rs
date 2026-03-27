//! Child session spawning and completion propagation helpers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::gate::BudgetSnapshot;
use crate::model_selection::ModelSelector;
use crate::skills::{SkillCatalog, SkillDefinition};
use crate::store::Store;

/// Parameters for creating a child session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub parent_session_id: String,
    pub task: String,
    pub task_kind: Option<String>,
    pub tier: Option<String>,
    pub model_override: Option<String>,
    pub reasoning_override: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub skill_token_budget: Option<u64>,
}

/// Result returned after a child session is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnResult {
    pub child_session_id: String,
    pub resolved_model: String,
}

/// Result returned after a spawned child drains to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnDrainResult {
    pub child_session_id: String,
    pub resolved_model: String,
    pub last_assistant_response: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ChildSessionMetadata {
    pub(crate) parent_session_id: String,
    pub(crate) task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) task_kind: Option<String>,
    pub(crate) tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_override: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) active_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) default_model: Option<String>,
    pub(crate) resolved_model: String,
    pub(crate) resolved_provider_model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) skills: Vec<SkillDefinition>,
}

pub(crate) fn parse_child_session_metadata(metadata: &str) -> Result<ChildSessionMetadata> {
    serde_json::from_str(metadata).context("failed to parse spawned child metadata")
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

fn validate_tier(tier: &str) -> Result<()> {
    match tier {
        "t1" | "t2" | "t3" => Ok(()),
        other => Err(anyhow::anyhow!("invalid child tier: {other}")),
    }
}

fn resolve_spawn_tier(config: &Config, request: &SpawnRequest) -> Result<String> {
    let tier = if let Some(tier) = request.tier.as_deref() {
        tier
    } else {
        config
            .active_agent_definition()
            .and_then(|agent| agent.tier.as_deref())
            .unwrap_or("t1")
    };
    validate_tier(tier)?;
    Ok(tier.to_string())
}

fn validate_requested_skills(
    config: &Config,
    resolved_tier: &str,
    selected_model: &crate::model_selection::SelectedModel<'_>,
    request: &SpawnRequest,
) -> Result<Vec<SkillDefinition>> {
    if !request.skills.is_empty() && resolved_tier != "t3" {
        return Err(anyhow::anyhow!(
            "skills may only be loaded for spawned t3 children"
        ));
    }

    let resolved_skills = config
        .skills
        .resolve_requested_skills(&request.skills)
        .map_err(|error| anyhow::anyhow!("failed to resolve requested child skills: {error}"))?;

    let budget = request.skill_token_budget.unwrap_or(4_096);
    let total_tokens = SkillCatalog::sum_token_estimates(&resolved_skills)?;
    if total_tokens > budget {
        return Err(anyhow::anyhow!(
            "spawn rejected: requested skills exceed token budget (estimated {}, budget {})",
            total_tokens,
            budget
        ));
    }

    for skill in &resolved_skills {
        for required_cap in &skill.required_caps {
            if !selected_model
                .definition
                .caps
                .iter()
                .any(|cap| cap == required_cap)
            {
                return Err(anyhow::anyhow!(
                    "spawn rejected: model {} lacks required cap `{}` for skill `{}`",
                    selected_model.key,
                    required_cap,
                    skill.name
                ));
            }
        }
    }

    Ok(resolved_skills)
}

/// Create a child session and enqueue its initial task in the shared store.
pub fn spawn_child(
    store: &mut Store,
    config: &Config,
    parent_budget: BudgetSnapshot,
    request: SpawnRequest,
) -> Result<SpawnResult> {
    validate_spawn_budget(config, parent_budget)?;
    let resolved_tier = resolve_spawn_tier(config, &request)?;
    let selected_model = resolve_model(config, &request)?;
    let resolved_skills =
        validate_requested_skills(config, &resolved_tier, &selected_model, &request)?;
    let child_session_id = generate_child_session_id();
    let metadata = ChildSessionMetadata {
        parent_session_id: request.parent_session_id.clone(),
        task: request.task.clone(),
        task_kind: request.task_kind.clone(),
        tier: resolved_tier,
        model_override: request.model_override.clone(),
        reasoning_override: request.reasoning_override.clone(),
        active_agent: config.active_agent.clone(),
        default_model: config.models.default.clone(),
        resolved_model: selected_model.key.to_string(),
        resolved_provider_model: selected_model.definition.model.to_string(),
        skills: resolved_skills,
    };
    let metadata =
        serde_json::to_string(&metadata).context("failed to serialize child metadata")?;

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

    use crate::child_session::{enqueue_child_completion, should_enqueue_child_completion};
    use crate::llm::{ChatMessage, ChatRole, MessageContent};
    use crate::principal::Principal;
    use crate::session::Session;

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
            subscriptions: crate::config::SubscriptionsConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: Vec::new(),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: crate::config::AgentsConfig::default(),
            models,
            domains: crate::config::DomainsConfig::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    fn write_skill(path: &std::path::Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    fn skill_config(skill_dir: &std::path::Path) -> Config {
        let mut config = test_config();
        config.skills_dir = skill_dir.to_path_buf();
        config.skills_dir_resolved = skill_dir.to_path_buf();
        config.skills = SkillCatalog::load_from_dir(skill_dir).unwrap();
        config
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
            tier: Some("t2".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            ..Default::default()
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
            tier: None,
            model_override: None,
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: Some("missing".to_string()),
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: None,
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: None,
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: None,
            reasoning_override: None,
            ..Default::default()
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
            tier: None,
            model_override: None,
            reasoning_override: None,
            ..Default::default()
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

        assert!(enqueue_child_completion(&mut store, "child", &session, None).unwrap());

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
        assert!(enqueue_child_completion(&mut store, "child", &session, None).unwrap());

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

        assert!(!enqueue_child_completion(&mut store, "child", &session, None).unwrap());

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_request_deserializes_missing_skills_as_empty() {
        let request: SpawnRequest =
            serde_json::from_str(r#"{"parent_session_id":"parent","task":"inspect the tree"}"#)
                .unwrap();
        assert!(request.skills.is_empty());
        assert!(request.skill_token_budget.is_none());
    }

    #[test]
    fn spawn_rejects_skills_for_non_t3_child() {
        let root = temp_root("spawn_child_skills_non_t3");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t2".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["code-review".to_string()],
            skill_token_budget: None,
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("non-t3 children must reject skills");
        assert!(
            err.to_string()
                .contains("skills may only be loaded for spawned t3 children")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_unknown_skill_name() {
        let root = temp_root("spawn_child_unknown_skill");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["missing".to_string()],
            skill_token_budget: None,
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("unknown skills must fail closed");
        assert!(err.to_string().contains("unknown skill requested"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_duplicate_skill_name() {
        let root = temp_root("spawn_child_duplicate_skill");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["code-review".to_string(), "code-review".to_string()],
            skill_token_budget: None,
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("duplicate skills must fail closed");
        assert!(err.to_string().contains("duplicate skill request"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_when_skill_tokens_exceed_explicit_budget() {
        let root = temp_root("spawn_child_skill_budget_explicit");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["code-review".to_string()],
            skill_token_budget: Some(400),
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("skill budget should be enforced");
        assert!(err.to_string().contains("exceed token budget"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_when_skill_tokens_exceed_default_budget_4096() {
        let root = temp_root("spawn_child_skill_budget_default");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=5000\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["code-review".to_string()],
            skill_token_budget: None,
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("default skill budget should be enforced");
        assert!(err.to_string().contains("exceed token budget"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_when_resolved_model_lacks_required_caps() {
        let root = temp_root("spawn_child_skill_caps");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("planning.toml"),
            "[skill]\nname='planning'\ndescription='Plans work'\nrequired_caps=['reasoning']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec!["planning".to_string()],
            skill_token_budget: Some(1000),
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("missing caps should fail closed");
        assert!(err.to_string().contains("lacks required cap"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_persists_resolved_skill_definitions_in_child_metadata() {
        let root = temp_root("spawn_child_skill_metadata");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let result = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "inspect".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: None,
                skills: vec!["code-review".to_string()],
                skill_token_budget: Some(1000),
            },
        )
        .unwrap();

        let conn = rusqlite::Connection::open(root.join("queue.sqlite")).unwrap();
        let metadata: String = conn
            .query_row(
                "SELECT metadata FROM sessions WHERE id = ?1",
                [&result.child_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(metadata.contains(r#""skills":[{"name":"code-review""#));
        assert!(metadata.contains("Original instructions."));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawned_child_metadata_deserializes_old_rows_without_skills_field() {
        let root = temp_root("spawn_child_skill_metadata_compat");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let result = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "inspect".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: None,
                skills: vec!["code-review".to_string()],
                skill_token_budget: Some(1000),
            },
        )
        .unwrap();

        let metadata_json: String = store
            .get_session_metadata(&result.child_session_id)
            .unwrap()
            .expect("child metadata should exist");
        let mut metadata_value: serde_json::Value = serde_json::from_str(&metadata_json).unwrap();
        metadata_value
            .as_object_mut()
            .expect("metadata should be an object")
            .remove("skills");
        let parsed = parse_child_session_metadata(&metadata_value.to_string()).unwrap();
        assert!(parsed.skills.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_rejects_overflowing_skill_token_estimates() {
        let root = temp_root("spawn_child_skill_budget_overflow");
        let skills_dir = root.join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        write_skill(
            &skills_dir.join("overflow.toml"),
            "[skill]\nname='overflow'\ndescription='Overflow test'\nrequired_caps=['code_review']\ntoken_estimate=9223372036854775807\ninstructions='Original instructions.'\n",
        );
        write_skill(
            &skills_dir.join("overflow-two.toml"),
            "[skill]\nname='overflow-two'\ndescription='Overflow test'\nrequired_caps=['code_review']\ntoken_estimate=9223372036854775807\ninstructions='Original instructions.'\n",
        );
        write_skill(
            &skills_dir.join("overflow-three.toml"),
            "[skill]\nname='overflow-three'\ndescription='Overflow test'\nrequired_caps=['code_review']\ntoken_estimate=9223372036854775807\ninstructions='Original instructions.'\n",
        );

        let mut store = Store::new(&root.join("queue.sqlite")).unwrap();
        store.create_session("parent", None).unwrap();

        let request = SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "inspect".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec![
                "overflow".to_string(),
                "overflow-two".to_string(),
                "overflow-three".to_string(),
            ],
            skill_token_budget: None,
        };

        let err = spawn_child(
            &mut store,
            &skill_config(&skills_dir),
            BudgetSnapshot::default(),
            request,
        )
        .expect_err("overflowing skill estimates must fail closed");
        assert!(err.to_string().contains("overflow"));

        let _ = fs::remove_dir_all(&root);
    }
}
