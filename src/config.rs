//! Configuration loading for runtime defaults and optional `agents.toml` overrides.

use std::path::Path;

use anyhow::{anyhow, Result};
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
            system_prompt: "You are a direct and capable coding agent. Execute tasks efficiently.".to_string(),
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
