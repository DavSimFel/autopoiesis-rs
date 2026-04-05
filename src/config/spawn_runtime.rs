use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::identity;

use super::domains::validate_domain_context_extend;
use super::load::ConfigError;
use super::runtime::Config;

pub fn validate_spawn_tier(tier: &str) -> Result<()> {
    match tier {
        "t1" | "t2" | "t3" => Ok(()),
        other => Err(anyhow!("invalid child tier: {other}")),
    }
}

impl Config {
    /// Clone the current runtime config and retarget it for a spawned child session.
    pub fn with_spawned_child_runtime(
        &self,
        tier: &str,
        provider_model: &str,
        reasoning_override: Option<&str>,
    ) -> Result<Self> {
        self.with_spawned_child_runtime_typed(tier, provider_model, reasoning_override)
            .map_err(anyhow::Error::new)
    }

    /// Typed variant of [`Config::with_spawned_child_runtime`].
    pub fn with_spawned_child_runtime_typed(
        &self,
        tier: &str,
        provider_model: &str,
        reasoning_override: Option<&str>,
    ) -> std::result::Result<Self, ConfigError> {
        let mut config = self.clone();
        validate_spawn_tier(tier).map_err(|_| ConfigError::InvalidSpawnTier {
            tier: tier.to_string(),
        })?;

        let parent_reasoning_effort = config.reasoning_effort.clone();
        let parent_session_name = config.session_name.clone();
        let active_name = config
            .active_agent
            .clone()
            .ok_or(ConfigError::MissingActiveAgent)?;

        let selected_identity = {
            let agent = config
                .agents
                .entries
                .get_mut(&active_name)
                .ok_or(ConfigError::MissingActiveAgentEntry)?;
            agent.tier = Some(tier.to_string());
            agent
                .identity
                .clone()
                .unwrap_or_else(|| active_name.clone())
        };

        // Invariant: child identity files follow the same trusted-root rules as load-time config.
        config.identity_files = if matches!(tier, "t2" | "t3") {
            identity::t2_identity_files(crate::paths::DEFAULT_IDENTITY_TEMPLATES_DIR)
        } else {
            identity::t1_identity_files(
                crate::paths::DEFAULT_IDENTITY_TEMPLATES_DIR,
                &selected_identity,
            )
        };
        if !config.domains.entries.is_empty() && config.domains.selected.is_empty() {
            return Err(ConfigError::validation(
                "domains config must select at least one context pack",
            ));
        }
        for domain_name in &config.domains.selected {
            let Some(domain) = config.domains.entries.get(domain_name) else {
                return Err(ConfigError::validation(format!(
                    "selected domain pack is missing from [domains]: {domain_name}"
                )));
            };
            let Some(path) = domain.context_extend.as_ref() else {
                return Err(ConfigError::validation(format!(
                    "selected domain pack is missing context_extend: {domain_name}"
                )));
            };
            validate_domain_context_extend(path).map_err(|source| {
                ConfigError::validation_with_source("invalid domain context_extend", source)
            })?;
            config.identity_files.push(PathBuf::from(path));
        }

        // Invariant: explicit override -> child tier -> agent defaults -> parent fallback.
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
                .ok_or(ConfigError::MissingActiveAgentDefinition)?;
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
                .ok_or(ConfigError::MissingActiveAgentDefinition)?;
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
                .ok_or(ConfigError::MissingActiveAgentDefinition)?;
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
