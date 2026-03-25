use crate::agent::spawn::{
    SpawnDrainContext, finish_spawned_child_drain, spawn_and_drain_with_provider,
};
use crate::agent::tests::common::*;
#[tokio::test]
async fn spawn_child_wrapper_enqueues_parent_completion_after_child_drain() {
    let root = temp_queue_root("child_completion");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let config = crate::config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: None,
        operator_key: None,
        shell_policy: crate::config::ShellPolicy::default(),
        budget: None,
        read: crate::config::ReadToolConfig::default(),
        queue: crate::config::QueueConfig::default(),
        identity_files: Vec::new(),
        skills_dir: std::path::PathBuf::from("skills"),
        skills_dir_resolved: std::path::PathBuf::from("skills"),
        skills: crate::skills::SkillCatalog::default(),
        agents: crate::config::AgentsConfig::default(),
        models: {
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
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    };

    let parent_session = Session::new(sessions_dir.join("parent")).expect("parent session");
    let parent_budget = parent_session
        .budget_snapshot()
        .expect("parent budget snapshot");

    let spawn_result = spawn_child(
        &mut store,
        &config,
        parent_budget,
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t2".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("low".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(spawn_result.resolved_model, "gpt-child");

    let mut session = Session::new(sessions_dir.join(&spawn_result.child_session_id)).unwrap();
    let turn = Turn::new();
    let mut provider_factory = || async {
        Ok::<_, anyhow::Error>(StaticProvider {
            turn: StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text("child finished")],
                },
                tool_calls: vec![],
                meta: Some(crate::llm::TurnMeta {
                    model: None,
                    input_tokens: None,
                    output_tokens: Some(1),
                    reasoning_tokens: None,
                    reasoning_trace: None,
                }),
                stop_reason: StopReason::Stop,
            },
        })
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    assert!(
        drain_queue(
            &mut store,
            &spawn_result.child_session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap()
        .is_none()
    );

    let completion = store.dequeue_next_message("parent").unwrap().unwrap();
    assert_eq!(completion.role, "user");
    assert_eq!(
        completion.source,
        format!("agent-{}", spawn_result.child_session_id)
    );
    assert!(completion.content.contains("Child session"));
    assert!(completion.content.contains("child finished"));

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spawn_and_drain_uses_child_runtime_config_and_returns_last_assistant_response() {
    use std::sync::{Arc, Mutex};

    let root = temp_queue_root("spawn_and_drain");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let config = crate::config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: None,
        operator_key: None,
        shell_policy: crate::config::ShellPolicy::default(),
        budget: None,
        read: crate::config::ReadToolConfig::default(),
        queue: crate::config::QueueConfig::default(),
        identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
        skills_dir: std::path::PathBuf::from("skills"),
        skills_dir_resolved: std::path::PathBuf::from("skills"),
        skills: crate::skills::SkillCatalog::default(),
        agents: {
            let mut agents = crate::config::AgentsConfig::default();
            agents.entries.insert(
                "silas".to_string(),
                crate::config::AgentDefinition {
                    identity: Some("silas".to_string()),
                    tier: None,
                    model: None,
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning_effort: None,
                    t1: crate::config::AgentTierConfig {
                        delegation_token_threshold: Some(12_000),
                        delegation_tool_depth: Some(3),
                        ..Default::default()
                    },
                    t2: crate::config::AgentTierConfig {
                        model: Some("o3".to_string()),
                        reasoning: Some("high".to_string()),
                        ..Default::default()
                    },
                },
            );
            agents
        },
        models: {
            let mut models = crate::config::ModelsConfig::default();
            models.default = Some("gpt-child".to_string());
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
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    };

    let observed_models = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));

    let mut provider_factory = {
        let observed_models = observed_models.clone();
        let observed_tools = observed_tools.clone();
        move |child_config: &crate::config::Config| {
            observed_models
                .lock()
                .expect("models mutex poisoned")
                .push((
                    child_config.model.clone(),
                    child_config.reasoning_effort.clone(),
                ));
            let provider = RecordingProvider {
                assistant_text: "child finished".to_string(),
                observed_tools: observed_tools.clone(),
            };
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let result = spawn_and_drain_with_provider(
        &mut store,
        &config,
        &sessions_dir,
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t2".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            ..Default::default()
        },
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert_eq!(result.resolved_model, "gpt-child");
    assert_eq!(
        result.last_assistant_response,
        Some("child finished".to_string())
    );
    assert_eq!(
        observed_models
            .lock()
            .expect("models mutex poisoned")
            .as_slice(),
        &[("gpt-child".to_string(), Some("high".to_string()))]
    );
    assert_eq!(
        observed_tools
            .lock()
            .expect("tools mutex poisoned")
            .as_slice(),
        &[vec!["read_file".to_string()]]
    );

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spawn_and_drain_uses_t3_runtime_config_and_returns_last_assistant_response() {
    use std::sync::{Arc, Mutex};

    let root = temp_queue_root("spawn_and_drain_t3");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let config = crate::config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: None,
        operator_key: None,
        shell_policy: crate::config::ShellPolicy::default(),
        budget: None,
        read: crate::config::ReadToolConfig::default(),
        queue: crate::config::QueueConfig::default(),
        identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
        skills_dir: std::path::PathBuf::from("skills"),
        skills_dir_resolved: std::path::PathBuf::from("skills"),
        skills: crate::skills::SkillCatalog::default(),
        agents: {
            let mut agents = crate::config::AgentsConfig::default();
            agents.entries.insert(
                "silas".to_string(),
                crate::config::AgentDefinition {
                    identity: Some("silas".to_string()),
                    tier: None,
                    model: None,
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning_effort: None,
                    t1: crate::config::AgentTierConfig::default(),
                    t2: crate::config::AgentTierConfig::default(),
                },
            );
            agents
        },
        models: {
            let mut models = crate::config::ModelsConfig::default();
            models.default = Some("gpt-child".to_string());
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
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    };

    let observed_models = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));

    let mut provider_factory = {
        let observed_models = observed_models.clone();
        let observed_tools = observed_tools.clone();
        move |child_config: &crate::config::Config| {
            observed_models
                .lock()
                .expect("models mutex poisoned")
                .push((
                    child_config.model.clone(),
                    child_config.reasoning_effort.clone(),
                ));
            let provider = RecordingProvider {
                assistant_text: "child finished".to_string(),
                observed_tools: observed_tools.clone(),
            };
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

    let result = spawn_and_drain_with_provider(
        &mut store,
        &config,
        &sessions_dir,
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            ..Default::default()
        },
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert_eq!(result.resolved_model, "gpt-child");
    assert_eq!(
        result.last_assistant_response,
        Some("child finished".to_string())
    );
    assert_eq!(
        observed_models
            .lock()
            .expect("models mutex poisoned")
            .as_slice(),
        &[("gpt-child".to_string(), Some("high".to_string()))]
    );
    assert_eq!(
        observed_tools
            .lock()
            .expect("tools mutex poisoned")
            .as_slice(),
        &[vec!["execute".to_string()]]
    );

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn drain_spawned_t3_uses_persisted_skill_snapshot_not_catalog_lookup() {
    use std::sync::{Arc, Mutex};

    let root = temp_queue_root("spawned_t3_skill_snapshot");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        )
        .unwrap();

    let mut config = spawned_t3_test_config(
        skills_dir.clone(),
        crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap(),
    );

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let spawn_result = spawn_child(
        &mut store,
        &config,
        crate::gate::BudgetSnapshot::default(),
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            skills: vec!["code-review".to_string()],
            skill_token_budget: Some(2_000),
        },
    )
    .unwrap();

    std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Mutated instructions.'\n",
        )
        .unwrap();
    config.skills = crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap_or_default();

    let observed_system_texts = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut provider_factory = {
        let observed_system_texts = observed_system_texts.clone();
        move |_child_config: &crate::config::Config| {
            let provider = MessageRecordingProvider {
                assistant_text: "child finished".to_string(),
                observed_system_texts: observed_system_texts.clone(),
            };
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let metadata_json = store
        .get_session_metadata(&spawn_result.child_session_id)
        .unwrap()
        .expect("child metadata should exist");
    let context = SpawnDrainContext {
        store: &mut store,
        config: &config,
        session_dir: &sessions_dir,
        spawn_result,
    };

    let result = finish_spawned_child_drain(
        context,
        &metadata_json,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert_eq!(result.resolved_model, "gpt-child");
    let system_texts = observed_system_texts
        .lock()
        .expect("system text mutex poisoned");
    assert_eq!(system_texts.len(), 1);
    assert!(system_texts[0].contains("Skill: code-review"));
    assert!(system_texts[0].contains("Original instructions."));
    assert!(!system_texts[0].contains("Mutated instructions."));
    assert!(!system_texts[0].contains("Available skills:"));

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn drain_old_spawned_child_without_skills_metadata_still_runs() {
    use std::sync::{Arc, Mutex};

    let root = temp_queue_root("spawned_t3_old_metadata");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        )
        .unwrap();

    let config = spawned_t3_test_config(
        skills_dir.clone(),
        crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap(),
    );

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let spawn_result = spawn_child(
        &mut store,
        &config,
        crate::gate::BudgetSnapshot::default(),
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            skills: vec!["code-review".to_string()],
            skill_token_budget: Some(2_000),
        },
    )
    .unwrap();

    let mut metadata_value: Value = serde_json::from_str(
        &store
            .get_session_metadata(&spawn_result.child_session_id)
            .unwrap()
            .expect("child metadata should exist"),
    )
    .unwrap();
    metadata_value
        .as_object_mut()
        .expect("metadata should be an object")
        .remove("skills");
    let old_metadata_json = metadata_value.to_string();

    let observed_system_texts = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut provider_factory = {
        let observed_system_texts = observed_system_texts.clone();
        move |_child_config: &crate::config::Config| {
            let provider = MessageRecordingProvider {
                assistant_text: "child finished".to_string(),
                observed_system_texts: observed_system_texts.clone(),
            };
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let context = SpawnDrainContext {
        store: &mut store,
        config: &config,
        session_dir: &sessions_dir,
        spawn_result,
    };

    let result = finish_spawned_child_drain(
        context,
        &old_metadata_json,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert_eq!(result.resolved_model, "gpt-child");
    let system_texts = observed_system_texts
        .lock()
        .expect("system text mutex poisoned");
    assert_eq!(system_texts.len(), 1);
    assert!(!system_texts[0].contains("Skill: code-review"));
    assert!(!system_texts[0].contains("Available skills:"));

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spawn_and_drain_invokes_approval_handler_for_t3_shell_calls() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct ApprovalGateProvider {
        call_index: Arc<AtomicUsize>,
    }

    impl crate::llm::LlmProvider for ApprovalGateProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            match self.call_index.fetch_add(1, Ordering::SeqCst) {
                0 => Ok(streamed_turn_with_tool_call(
                    Some("requesting approval"),
                    "true",
                    "call-1",
                )),
                _ => Ok(StreamedTurn {
                    assistant_message: ChatMessage {
                        role: crate::llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![MessageContent::text("approval handled")],
                    },
                    tool_calls: vec![],
                    meta: Some(crate::llm::TurnMeta {
                        model: Some("gpt-child".to_string()),
                        input_tokens: Some(1),
                        output_tokens: Some(1),
                        reasoning_tokens: None,
                        reasoning_trace: None,
                    }),
                    stop_reason: StopReason::Stop,
                }),
            }
        }
    }

    let root = temp_queue_root("spawn_and_drain_approval");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();

    let mut config = crate::config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: None,
        operator_key: None,
        shell_policy: shell_policy("approve", &[], &[], &[], "medium"),
        budget: None,
        read: crate::config::ReadToolConfig::default(),
        queue: crate::config::QueueConfig::default(),
        identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
        skills_dir: std::path::PathBuf::from("skills"),
        skills_dir_resolved: std::path::PathBuf::from("skills"),
        skills: crate::skills::SkillCatalog::default(),
        agents: {
            let mut agents = crate::config::AgentsConfig::default();
            agents.entries.insert(
                "silas".to_string(),
                crate::config::AgentDefinition {
                    identity: Some("silas".to_string()),
                    tier: None,
                    model: None,
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning_effort: None,
                    t1: crate::config::AgentTierConfig::default(),
                    t2: crate::config::AgentTierConfig::default(),
                },
            );
            agents
        },
        models: {
            let mut models = crate::config::ModelsConfig::default();
            models.default = Some("gpt-child".to_string());
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
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    };
    config.agents.entries.get_mut("silas").unwrap().tier = Some("t3".to_string());

    let approval_calls = Arc::new(AtomicUsize::new(0));
    let observed_calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let mut provider_factory = {
        let call_index = Arc::new(AtomicUsize::new(0));
        let observed_calls = observed_calls.clone();
        move |_child_config: &crate::config::Config| {
            let provider = ApprovalGateProvider {
                call_index: call_index.clone(),
            };
            let observed_calls = observed_calls.clone();
            async move {
                observed_calls
                    .lock()
                    .expect("calls mutex poisoned")
                    .push(vec!["execute".to_string()]);
                Ok::<_, anyhow::Error>(provider)
            }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| {
        approval_calls.fetch_add(1, Ordering::SeqCst);
        true
    };

    let result = spawn_and_drain_with_provider(
        &mut store,
        &config,
        &sessions_dir,
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            ..Default::default()
        },
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .unwrap();

    assert_eq!(
        result.last_assistant_response,
        Some("approval handled".to_string())
    );
    assert!(approval_calls.load(Ordering::SeqCst) > 0);

    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn spawn_and_drain_rejects_invalid_persisted_tier() {
    use std::sync::{Arc, Mutex};

    let root = temp_queue_root("spawn_and_drain_bad_tier");
    let queue_path = root.join("queue.sqlite");
    let sessions_dir = root.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();

    let mut store = Store::new(&queue_path).unwrap();
    store.create_session("parent", None).unwrap();
    let spawn_result = spawn_child(
        &mut store,
        &crate::config::Config {
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
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig::default(),
                        t2: crate::config::AgentTierConfig::default(),
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
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
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        },
        crate::gate::BudgetSnapshot::default(),
        SpawnRequest {
            parent_session_id: "parent".to_string(),
            task: "child task".to_string(),
            task_kind: Some("code_review".to_string()),
            tier: Some("t2".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: Some("high".to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let bad_metadata = serde_json::json!({
        "parent_session_id": "parent",
        "task": "child task",
        "task_kind": "code_review",
        "tier": "bogus",
        "model_override": "gpt-child",
        "reasoning_override": "high",
        "resolved_model": spawn_result.resolved_model,
        "resolved_provider_model": "gpt-child",
    })
    .to_string();

    let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let mut provider_factory = {
        let observed_tools = observed_tools.clone();
        move |_child_config: &crate::config::Config| {
            let provider = RecordingProvider {
                assistant_text: "child finished".to_string(),
                observed_tools: observed_tools.clone(),
            };
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

    let context = SpawnDrainContext {
        store: &mut store,
        config: &crate::config::Config {
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
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig::default(),
                        t2: crate::config::AgentTierConfig::default(),
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
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
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        },
        session_dir: &sessions_dir,
        spawn_result,
    };

    let error = finish_spawned_child_drain(
        context,
        &bad_metadata,
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await
    .expect_err("invalid persisted tier should fail");

    assert!(error.to_string().contains("invalid child tier"));
    assert!(
        observed_tools
            .lock()
            .expect("tools mutex poisoned")
            .is_empty(),
        "provider should never be created for invalid metadata"
    );

    std::fs::remove_dir_all(&root).unwrap();
}
