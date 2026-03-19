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

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn loads_valid_agents_toml_with_all_fields() {
        let path = temp_toml_path(
            "all_fields",
            "[agent]\nmodel='gpt-5.1'\nsystem_prompt='All good'\nbase_url='https://example.test/api'\nreasoning_effort='low'\n",
        );

        let config = Config::load(&path).expect("expected config to load");
        assert_eq!(config.model, "gpt-5.1");
        assert_eq!(config.system_prompt, "All good");
        assert_eq!(config.base_url, "https://example.test/api");
        assert_eq!(config.reasoning_effort, Some("low".to_string()));
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
    }

    #[test]
    fn malformed_toml_returns_error() {
        let path = temp_toml_path("malformed", "[agent]\nmodel = ");

        let result = Config::load(&path);
        assert!(result.is_err());
    }
}

#[derive(Debug, Deserialize)]
struct AgentFileConfig {
    agent: AgentFileSection,
}

#[derive(Debug, Deserialize)]
struct AgentFileSection {
    model: Option<String>,
    system_prompt: Option<String>,
    base_url: Option<String>,
    reasoning_effort: Option<String>,
}
