use std::env;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: Option<u32>,
    pub openai_api_key: Option<String>,
    pub base_url: String,
}

impl Config {
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self> {
        dotenvy::dotenv().ok();

        let mut config = Self {
            model: "gpt-4o".to_string(),
            system_prompt: "You are a direct and capable coding agent. Execute tasks efficiently.".to_string(),
            max_tokens: Some(8192),
            openai_api_key: env::var("OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| env::var("OPENROUTER_API_KEY").ok())
                .filter(|v| !v.trim().is_empty()),
            base_url: env::var("BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        };

        if let Ok(contents) = std::fs::read_to_string(config_path.as_ref()) {
            let file_config: AgentFileConfig = toml::from_str(&contents)
                .context("failed to parse agents.toml")?;
            if let Some(model) = file_config.agent.model {
                config.model = model;
            }
            if let Some(prompt) = file_config.agent.system_prompt {
                config.system_prompt = prompt;
            }
            if let Some(max_tokens) = file_config.agent.max_tokens {
                config.max_tokens = Some(max_tokens);
            }
        }

        Ok(config)
    }

    pub fn openai_api_key(&self) -> Result<String> {
        let key = self
            .openai_api_key
            .clone()
            .unwrap_or_default()
            .replace("\\n", "\n")
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();

        if key.is_empty() {
            Err(anyhow!("OPENAI_API_KEY is not set"))
        } else {
            Ok(key)
        }
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
}
