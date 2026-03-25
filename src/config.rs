//! Configuration loading for runtime defaults and optional `agents.toml` overrides.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::identity;

/// Runtime configuration loaded by the CLI when starting agent mode.
#[derive(Debug, Clone)]
pub struct Config {
    /// LLM model name used for each API request.
    pub model: String,
    /// Starting system prompt injected into each new session.
    pub system_prompt: String,
    /// Base URL of the OpenAI-compatible responses endpoint.
    pub base_url: String,
    /// Optional reasoning effort hint forwarded to the model provider.
    pub reasoning_effort: Option<String>,
    /// Optional default CLI session name loaded from configuration.
    pub session_name: Option<String>,
    /// Optional operator API key for privileged HTTP access.
    pub operator_key: Option<String>,
    /// Shell execution policy loaded from the optional `[shell]` table.
    pub shell_policy: ShellPolicy,
    /// Optional budget ceilings loaded from the optional `[budget]` table.
    pub budget: Option<BudgetConfig>,
    /// Optional structured read policy loaded from the optional `[read]` table.
    pub read: ReadToolConfig,
    /// Queue recovery settings loaded from the optional `[queue]` table.
    pub queue: QueueConfig,
    /// Resolved identity prompt files used to assemble the system prompt.
    pub identity_files: Vec<PathBuf>,
    /// Parsed `[agents]` catalog, if present.
    pub agents: AgentsConfig,
    /// Parsed `[models]` catalog and routes.
    pub models: ModelsConfig,
    /// Parsed `[domains]` packs.
    pub domains: DomainsConfig,
    /// Selected named brain, if v2 config is active.
    pub active_agent: Option<String>,
}

/// Optional budget ceilings loaded from `[budget]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BudgetConfig {
    /// Maximum tokens allowed for the most recent completed turn.
    pub max_tokens_per_turn: Option<u64>,
    /// Maximum tokens allowed for the whole session.
    pub max_tokens_per_session: Option<u64>,
    /// Maximum tokens allowed for the current day.
    pub max_tokens_per_day: Option<u64>,
}

/// Queue recovery defaults loaded from `[queue]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct QueueConfig {
    /// Requeue `processing` rows only after this many seconds since claim.
    #[serde(default = "default_stale_processing_timeout_secs")]
    pub stale_processing_timeout_secs: u64,
}

/// Structured read tool policy loaded from `[read]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReadToolConfig {
    /// Root-relative paths allowed for structured file reads.
    pub allowed_paths: Vec<String>,
    /// Maximum bytes returned by one read.
    pub max_read_bytes: usize,
}

pub(crate) const DEFAULT_SHELL_MAX_OUTPUT_BYTES: usize = 1_048_576;
pub(crate) const DEFAULT_SHELL_MAX_TIMEOUT_MS: u64 = 120_000;
pub(crate) const DEFAULT_STALE_PROCESSING_TIMEOUT_SECS: u64 = 300;

pub(crate) const fn default_shell_max_output_bytes() -> usize {
    DEFAULT_SHELL_MAX_OUTPUT_BYTES
}

pub(crate) const fn default_shell_max_timeout_ms() -> u64 {
    DEFAULT_SHELL_MAX_TIMEOUT_MS
}

pub(crate) const fn default_stale_processing_timeout_secs() -> u64 {
    DEFAULT_STALE_PROCESSING_TIMEOUT_SECS
}

impl Config {
    /// Load configuration from a file, falling back to sensible defaults.
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Self {
            model: "gpt-5.4".to_string(),
            system_prompt: "You are a direct and capable coding agent. Execute tasks efficiently."
                .to_string(),
            base_url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            reasoning_effort: None,
            session_name: None,
            operator_key: None,
            shell_policy: ShellPolicy::default(),
            budget: None,
            read: ReadToolConfig::default(),
            queue: QueueConfig::default(),
            identity_files: identity::t1_identity_files("identity-templates", "silas"),
            agents: AgentsConfig::default(),
            models: ModelsConfig::default(),
            domains: DomainsConfig::default(),
            active_agent: None,
        };

        let contents = match std::fs::read_to_string(config_path.as_ref()) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(config),
            Err(error) => {
                return Err(anyhow!("failed to read config file: {error}"));
            }
        };

        let file_config: RuntimeFileConfig = toml::from_str(&contents)
            .map_err(|error| anyhow!("failed to parse agents.toml: {error}"))?;

        if file_config.agents.is_none() {
            return Err(anyhow!(
                "agents.toml must define at least one [agents.*] table"
            ));
        }

        config.models = file_config.models;
        config.domains = file_config.domains;
        validate_read_tool_config(&file_config.read)?;
        config.read = file_config.read;

        if let Some(agents) = file_config.agents {
            let (active_name, active_agent) = select_active_agent(&agents)?;
            config.active_agent = Some(active_name.clone());
            config.agents = agents;

            let use_t2_files = matches!(active_agent.tier.as_deref(), Some("t2") | Some("t3"));
            let selected_tier = if use_t2_files {
                &active_agent.t2
            } else {
                &active_agent.t1
            };
            let selected_identity = active_agent
                .identity
                .clone()
                .unwrap_or_else(|| active_name.clone());
            validate_agent_identity(&selected_identity)?;

            config.identity_files = if use_t2_files {
                identity::t2_identity_files("identity-templates")
            } else {
                identity::t1_identity_files("identity-templates", &selected_identity)
            };
            if !config.domains.entries.is_empty() && config.domains.selected.is_empty() {
                return Err(anyhow!(
                    "domains config must select at least one context pack"
                ));
            }
            for domain_name in &config.domains.selected {
                let Some(domain) = config.domains.entries.get(domain_name) else {
                    return Err(anyhow!(
                        "selected domain pack is missing from [domains]: {domain_name}"
                    ));
                };
                let Some(path) = domain.context_extend.as_ref() else {
                    return Err(anyhow!(
                        "selected domain pack is missing context_extend: {domain_name}"
                    ));
                };
                validate_domain_context_extend(path)?;
                config.identity_files.push(PathBuf::from(path));
            }
            if let Some(model) = selected_tier
                .model
                .clone()
                .or_else(|| active_agent.model.clone())
            {
                config.model = model;
            }
            if let Some(reasoning_effort) = selected_tier
                .reasoning
                .clone()
                .or_else(|| selected_tier.reasoning_effort.clone())
                .or_else(|| active_agent.reasoning_effort.clone())
            {
                config.reasoning_effort = Some(reasoning_effort);
            }
            if let Some(base_url) = selected_tier
                .base_url
                .clone()
                .or_else(|| active_agent.base_url.clone())
            {
                config.base_url = base_url;
            }
            if let Some(prompt) = selected_tier
                .system_prompt
                .clone()
                .or_else(|| active_agent.system_prompt.clone())
            {
                config.system_prompt = prompt;
            }
            if let Some(session_name) = selected_tier
                .session_name
                .clone()
                .or_else(|| active_agent.session_name.clone())
            {
                config.session_name = Some(session_name);
            }
        }

        if let Some(operator_key) = file_config
            .auth
            .as_ref()
            .and_then(|auth| auth.operator_key.clone())
        {
            config.operator_key = Some(operator_key);
        }

        config.shell_policy = file_config.shell;
        config.budget = file_config.budget;
        config.queue = file_config.queue;

        if let Ok(operator_key) = std::env::var("AUTOPOIESIS_OPERATOR_KEY") {
            config.operator_key = Some(operator_key);
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Guard, GuardContext, GuardEvent, ShellSafety, Verdict};
    use crate::llm::ToolCall;
    use std::env;
    use std::fs::File;
    use std::io::Write;
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
        assert_eq!(policy.default, "approve");
        assert!(policy.allow_patterns.is_empty());
        assert!(policy.deny_patterns.is_empty());
        assert!(policy.standing_approvals.is_empty());
        assert_eq!(policy.default_severity, "medium");
        assert_eq!(policy.max_output_bytes, DEFAULT_SHELL_MAX_OUTPUT_BYTES);
        assert_eq!(policy.max_timeout_ms, DEFAULT_SHELL_MAX_TIMEOUT_MS);
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
        assert_eq!(config.shell_policy.default, "allow");
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
        assert_eq!(config.shell_policy.default_severity, "high");
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
            queue: QueueConfig::default(),
            identity_files: vec![
                PathBuf::from("identity-templates/constitution.md"),
                PathBuf::from("identity-templates/context.md"),
            ],
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
            arguments: serde_json::json!({"command":"git diff --no-index /dev/null ~/.autopoiesis/auth.json"}).to_string(),
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFileConfig {
    #[serde(default)]
    agents: Option<AgentsConfig>,
    #[serde(default)]
    models: ModelsConfig,
    #[serde(default)]
    domains: DomainsConfig,
    auth: Option<AuthFileSection>,
    #[serde(default)]
    shell: ShellPolicy,
    budget: Option<BudgetConfig>,
    #[serde(default)]
    read: ReadToolConfig,
    #[serde(default)]
    queue: QueueConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentsConfig {
    #[serde(flatten, default)]
    pub entries: HashMap<String, AgentDefinition>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentDefinition {
    pub identity: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub t1: AgentTierConfig,
    #[serde(default)]
    pub t2: AgentTierConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentTierConfig {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub delegation_token_threshold: Option<u64>,
    #[serde(default)]
    pub delegation_tool_depth: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelsConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub catalog: HashMap<String, ModelDefinition>,
    #[serde(default)]
    pub routes: HashMap<String, ModelRoute>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ModelDefinition {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub caps: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub cost_tier: Option<String>,
    #[serde(default)]
    pub cost_unit: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ModelRoute {
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub prefer: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DomainsConfig {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(flatten, default)]
    pub entries: HashMap<String, DomainConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DomainConfig {
    pub context_extend: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthFileSection {
    operator_key: Option<String>,
}

/// v2 currently supports exactly one active agent entry. If multiple named
/// agents are added, configuration should be extended with an explicit
/// selector before startup tries to resolve them.
fn select_active_agent(agents: &AgentsConfig) -> Result<(String, AgentDefinition)> {
    let mut active = agents
        .entries
        .iter()
        .filter(|(name, _)| name.as_str() != "default");

    let Some((name, definition)) = active.next() else {
        return Err(anyhow!("agents config does not define an active brain"));
    };

    if active.next().is_some() {
        return Err(anyhow!("agents config defines multiple active brains"));
    }

    Ok((name.clone(), definition.clone()))
}

fn validate_agent_identity(identity: &str) -> Result<()> {
    if identity.is_empty() {
        return Err(anyhow!("agent identity must not be empty"));
    }
    if identity == "." || identity == ".." || identity.chars().any(std::path::is_separator) {
        return Err(anyhow!("agent identity must be a single path segment"));
    }

    Ok(())
}

fn validate_domain_context_extend(path: &str) -> Result<()> {
    let mut components = Path::new(path).components();
    match components.next() {
        Some(Component::Normal(root)) if root == "identity-templates" => {}
        _ => {
            return Err(anyhow!(
                "domain context_extend must stay under identity-templates/"
            ));
        }
    }

    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(anyhow!(
            "domain context_extend must stay under identity-templates/"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod domain_context_extend_tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_toml_path(prefix: &str, contents: &str) -> String {
        let mut path = std::env::temp_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch");
        path.push(format!(
            "autopoiesis_{prefix}_{}_{}.toml",
            std::process::id(),
            now.as_nanos()
        ));
        fs::write(&path, contents).expect("failed to write temp toml");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn rejects_domain_context_extend_outside_identity_templates() {
        let path = temp_toml_path(
            "bad_domain_context",
            "[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nmodel='gpt-5.4-mini'\n[domains]\nselected=['demo']\n[domains.demo]\ncontext_extend='../prompt.md'\n",
        );

        let err = Config::load(&path).expect_err("expected invalid domain path to fail");
        assert!(
            err.to_string()
                .contains("domain context_extend must stay under identity-templates/")
        );
    }
}

/// Shell execution policy loaded from `[shell]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShellPolicy {
    /// "approve" (default) or "allow".
    pub default: String,
    /// Glob patterns that are auto-allowed.
    pub allow_patterns: Vec<String>,
    /// Glob patterns that are always denied.
    pub deny_patterns: Vec<String>,
    /// Glob patterns that bypass approval prompts but remain auditable.
    pub standing_approvals: Vec<String>,
    /// Severity for commands that don't match any pattern.
    pub default_severity: String,
    /// Maximum combined stdout/stderr bytes captured from a shell command.
    #[serde(default = "default_shell_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Maximum shell command runtime allowed, even if the model requests more.
    #[serde(default = "default_shell_max_timeout_ms")]
    pub max_timeout_ms: u64,
}

impl Default for ShellPolicy {
    fn default() -> Self {
        Self {
            default: "approve".to_string(),
            allow_patterns: Vec::new(),
            deny_patterns: Vec::new(),
            standing_approvals: Vec::new(),
            default_severity: "medium".to_string(),
            max_output_bytes: default_shell_max_output_bytes(),
            max_timeout_ms: default_shell_max_timeout_ms(),
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            stale_processing_timeout_secs: default_stale_processing_timeout_secs(),
        }
    }
}

impl Default for ReadToolConfig {
    fn default() -> Self {
        Self {
            allowed_paths: vec!["identity-templates".to_string()],
            max_read_bytes: 65_536,
        }
    }
}

impl Config {
    /// Resolve the active agent definition, if v2 config loaded successfully.
    pub fn active_agent_definition(&self) -> Option<&AgentDefinition> {
        let active_name = self.active_agent.as_ref()?;
        self.agents.entries.get(active_name)
    }

    /// Resolve the active T1 tier config for the current brain, if available.
    pub fn active_t1_config(&self) -> Option<&AgentTierConfig> {
        self.active_agent_definition().map(|agent| &agent.t1)
    }

    /// Clone the current runtime config and retarget it for a spawned child session.
    pub fn with_spawned_child_runtime(
        &self,
        tier: &str,
        provider_model: &str,
        reasoning_override: Option<&str>,
    ) -> Result<Self> {
        validate_spawn_tier(tier)?;

        let mut config = self.clone();
        let parent_reasoning_effort = config.reasoning_effort.clone();
        let parent_session_name = config.session_name.clone();
        let active_name = config
            .active_agent
            .clone()
            .ok_or_else(|| anyhow!("spawned child config requires an active agent"))?;

        let selected_identity = {
            let agent = config
                .agents
                .entries
                .get_mut(&active_name)
                .ok_or_else(|| anyhow!("spawned child config missing active agent entry"))?;
            agent.tier = Some(tier.to_string());
            agent
                .identity
                .clone()
                .unwrap_or_else(|| active_name.clone())
        };

        config.identity_files = if matches!(tier, "t2" | "t3") {
            identity::t2_identity_files("identity-templates")
        } else {
            identity::t1_identity_files("identity-templates", &selected_identity)
        };
        if !config.domains.entries.is_empty() && config.domains.selected.is_empty() {
            return Err(anyhow!(
                "domains config must select at least one context pack"
            ));
        }
        for domain_name in &config.domains.selected {
            let Some(domain) = config.domains.entries.get(domain_name) else {
                return Err(anyhow!(
                    "selected domain pack is missing from [domains]: {domain_name}"
                ));
            };
            let Some(path) = domain.context_extend.as_ref() else {
                return Err(anyhow!(
                    "selected domain pack is missing context_extend: {domain_name}"
                ));
            };
            validate_domain_context_extend(path)?;
            config.identity_files.push(PathBuf::from(path));
        }

        config.model = provider_model.to_string();
        config.reasoning_effort = reasoning_override.map(ToString::to_string).or_else(|| {
            let agent = config.active_agent_definition()?;
            let tier_config = if matches!(tier, "t2" | "t3") {
                &agent.t2
            } else {
                &agent.t1
            };
            tier_config
                .reasoning
                .clone()
                .or_else(|| tier_config.reasoning_effort.clone())
                .or_else(|| agent.reasoning_effort.clone())
                .or(parent_reasoning_effort.clone())
        });
        config.base_url = {
            let agent = config
                .active_agent_definition()
                .ok_or_else(|| anyhow!("spawned child config missing active agent definition"))?;
            let tier_config = if matches!(tier, "t2" | "t3") {
                &agent.t2
            } else {
                &agent.t1
            };
            tier_config
                .base_url
                .clone()
                .or_else(|| agent.base_url.clone())
                .unwrap_or_else(|| config.base_url.clone())
        };
        config.system_prompt = {
            let agent = config
                .active_agent_definition()
                .ok_or_else(|| anyhow!("spawned child config missing active agent definition"))?;
            let tier_config = if matches!(tier, "t2" | "t3") {
                &agent.t2
            } else {
                &agent.t1
            };
            tier_config
                .system_prompt
                .clone()
                .or_else(|| agent.system_prompt.clone())
                .unwrap_or_else(|| config.system_prompt.clone())
        };
        config.session_name = {
            let agent = config
                .active_agent_definition()
                .ok_or_else(|| anyhow!("spawned child config missing active agent definition"))?;
            let tier_config = if matches!(tier, "t2" | "t3") {
                &agent.t2
            } else {
                &agent.t1
            };
            tier_config
                .session_name
                .clone()
                .or_else(|| agent.session_name.clone())
                .or(parent_session_name.clone())
        };

        Ok(config)
    }
}

fn validate_read_tool_config(read: &ReadToolConfig) -> Result<()> {
    if read.allowed_paths.iter().any(|path| path.trim().is_empty()) {
        return Err(anyhow!("read.allowed_paths entries must not be empty"));
    }

    if read.max_read_bytes == 0 {
        return Err(anyhow!("read.max_read_bytes must be greater than zero"));
    }

    Ok(())
}

fn validate_spawn_tier(tier: &str) -> Result<()> {
    match tier {
        "t1" | "t2" | "t3" => Ok(()),
        other => Err(anyhow!("invalid child tier: {other}")),
    }
}
