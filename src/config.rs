use std::path::Path;

use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: Option<u32>,
    pub base_url: String,
}

impl Config {
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self> {
        let mut config = Self {
            model: "gpt-4o".to_string(),
            system_prompt: "You are a direct and capable coding agent. Execute tasks efficiently.".to_string(),
            max_tokens: Some(8192),
            base_url: "https://api.openai.com/v1".to_string(),
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

            if let Some(max_tokens) = file_config.agent.max_tokens {
                config.max_tokens = Some(max_tokens);
            }

            if let Some(base_url) = file_config.agent.base_url {
                config.base_url = base_url;
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
    max_tokens: Option<u32>,
    base_url: Option<String>,
}
