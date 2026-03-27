use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct AgentsConfig {
    #[serde(flatten, default)]
    pub entries: HashMap<String, AgentDefinition>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
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

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
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

impl AgentTierConfig {
    pub fn is_configured(&self) -> bool {
        self.model.is_some()
            || self.base_url.is_some()
            || self.system_prompt.is_some()
            || self.session_name.is_some()
            || self.reasoning.is_some()
            || self.reasoning_effort.is_some()
            || self.delegation_token_threshold.is_some()
            || self.delegation_tool_depth.is_some()
    }
}

/// v2 currently supports exactly one active agent entry. If multiple named
/// agents are added, configuration should be extended with an explicit
/// selector before startup tries to resolve them.
pub fn select_active_agent(agents: &AgentsConfig) -> Result<(String, AgentDefinition)> {
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

pub fn validate_agent_identity(identity: &str) -> Result<()> {
    if identity.is_empty() {
        return Err(anyhow!("agent identity must not be empty"));
    }
    if identity == "." || identity == ".." || identity.chars().any(std::path::is_separator) {
        return Err(anyhow!("agent identity must be a single path segment"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_agent_identity;

    #[test]
    fn rejects_empty_and_reserved_and_separator_containing_identity_segments() {
        assert!(validate_agent_identity("").is_err());
        assert!(validate_agent_identity(".").is_err());
        assert!(validate_agent_identity("..").is_err());
        assert!(validate_agent_identity("silas/nested").is_err());
    }

    #[test]
    fn accepts_single_path_segment_identity() {
        assert!(validate_agent_identity("silas").is_ok());
    }
}
