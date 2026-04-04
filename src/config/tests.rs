#![cfg(not(clippy))]

use super::*;
use crate::gate::{Guard, GuardContext, GuardEvent, ShellSafety, Verdict};
use crate::llm::ToolCall;
use crate::skills::SkillCatalog;
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_toml_path(prefix: &str, contents: &str) -> String {
    let mut path = env::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    path.push(format!("autopoiesis_test_{prefix}_{now}.toml"));
    let mut file = File::create(&path).expect("failed to create temp toml file");
    file.write_all(contents.as_bytes())
        .expect("failed to write temp toml");
    path.to_string_lossy().to_string()
}

fn assert_default_shell_policy(policy: &ShellPolicy) {
    assert_eq!(policy.default, ShellDefaultAction::Approve);
    assert!(policy.allow_patterns.is_empty());
    assert!(policy.deny_patterns.is_empty());
    assert!(policy.standing_approvals.is_empty());
    assert_eq!(policy.default_severity, ShellDefaultSeverity::Medium);
    assert_eq!(policy.max_output_bytes, DEFAULT_SHELL_MAX_OUTPUT_BYTES);
    assert_eq!(policy.max_timeout_ms, DEFAULT_SHELL_MAX_TIMEOUT_MS);
}

#[test]
fn shell_policy_parsing_is_trimmed_and_case_insensitive() {
    assert_eq!(
        <ShellDefaultAction as std::convert::TryFrom<String>>::try_from("  AlLoW  ".to_string())
            .unwrap(),
        ShellDefaultAction::Allow
    );
    assert_eq!(
        <ShellDefaultSeverity as std::convert::TryFrom<String>>::try_from("  MeDiuM  ".to_string())
            .unwrap(),
        ShellDefaultSeverity::Medium
    );
}

#[test]
fn shell_policy_parsing_works_through_toml_deserialization() {
    #[derive(serde::Deserialize)]
    struct WrapperAction {
        action: ShellDefaultAction,
    }

    #[derive(serde::Deserialize)]
    struct WrapperSeverity {
        severity: ShellDefaultSeverity,
    }

    let action: WrapperAction = toml::from_str("action = \"  ApPrOvE  \"").unwrap();
    let severity: WrapperSeverity = toml::from_str("severity = \"  HiGh  \"").unwrap();

    assert_eq!(action.action, ShellDefaultAction::Approve);
    assert_eq!(severity.severity, ShellDefaultSeverity::High);
}

fn assert_default_queue_config(queue: &QueueConfig) {
    assert_eq!(
        queue.stale_processing_timeout_secs,
        DEFAULT_STALE_PROCESSING_TIMEOUT_SECS
    );
}

fn assert_default_read_config(read: &ReadToolConfig) {
    assert_eq!(read.allowed_paths, vec!["identity-templates".to_string()]);
    assert_eq!(read.max_read_bytes, 65_536);
}

#[test]
fn agent_tier_config_reports_configuration_state() {
    let mut tier = AgentTierConfig::default();
    assert!(!tier.is_configured());

    tier.model = Some("gpt-5.4-mini".to_string());
    assert!(tier.is_configured());
}

#[test]
fn loads_valid_agents_toml_with_all_fields() {
    let path = temp_toml_path(
        "all_fields",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-5.1'\nsystem_prompt='All good'\nbase_url='https://example.test/api'\nreasoning='low'\nsession_name='fix-auth'\n[models]\ndefault='gpt5_mini'\n[models.catalog.gpt5_mini]\nprovider='openai'\nmodel='gpt-5.1'\n[models.routes.default]\nrequires=[]\nprefer=['gpt5_mini']\n[domains]\nselected=['demo']\n[domains.demo]\ncontext_extend='identity-templates/domains/demo.md'\n[auth]\noperator_key='operator-secret'\n[shell]\ndefault='allow'\nallow_patterns=['git *','cargo *']\ndeny_patterns=['rm -rf /*']\nstanding_approvals=['git push *','cargo publish *']\ndefault_severity='high'\nmax_output_bytes=2048\nmax_timeout_ms=4096\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.model, "gpt-5.1");
    assert_eq!(config.system_prompt, "All good");
    assert_eq!(config.base_url, "https://example.test/api");
    assert_eq!(config.reasoning_effort, Some("low".to_string()));
    assert_eq!(config.session_name, Some("fix-auth".to_string()));
    assert_eq!(config.operator_key, Some("operator-secret".to_string()));
    assert_eq!(config.shell_policy.default, ShellDefaultAction::Allow);
    assert_eq!(
        config.shell_policy.allow_patterns,
        vec!["git *".to_string(), "cargo *".to_string()]
    );
    assert_eq!(
        config.shell_policy.deny_patterns,
        vec!["rm -rf /*".to_string()]
    );
    assert_eq!(
        config.shell_policy.standing_approvals,
        vec!["git push *".to_string(), "cargo publish *".to_string()]
    );
    assert_eq!(
        config.shell_policy.default_severity,
        ShellDefaultSeverity::High
    );
    assert_eq!(config.shell_policy.max_output_bytes, 2048);
    assert_eq!(config.shell_policy.max_timeout_ms, 4096);
    assert_eq!(config.budget, None);
    assert_default_read_config(&config.read);
    assert_default_queue_config(&config.queue);
}

#[test]
fn loads_new_agents_silas_config_with_models_and_domains() {
    let path = temp_toml_path(
        "agents_v2",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nbase_url='https://example.test/api'\nsystem_prompt='legacy defaults'\nsession_name='legacy-session'\nmodel='gpt-5.4-mini'\nreasoning='medium'\ndelegation_token_threshold=12000\ndelegation_tool_depth=3\n[agents.silas.t2]\nmodel='o3'\nreasoning='xhigh'\n[models]\ndefault='gpt5_mini'\n[models.catalog.gpt5_mini]\nprovider='openai'\nmodel='gpt-5.4-mini'\ncaps=['fast','cheap','reasoning']\ncontext_window=128000\ncost_tier='cheap'\ncost_unit=1\nenabled=true\n[models.routes.code_review]\nrequires=['code']\nprefer=['gpt5_mini']\n[domains]\nselected=['fitness']\n[domains.fitness]\ncontext_extend='identity-templates/domains/fitness.md'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.active_agent, Some("silas".to_string()));
    assert_eq!(
        config.identity_files,
        vec![
            PathBuf::from("identity-templates/constitution.md"),
            PathBuf::from("identity-templates/agents/silas/agent.md"),
            PathBuf::from("identity-templates/context.md"),
            PathBuf::from("identity-templates/domains/fitness.md"),
        ]
    );
    assert_eq!(config.model, "gpt-5.4-mini");
    assert_eq!(config.reasoning_effort, Some("medium".to_string()));
    assert_eq!(config.base_url, "https://example.test/api");
    assert_eq!(config.system_prompt, "legacy defaults");
    assert_eq!(config.session_name, Some("legacy-session".to_string()));
    assert_eq!(
        config
            .active_t1_config()
            .map(|tier| tier.delegation_token_threshold),
        Some(Some(12_000))
    );
    assert_eq!(
        config
            .active_t1_config()
            .map(|tier| tier.delegation_tool_depth),
        Some(Some(3))
    );
    assert_eq!(config.models.default, Some("gpt5_mini".to_string()));
    let catalog = config
        .models
        .catalog
        .get("gpt5_mini")
        .expect("expected catalog entry");
    assert_eq!(catalog.provider, "openai");
    assert_eq!(catalog.model, "gpt-5.4-mini");
    assert_eq!(
        config
            .models
            .routes
            .get("code_review")
            .expect("expected route")
            .prefer,
        vec!["gpt5_mini".to_string()]
    );
    assert_eq!(
        config
            .domains
            .entries
            .get("fitness")
            .and_then(|domain| domain.context_extend.as_deref()),
        Some("identity-templates/domains/fitness.md")
    );
    assert_default_read_config(&config.read);
    assert_default_queue_config(&config.queue);
}

#[test]
fn rejects_agent_identity_path_traversal() {
    let path = temp_toml_path(
        "identity_traversal",
        "[agents.silas]\nidentity='../tmp/prompt'\n[agents.silas.t1]\nmodel='gpt-5.4-mini'\n",
    );

    let err = Config::load(&path).expect_err("expected invalid identity to fail");
    assert!(
        err.to_string()
            .contains("agent identity must be a single path segment")
    );
}

#[test]
fn rejects_domains_without_explicit_selection() {
    let path = temp_toml_path(
        "domains_without_selection",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-5.4-mini'\n[domains.demo]\ncontext_extend='identity-templates/domains/demo.md'\n",
    );

    let err = Config::load(&path).expect_err("expected missing selection to fail");
    assert!(
        err.to_string()
            .contains("domains config must select at least one context pack")
    );
}

#[test]
fn t2_agent_uses_t2_identity_files() {
    let path = temp_toml_path(
        "mixed_mode",
        "[agents.silas]\nidentity='silas'\ntier='t2'\n[agents.silas.t1]\nmodel='gpt-5.4-mini'\nreasoning='medium'\n[agents.silas.t2]\nmodel='o3'\nreasoning='xhigh'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.identity_files,
        vec![
            PathBuf::from("identity-templates/constitution.md"),
            PathBuf::from("identity-templates/context.md"),
        ]
    );
    assert_eq!(config.model, "o3");
    assert_eq!(config.active_agent, Some("silas".to_string()));
    assert_default_read_config(&config.read);
}

#[test]
fn spawned_child_runtime_uses_t2_identity_files_and_reasoning_override() {
    let path = temp_toml_path(
        "spawned_child_t2",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nbase_url='https://example.test/api'\nsystem_prompt='legacy defaults'\nsession_name='legacy-session'\nmodel='gpt-5.4-mini'\nreasoning='medium'\ndelegation_token_threshold=12000\ndelegation_tool_depth=3\n[agents.silas.t2]\nmodel='o3'\nreasoning='xhigh'\n[models]\ndefault='gpt5_mini'\n[models.catalog.gpt5_mini]\nprovider='openai'\nmodel='gpt-5.4-mini'\n[domains]\nselected=['fitness']\n[domains.fitness]\ncontext_extend='identity-templates/domains/fitness.md'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    let child = config
        .with_spawned_child_runtime("t2", "o3", Some("high"))
        .expect("expected child runtime config");

    assert_eq!(child.model, "o3");
    assert_eq!(child.reasoning_effort, Some("high".to_string()));
    assert_eq!(
        child.identity_files,
        vec![
            PathBuf::from("identity-templates/constitution.md"),
            PathBuf::from("identity-templates/context.md"),
            PathBuf::from("identity-templates/domains/fitness.md"),
        ]
    );
    assert_eq!(
        child
            .active_agent_definition()
            .and_then(|agent| agent.tier.as_deref()),
        Some("t2")
    );
}

#[test]
fn spawned_child_runtime_uses_t1_identity_files_and_selected_domains() {
    let path = temp_toml_path(
        "spawned_child_t1",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nbase_url='https://example.test/api'\nsystem_prompt='legacy defaults'\nsession_name='legacy-session'\nmodel='gpt-5.4-mini'\nreasoning='medium'\n[agents.silas.t2]\nmodel='o3'\nreasoning='xhigh'\n[models]\ndefault='gpt5_mini'\n[models.catalog.gpt5_mini]\nprovider='openai'\nmodel='gpt-5.4-mini'\n[domains]\nselected=['fitness']\n[domains.fitness]\ncontext_extend='identity-templates/domains/fitness.md'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    let child = config
        .with_spawned_child_runtime("t1", "gpt-5.4-mini", None)
        .expect("expected child runtime config");

    assert_eq!(child.model, "gpt-5.4-mini");
    assert_eq!(
        child.identity_files,
        vec![
            PathBuf::from("identity-templates/constitution.md"),
            PathBuf::from("identity-templates/agents/silas/agent.md"),
            PathBuf::from("identity-templates/context.md"),
            PathBuf::from("identity-templates/domains/fitness.md"),
        ]
    );
    assert_eq!(
        child
            .active_agent_definition()
            .and_then(|agent| agent.tier.as_deref()),
        Some("t1")
    );
}

#[test]
fn spawned_child_runtime_falls_back_to_parent_reasoning_and_session_name() {
    let config = Config {
        model: "gpt-5.4-mini".to_string(),
        system_prompt: "parent system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("parent-reasoning".to_string()),
        session_name: Some("parent-session".to_string()),
        operator_key: None,
        shell_policy: ShellPolicy::default(),
        budget: None,
        read: ReadToolConfig::default(),
        subscriptions: SubscriptionsConfig::default(),
        queue: QueueConfig::default(),
        identity_files: vec![
            PathBuf::from("identity-templates/constitution.md"),
            PathBuf::from("identity-templates/context.md"),
        ],
        skills_dir: PathBuf::from("skills"),
        skills_dir_resolved: PathBuf::from("skills"),
        skills: SkillCatalog::default(),
        agents: {
            let mut agents = AgentsConfig::default();
            agents.entries.insert(
                "silas".to_string(),
                AgentDefinition {
                    identity: Some("silas".to_string()),
                    tier: None,
                    model: None,
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning_effort: None,
                    t1: AgentTierConfig::default(),
                    t2: AgentTierConfig::default(),
                },
            );
            agents
        },
        models: {
            let mut models = ModelsConfig::default();
            models.default = Some("gpt-5.4-mini".to_string());
            models.catalog.insert(
                "gpt-5.4-mini".to_string(),
                ModelDefinition {
                    provider: "openai".to_string(),
                    model: "gpt-5.4-mini".to_string(),
                    caps: vec!["code_review".to_string()],
                    context_window: Some(128_000),
                    cost_tier: Some("medium".to_string()),
                    cost_unit: Some(2),
                    enabled: Some(true),
                },
            );
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    };

    let child = config
        .with_spawned_child_runtime("t1", "gpt-5.4-mini", None)
        .expect("expected child runtime config");

    assert_eq!(child.reasoning_effort, Some("parent-reasoning".to_string()));
    assert_eq!(child.session_name, Some("parent-session".to_string()));
}

#[test]
fn loads_tightened_shell_policy_fixture() {
    let path = temp_toml_path(
        "tightened_shell",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-tightened'\n[shell]\ndefault='approve'\nallow_patterns=['cargo *','ls *','pwd','which *','date','uname *']\ndeny_patterns=['rm -rf /*','rm -rf ~*','curl * | sh*','wget * | sh*','> /dev/sd*']\ndefault_severity='medium'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.shell_policy.allow_patterns,
        vec![
            "cargo *".to_string(),
            "ls *".to_string(),
            "pwd".to_string(),
            "which *".to_string(),
            "date".to_string(),
            "uname *".to_string(),
        ]
    );
    assert!(
        !config
            .shell_policy
            .allow_patterns
            .iter()
            .any(|pattern| pattern == "git *" || pattern == "cat *" || pattern == "env")
    );
    assert_eq!(
        config.shell_policy.deny_patterns,
        vec![
            "rm -rf /*".to_string(),
            "rm -rf ~*".to_string(),
            "curl * | sh*".to_string(),
            "wget * | sh*".to_string(),
            "> /dev/sd*".to_string(),
        ]
    );
}

#[test]
fn loaded_shell_policy_still_allows_ls_but_not_env_or_git_show() {
    let path = temp_toml_path(
        "tightened_shell_behavior",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-tightened'\n[shell]\ndefault='approve'\nallow_patterns=['cargo *','ls *','pwd','which *','date','uname *']\ndeny_patterns=['rm -rf /*','rm -rf ~*','curl * | sh*','wget * | sh*','> /dev/sd*']\ndefault_severity='medium'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    let gate = ShellSafety::with_policy(config.shell_policy);

    let allow_call = ToolCall {
        id: "call-1".to_string(),
        name: "execute".to_string(),
        arguments: serde_json::json!({"command":"ls /tmp"}).to_string(),
    };
    let mut allow_event = GuardEvent::ToolCall(&allow_call);
    assert!(matches!(
        gate.check(&mut allow_event, &GuardContext::default()),
        Verdict::Allow
    ));

    let env_call = ToolCall {
        id: "call-2".to_string(),
        name: "execute".to_string(),
        arguments: serde_json::json!({"command":"env"}).to_string(),
    };
    let mut env_event = GuardEvent::ToolCall(&env_call);
    assert!(matches!(
        gate.check(&mut env_event, &GuardContext::default()),
        Verdict::Approve { .. }
    ));

    let git_call = ToolCall {
        id: "call-3".to_string(),
        name: "execute".to_string(),
        arguments:
            serde_json::json!({"command":"git diff --no-index /dev/null ~/.autopoiesis/auth.json"})
                .to_string(),
    };
    let mut git_event = GuardEvent::ToolCall(&git_call);
    assert!(matches!(
        gate.check(&mut git_event, &GuardContext::default()),
        Verdict::Deny { .. }
    ));
}

#[test]
fn loads_minimal_agents_toml_with_just_model() {
    let path = temp_toml_path(
        "minimal",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-minimal'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.model, "gpt-minimal");
    assert_eq!(
        config.system_prompt,
        "You are a direct and capable coding agent. Execute tasks efficiently."
    );
    assert_default_shell_policy(&config.shell_policy);
    assert_eq!(config.budget, None);
    assert_default_read_config(&config.read);
    assert_default_queue_config(&config.queue);
}

#[test]
fn uses_defaults_when_file_missing() {
    let config = Config::load("/does/not/exist.toml").expect("expected defaults to be used");
    assert_eq!(config.model, "gpt-5.4");
    assert_eq!(
        config.base_url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(config.reasoning_effort, None);
    assert_eq!(config.session_name, None);
    assert_eq!(config.operator_key, None);
    assert_default_shell_policy(&config.shell_policy);
    assert_eq!(config.budget, None);
    assert_default_read_config(&config.read);
    assert_default_queue_config(&config.queue);
}

#[test]
fn uses_defaults_for_missing_optional_fields() {
    let path = temp_toml_path(
        "missing_optional",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-only'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.model, "gpt-only");
    assert_eq!(
        config.base_url,
        "https://chatgpt.com/backend-api/codex/responses"
    );
    assert_eq!(
        config.system_prompt,
        "You are a direct and capable coding agent. Execute tasks efficiently."
    );
    assert_eq!(config.reasoning_effort, None);
    assert_eq!(config.session_name, None);
    assert_eq!(config.operator_key, None);
    assert_default_shell_policy(&config.shell_policy);
    assert_eq!(config.budget, None);
    assert_default_queue_config(&config.queue);
}

#[test]
fn loads_session_name_from_agents_toml() {
    let path = temp_toml_path(
        "session_name",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nsession_name='default-work'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.session_name, Some("default-work".to_string()));
}

#[test]
fn malformed_toml_returns_error() {
    let path = temp_toml_path("malformed", "[agents.silas]\nmodel = ");

    let result = Config::load(&path);
    assert!(result.is_err());
}

#[test]
fn rejects_agents_toml_without_any_agent_tables() {
    let path = temp_toml_path("no_agents", "");

    let result = Config::load(&path);
    assert!(result.is_err());
}

#[test]
fn rejects_legacy_agent_table_only() {
    let path = temp_toml_path("legacy_agent_only", "[agent]\nmodel='gpt-legacy'\n");

    let result = Config::load(&path);
    assert!(result.is_err());
}

#[test]
fn rejects_mixed_legacy_and_new_agent_tables() {
    let path = temp_toml_path(
        "mixed_agent_tables",
        "[agent]\nmodel='gpt-legacy'\n[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-new'\n",
    );

    let result = Config::load(&path);
    assert!(result.is_err());
}

#[test]
fn loads_operator_key_from_auth_section() {
    let path = temp_toml_path(
        "operator_key",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-auth'\n[auth]\noperator_key='operator-from-file'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.operator_key, Some("operator-from-file".to_string()));
}

#[test]
fn operator_key_env_overrides_file_value() {
    let path = temp_toml_path(
        "operator_key_env",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-auth'\n[auth]\noperator_key='operator-from-file'\n",
    );

    super::with_config_load_lock(|| {
        let original = std::env::var("AUTOPOIESIS_OPERATOR_KEY").ok();
        struct EnvGuard {
            original: Option<String>,
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                if let Some(value) = self.original.as_ref() {
                    unsafe {
                        std::env::set_var("AUTOPOIESIS_OPERATOR_KEY", value);
                    }
                } else {
                    unsafe {
                        std::env::remove_var("AUTOPOIESIS_OPERATOR_KEY");
                    }
                }
            }
        }

        let _env_guard = EnvGuard { original };
        unsafe {
            std::env::set_var("AUTOPOIESIS_OPERATOR_KEY", "operator-from-env");
        }

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.operator_key, Some("operator-from-env".to_string()));
    });
}

#[test]
fn loads_skills_catalog_even_when_config_file_is_missing() {
    let config = Config::load("/does/not/exist.toml").expect("expected defaults to be used");
    assert!(!config.skills.is_empty());
    assert!(config.skills.get("code-review").is_some());
}

#[test]
fn loads_skills_catalog_from_configured_directory() {
    let root = std::env::temp_dir().join(format!(
        "autopoiesis_config_skills_test_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let skills_dir = root.join("custom-skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code']\ntoken_estimate=500\ninstructions='full prompt'\n",
        )
        .unwrap();

    let config_path = root.join("agents.toml");
    let config_contents = format!(
        "skills_dir='custom-skills'\n[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-skills'\n",
    );
    std::fs::write(&config_path, config_contents).unwrap();

    let config = Config::load(&config_path).expect("expected config to load");
    assert_eq!(config.skills_dir, PathBuf::from("custom-skills"));
    assert_eq!(config.skills_dir_resolved, root.join("custom-skills"));
    assert_eq!(config.skills.browse().len(), 1);
    assert_eq!(
        config.skills.get("code-review").unwrap().description,
        "Reviews code changes"
    );

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn loads_skills_catalog_from_absolute_directory_without_rebasing() {
    let root = std::env::temp_dir().join(format!(
        "autopoiesis_config_skills_absolute_test_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let skills_dir = root.join("absolute-skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        skills_dir.join("code-review.toml"),
        "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code']\ntoken_estimate=500\ninstructions='full prompt'\n",
    )
    .unwrap();

    let config_path = root.join("agents.toml");
    let config_contents = format!(
        "skills_dir='{}'\n[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-skills'\n",
        skills_dir.display()
    );
    std::fs::write(&config_path, config_contents).unwrap();

    let config = Config::load(&config_path).expect("expected config to load");
    assert_eq!(config.skills_dir, skills_dir);
    assert_eq!(config.skills_dir_resolved, skills_dir);
    assert_eq!(config.skills.browse().len(), 1);

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn loads_read_config_with_all_fields() {
    let path = temp_toml_path(
        "read_all",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-read'\n[read]\nallowed_paths=['identity-templates','sessions']\nmax_read_bytes=4096\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.read,
        ReadToolConfig {
            allowed_paths: vec!["identity-templates".to_string(), "sessions".to_string()],
            max_read_bytes: 4096,
        }
    );
}

#[test]
fn missing_read_table_keeps_read_defaults() {
    let path = temp_toml_path(
        "read_missing",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-read'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_default_read_config(&config.read);
}

#[test]
fn read_config_rejects_empty_allowed_path_entry() {
    let path = temp_toml_path(
        "read_empty_path",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-read'\n[read]\nallowed_paths=['identity-templates','']\n",
    );

    let err = Config::load(&path).expect_err("expected invalid read config to fail");
    assert!(err.to_string().contains("read.allowed_paths"));
}

#[test]
fn read_config_rejects_zero_max_read_bytes() {
    let path = temp_toml_path(
        "read_zero_max",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-read'\n[read]\nmax_read_bytes=0\n",
    );

    let err = Config::load(&path).expect_err("expected invalid read config to fail");
    assert!(err.to_string().contains("max_read_bytes"));
}

#[test]
fn loads_budget_config_with_all_fields() {
    let path = temp_toml_path(
        "budget_all",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-budget'\n[budget]\nmax_tokens_per_turn=100\nmax_tokens_per_session=200\nmax_tokens_per_day=300\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.budget,
        Some(BudgetConfig {
            max_tokens_per_turn: Some(100),
            max_tokens_per_session: Some(200),
            max_tokens_per_day: Some(300),
        })
    );
    assert_default_queue_config(&config.queue);
}

#[test]
fn loads_budget_config_with_partial_fields() {
    let path = temp_toml_path(
        "budget_partial",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-budget'\n[budget]\nmax_tokens_per_session=250\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.budget,
        Some(BudgetConfig {
            max_tokens_per_turn: None,
            max_tokens_per_session: Some(250),
            max_tokens_per_day: None,
        })
    );
    assert_default_queue_config(&config.queue);
}

#[test]
fn missing_budget_table_keeps_budget_none() {
    let path = temp_toml_path(
        "budget_missing",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-budget'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.budget, None);
    assert_default_queue_config(&config.queue);
}

#[test]
fn shell_max_output_bytes_defaults_to_one_megabyte() {
    let path = temp_toml_path(
        "shell_default_output_bytes",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-shell'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.shell_policy.max_output_bytes,
        DEFAULT_SHELL_MAX_OUTPUT_BYTES
    );
    assert_default_queue_config(&config.queue);
}

#[test]
fn shell_max_output_bytes_override_is_honored() {
    let path = temp_toml_path(
        "shell_output_bytes_override",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-shell'\n[shell]\nmax_output_bytes=8192\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.shell_policy.max_output_bytes, 8192);
    assert_default_queue_config(&config.queue);
}

#[test]
fn shell_max_timeout_ms_defaults_to_two_minutes() {
    let path = temp_toml_path(
        "shell_default_timeout_ms",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-shell'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(
        config.shell_policy.max_timeout_ms,
        DEFAULT_SHELL_MAX_TIMEOUT_MS
    );
    assert_default_queue_config(&config.queue);
}

#[test]
fn shell_max_timeout_ms_override_is_honored() {
    let path = temp_toml_path(
        "shell_timeout_ms_override",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-shell'\n[shell]\nmax_timeout_ms=1500\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.shell_policy.max_timeout_ms, 1500);
    assert_default_queue_config(&config.queue);
}

#[test]
fn queue_stale_processing_timeout_defaults_to_five_minutes() {
    let path = temp_toml_path(
        "queue_default_timeout",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-queue'\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_default_queue_config(&config.queue);
}

#[test]
fn queue_stale_processing_timeout_override_is_honored() {
    let path = temp_toml_path(
        "queue_timeout_override",
        "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-queue'\n[queue]\nstale_processing_timeout_secs=42\n",
    );

    let config = Config::load(&path).expect("expected config to load");
    assert_eq!(config.queue.stale_processing_timeout_secs, 42);
}
