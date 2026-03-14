use std::path::Path;

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub system_prompt: String,
    pub max_output_tokens: Option<u32>,
    pub base_url: String,
    pub reasoning_effort: Option<String>,
}

impl Config {
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Self {
            model: "gpt-5.4".to_string(),
            system_prompt: "You are a direct and capable coding agent. Execute tasks efficiently.".to_string(),
            max_output_tokens: Some(16384),
            base_url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
            reasoning_effort: None,
        };

        if let Ok(contents) = std::fs::read_to_string(config_path.as_ref()) {
            let file_config: AgentFileConfig = toml::from_str(&contents)
                .map_err(|error| anyhow!("failed to parse agents.toml: {error}"))?;

            if let Some(model) = file_config.agent.model {
                config.model = model;
            }

            if let Some(prompt) = file_config.agent.system_prompt {
                config.system_prompt = prompt;
            }

            if let Some(max_output_tokens) = file_config.agent.max_output_tokens {
                config.max_output_tokens = Some(max_output_tokens);
            }

            if let Some(base_url) = file_config.agent.base_url {
                config.base_url = base_url;
            }

            if let Some(reasoning_effort) = file_config.agent.reasoning_effort {
                config.reasoning_effort = Some(reasoning_effort);
            }
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
    max_output_tokens: Option<u32>,
    base_url: Option<String>,
    reasoning_effort: Option<String>,
}
