use std::collections::HashMap;

use anyhow::{Result, anyhow};

use crate::config::{AgentDefinition, AgentTierConfig, Config};

/// Reject session IDs containing path traversal or unsafe characters.
pub fn is_valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub session_id: String,
    pub tier: String,
    pub config: Config,
    pub description: String,
    pub always_on: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    specs: HashMap<String, SessionSpec>,
}

impl SessionSpec {
    pub fn is_queue_owned(&self) -> bool {
        self.always_on
    }

    pub fn is_request_owned(&self) -> bool {
        !self.is_queue_owned()
    }
}

impl SessionRegistry {
    pub fn from_config(config: &Config) -> Result<Self> {
        let Some(active_name) = config.active_agent.as_deref() else {
            return Ok(Self::default());
        };
        let Some(active_agent) = config.active_agent_definition() else {
            return Err(anyhow!(
                "active agent '{active_name}' is missing from agents config"
            ));
        };

        let mut specs: HashMap<String, SessionSpec> = HashMap::new();
        for (tier, tier_config) in [("t1", &active_agent.t1), ("t2", &active_agent.t2)] {
            if !tier_config.is_configured() {
                continue;
            }

            let session_id = tier_session_id(active_name, tier, tier_config);
            validate_registry_session_id(active_name, tier, &session_id)?;
            if let Some(existing) = specs.get(&session_id) {
                return Err(anyhow!(
                    "active agent '{active_name}' tier '{tier}' reuses session id '{session_id}' already claimed by tier '{}'",
                    existing.tier
                ));
            }
            let description = tier_description(tier);
            let tier_model = tier_model(config, active_agent, tier_config);
            let session_config = config.with_spawned_child_runtime(tier, &tier_model, None)?;
            specs.insert(
                session_id.clone(),
                SessionSpec {
                    session_id,
                    tier: tier.to_string(),
                    config: session_config,
                    description: description.to_string(),
                    always_on: true,
                },
            );
        }

        Ok(Self { specs })
    }

    pub fn get(&self, session_id: &str) -> Option<&SessionSpec> {
        self.specs.get(session_id)
    }

    pub fn sessions(&self) -> Vec<&SessionSpec> {
        let mut sessions = self.specs.values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        sessions
    }

    pub fn always_on_sessions(&self) -> Vec<&SessionSpec> {
        self.sessions()
            .into_iter()
            .filter(|spec| spec.is_queue_owned())
            .collect()
    }

    pub fn default_request_owned_session(&self) -> Option<&SessionSpec> {
        self.sessions()
            .into_iter()
            .find(|spec| spec.is_request_owned())
    }
}

fn validate_registry_session_id(active_name: &str, tier: &str, session_id: &str) -> Result<()> {
    if is_valid_session_id(session_id) {
        return Ok(());
    }

    Err(anyhow!(
        "active agent '{active_name}' tier '{tier}' produced invalid session id '{session_id}'"
    ))
}

fn tier_model(
    config: &Config,
    active_agent: &AgentDefinition,
    tier_config: &AgentTierConfig,
) -> String {
    tier_config
        .model
        .clone()
        .or_else(|| active_agent.model.clone())
        .unwrap_or_else(|| config.model.clone())
}

fn tier_description(tier: &str) -> &'static str {
    match tier {
        "t1" => "Fast operator-facing tier (shell)",
        "t2" => "Deep analysis tier (read_file, planning)",
        _ => "Always-on session",
    }
}

fn tier_session_id(active_name: &str, tier: &str, tier_config: &AgentTierConfig) -> String {
    tier_config
        .session_name
        .clone()
        .unwrap_or_else(|| format!("{active_name}-{tier}"))
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;

    use crate::config::{
        AgentDefinition, AgentTierConfig, BudgetConfig, DomainsConfig, ModelsConfig, QueueConfig,
        ReadToolConfig, ShellPolicy, SubscriptionsConfig,
    };
    use crate::identity;
    use crate::skills::SkillCatalog;
    use std::path::PathBuf;

    fn base_config() -> Config {
        let mut agents = crate::config::AgentsConfig::default();
        agents.entries.insert(
            "silas".to_string(),
            AgentDefinition {
                identity: Some("silas".to_string()),
                tier: None,
                model: Some("gpt-5.4-mini".to_string()),
                base_url: Some("https://example.test/api".to_string()),
                system_prompt: Some("legacy defaults".to_string()),
                session_name: Some("legacy-session".to_string()),
                reasoning_effort: Some("medium".to_string()),
                t1: AgentTierConfig {
                    model: Some("gpt-5.4-mini".to_string()),
                    base_url: None,
                    system_prompt: Some("t1 prompt".to_string()),
                    session_name: Some("silas-t1".to_string()),
                    reasoning: None,
                    reasoning_effort: None,
                    delegation_token_threshold: None,
                    delegation_tool_depth: None,
                },
                t2: AgentTierConfig {
                    model: Some("o3".to_string()),
                    base_url: None,
                    system_prompt: Some("t2 prompt".to_string()),
                    session_name: Some("silas-t2".to_string()),
                    reasoning: Some("high".to_string()),
                    reasoning_effort: None,
                    delegation_token_threshold: None,
                    delegation_tool_depth: None,
                },
            },
        );

        Config {
            model: "gpt-5.4-mini".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: Some("legacy-session".to_string()),
            operator_key: None,
            shell_policy: ShellPolicy::default(),
            budget: Some(BudgetConfig {
                max_tokens_per_turn: None,
                max_tokens_per_session: None,
                max_tokens_per_day: None,
            }),
            read: ReadToolConfig::default(),
            subscriptions: SubscriptionsConfig::default(),
            queue: QueueConfig::default(),
            identity_files: identity::t1_identity_files("identity-templates", "silas"),
            agents,
            models: ModelsConfig::default(),
            domains: DomainsConfig::default(),
            skills_dir: PathBuf::from("skills"),
            skills_dir_resolved: PathBuf::from("skills"),
            skills: SkillCatalog::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    #[test]
    fn from_config_materializes_t1_and_t2_sessions() {
        let registry = SessionRegistry::from_config(&base_config()).unwrap();
        let sessions = registry.always_on_sessions();

        assert_eq!(
            sessions
                .iter()
                .map(|spec| spec.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["silas-t1", "silas-t2"]
        );

        let t1 = registry.get("silas-t1").unwrap();
        assert_eq!(t1.tier, "t1");
        assert!(t1.always_on);
        assert!(t1.is_queue_owned());
        assert!(!t1.is_request_owned());
        assert_eq!(t1.description, "Fast operator-facing tier (shell)");
        assert_eq!(
            t1.config
                .active_agent_definition()
                .and_then(|agent| agent.tier.as_deref()),
            Some("t1")
        );

        let t2 = registry.get("silas-t2").unwrap();
        assert_eq!(t2.tier, "t2");
        assert!(t2.always_on);
        assert!(t2.is_queue_owned());
        assert!(!t2.is_request_owned());
        assert_eq!(t2.description, "Deep analysis tier (read_file, planning)");
        assert_eq!(
            t2.config
                .active_agent_definition()
                .and_then(|agent| agent.tier.as_deref()),
            Some("t2")
        );
        assert!(registry.default_request_owned_session().is_none());
    }

    #[test]
    fn from_config_is_empty_without_active_agent() {
        let registry = SessionRegistry::from_config(&Config {
            active_agent: None,
            ..base_config()
        })
        .unwrap();

        assert!(registry.always_on_sessions().is_empty());
        assert!(registry.get("silas-t1").is_none());
    }

    #[test]
    fn from_config_skips_unconfigured_tiers() {
        let mut config = base_config();
        config.agents.entries.get_mut("silas").unwrap().t2 = AgentTierConfig::default();

        let registry = SessionRegistry::from_config(&config).unwrap();
        assert_eq!(
            registry
                .always_on_sessions()
                .iter()
                .map(|spec| spec.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["silas-t1"]
        );
        assert!(registry.get("silas-t2").is_none());
    }

    #[test]
    fn from_config_uses_configured_tier_session_names() {
        let mut config = base_config();
        config
            .agents
            .entries
            .get_mut("silas")
            .unwrap()
            .t1
            .session_name = Some("custom-t1".to_string());

        let registry = SessionRegistry::from_config(&config).unwrap();
        assert!(registry.get("custom-t1").is_some());
        assert!(registry.get("silas-t1").is_none());
    }

    #[test]
    fn from_config_rejects_invalid_configured_session_name() {
        let mut config = base_config();
        config
            .agents
            .entries
            .get_mut("silas")
            .unwrap()
            .t1
            .session_name = Some("invalid/session".to_string());

        let err = SessionRegistry::from_config(&config).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("active agent 'silas'"));
        assert!(message.contains("tier 't1'"));
        assert!(message.contains("invalid session id 'invalid/session'"));
    }

    #[test]
    fn from_config_rejects_duplicate_explicit_session_names() {
        let mut config = base_config();
        config
            .agents
            .entries
            .get_mut("silas")
            .unwrap()
            .t2
            .session_name = Some("silas-t1".to_string());

        let err = SessionRegistry::from_config(&config).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("active agent 'silas'"));
        assert!(message.contains("tier 't2'"));
        assert!(message.contains("reuses session id 'silas-t1'"));
        assert!(message.contains("tier 't1'"));
    }

    #[test]
    fn from_config_rejects_duplicate_implicit_and_explicit_session_names() {
        let mut config = base_config();
        let agent = config.agents.entries.get_mut("silas").unwrap();
        agent.t1.session_name = None;
        agent.t2.session_name = Some("silas-t1".to_string());

        let err = SessionRegistry::from_config(&config).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("active agent 'silas'"));
        assert!(message.contains("tier 't2'"));
        assert!(message.contains("reuses session id 'silas-t1'"));
        assert!(message.contains("tier 't1'"));
    }

    #[test]
    fn session_id_validation_matches_server_rules() {
        assert!(is_valid_session_id("silas-t1"));
        assert!(is_valid_session_id("session_123"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("contains/slash"));
        assert!(!is_valid_session_id("contains space"));
        assert!(!is_valid_session_id(&"a".repeat(129)));
    }
}
