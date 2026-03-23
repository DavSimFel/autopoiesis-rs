//! Configuration loading for runtime defaults and optional `agents.toml` overrides.

use std::path::Path;

use anyhow::{Result, anyhow};
use serde::Deserialize;

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
        };

        let contents = match std::fs::read_to_string(config_path.as_ref()) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(config),
            Err(error) => {
                return Err(anyhow!("failed to read config file: {error}"));
            }
        };

        let file_config: AgentFileConfig = toml::from_str(&contents)
            .map_err(|error| anyhow!("failed to parse agents.toml: {error}"))?;

        if let Some(model) = file_config.agent.model {
            config.model = model;
        }

        if let Some(prompt) = file_config.agent.system_prompt {
            config.system_prompt = prompt;
        }

        if let Some(base_url) = file_config.agent.base_url {
            config.base_url = base_url;
        }

        if let Some(reasoning_effort) = file_config.agent.reasoning_effort {
            config.reasoning_effort = Some(reasoning_effort);
        }

        if let Some(session_name) = file_config.agent.session_name {
            config.session_name = Some(session_name);
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
    }

    #[test]
    fn loads_valid_agents_toml_with_all_fields() {
        let path = temp_toml_path(
            "all_fields",
            "[agent]\nmodel='gpt-5.1'\nsystem_prompt='All good'\nbase_url='https://example.test/api'\nreasoning_effort='low'\nsession_name='fix-auth'\n[auth]\noperator_key='operator-secret'\n[shell]\ndefault='allow'\nallow_patterns=['git *','cargo *']\ndeny_patterns=['rm -rf /*']\nstanding_approvals=['git push *','cargo publish *']\ndefault_severity='high'\n",
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
        assert_eq!(config.budget, None);
    }

    #[test]
    fn loads_tightened_shell_policy_fixture() {
        let path = temp_toml_path(
            "tightened_shell",
            "[agent]\nmodel='gpt-tightened'\n[shell]\ndefault='approve'\nallow_patterns=['cargo *','ls *','pwd','which *','date','uname *']\ndeny_patterns=['rm -rf /*','rm -rf ~*','curl * | sh*','wget * | sh*','> /dev/sd*']\ndefault_severity='medium'\n",
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
            "[agent]\nmodel='gpt-tightened'\n[shell]\ndefault='approve'\nallow_patterns=['cargo *','ls *','pwd','which *','date','uname *']\ndeny_patterns=['rm -rf /*','rm -rf ~*','curl * | sh*','wget * | sh*','> /dev/sd*']\ndefault_severity='medium'\n",
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
        let path = temp_toml_path("minimal", "[agent]\nmodel='gpt-minimal'\n");

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.model, "gpt-minimal");
        assert_eq!(
            config.system_prompt,
            "You are a direct and capable coding agent. Execute tasks efficiently."
        );
        assert_default_shell_policy(&config.shell_policy);
        assert_eq!(config.budget, None);
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
    }

    #[test]
    fn uses_defaults_for_missing_optional_fields() {
        let path = temp_toml_path("missing_optional", "[agent]\nmodel='gpt-only'\n");

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
    }

    #[test]
    fn loads_session_name_from_agents_toml() {
        let path = temp_toml_path("session_name", "[agent]\nsession_name='default-work'\n");

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.session_name, Some("default-work".to_string()));
    }

    #[test]
    fn malformed_toml_returns_error() {
        let path = temp_toml_path("malformed", "[agent]\nmodel = ");

        let result = Config::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn loads_operator_key_from_auth_section() {
        let path = temp_toml_path(
            "operator_key",
            "[agent]\nmodel='gpt-auth'\n[auth]\noperator_key='operator-from-file'\n",
        );

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.operator_key, Some("operator-from-file".to_string()));
    }

    #[test]
    fn loads_budget_config_with_all_fields() {
        let path = temp_toml_path(
            "budget_all",
            "[agent]\nmodel='gpt-budget'\n[budget]\nmax_tokens_per_turn=100\nmax_tokens_per_session=200\nmax_tokens_per_day=300\n",
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
    }

    #[test]
    fn loads_budget_config_with_partial_fields() {
        let path = temp_toml_path(
            "budget_partial",
            "[agent]\nmodel='gpt-budget'\n[budget]\nmax_tokens_per_session=250\n",
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
    }

    #[test]
    fn missing_budget_table_keeps_budget_none() {
        let path = temp_toml_path("budget_missing", "[agent]\nmodel='gpt-budget'\n");

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.budget, None);
    }
}

#[derive(Debug, Deserialize)]
struct AgentFileConfig {
    agent: AgentFileSection,
    auth: Option<AuthFileSection>,
    #[serde(default)]
    shell: ShellPolicy,
    budget: Option<BudgetConfig>,
}

#[derive(Debug, Deserialize)]
struct AgentFileSection {
    model: Option<String>,
    system_prompt: Option<String>,
    base_url: Option<String>,
    reasoning_effort: Option<String>,
    session_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthFileSection {
    operator_key: Option<String>,
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
}

impl Default for ShellPolicy {
    fn default() -> Self {
        Self {
            default: "approve".to_string(),
            allow_patterns: Vec::new(),
            deny_patterns: Vec::new(),
            standing_approvals: Vec::new(),
            default_severity: "medium".to_string(),
        }
    }
}
