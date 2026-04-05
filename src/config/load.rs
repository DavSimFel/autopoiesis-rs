#[cfg(all(test, not(clippy)))]
use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
#[cfg(all(test, not(clippy)))]
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use thiserror::Error;

use crate::identity;
use crate::skills::SkillCatalog;

use super::agents::{AgentsConfig, select_active_agent, validate_agent_identity};
use super::domains::{DomainsConfig, validate_domain_context_extend};
use super::file_schema::RuntimeFileConfig;
use super::models::ModelsConfig;
use super::policy::{ShellPolicy, validate_read_tool_config, validate_subscriptions_config};
use super::runtime::{Config, QueueConfig, ReadToolConfig, SubscriptionsConfig};

#[cfg(all(test, not(clippy)))]
static CONFIG_LOAD_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(test, not(clippy)))]
thread_local! {
    static SKIP_CONFIG_LOAD_LOCK: Cell<bool> = const { Cell::new(false) };
}

#[cfg(all(test, not(clippy)))]
pub(crate) fn with_config_load_lock<T>(f: impl FnOnce() -> T) -> T {
    struct ResetFlag<'a> {
        cell: &'a Cell<bool>,
        previous: bool,
    }

    impl Drop for ResetFlag<'_> {
        fn drop(&mut self) {
            self.cell.set(self.previous);
        }
    }

    SKIP_CONFIG_LOAD_LOCK.with(|skip| {
        let previous = skip.replace(true);
        let _reset = ResetFlag {
            cell: skip,
            previous,
        };
        let _guard = CONFIG_LOAD_LOCK
            .lock()
            .expect("config load lock should be available");
        f()
    })
}

/// Typed boundary error for config loading and spawned-child runtime reconstruction.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to parse agents.toml: {source}")]
    ParseAgents {
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid child tier: {tier}")]
    InvalidSpawnTier { tier: String },
    #[error("spawned child config requires an active agent")]
    MissingActiveAgent,
    #[error("spawned child config missing active agent entry")]
    MissingActiveAgentEntry,
    #[error("spawned child config missing active agent definition")]
    MissingActiveAgentDefinition,
    #[error("{message}: {source}")]
    Validation {
        message: String,
        #[source]
        source: anyhow::Error,
    },
}

impl ConfigError {
    pub(crate) fn validation(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::Validation {
            source: anyhow!(message.clone()),
            message,
        }
    }

    pub(crate) fn validation_with_source(
        message: impl Into<String>,
        source: anyhow::Error,
    ) -> Self {
        Self::Validation {
            message: message.into(),
            source,
        }
    }
}

impl Config {
    /// Load configuration from a file, falling back to sensible defaults.
    pub fn load(config_path: impl AsRef<Path>) -> Result<Self> {
        Self::load_typed(config_path).map_err(anyhow::Error::new)
    }

    /// Typed variant of [`Config::load`].
    pub fn load_typed(config_path: impl AsRef<Path>) -> std::result::Result<Self, ConfigError> {
        Self::from_file_typed(config_path)
    }

    /// Compatibility wrapper for callers that prefer the older name.
    pub fn from_file(config_path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file_typed(config_path).map_err(anyhow::Error::new)
    }

    /// Typed variant of [`Config::from_file`].
    pub fn from_file_typed(
        config_path: impl AsRef<Path>,
    ) -> std::result::Result<Self, ConfigError> {
        #[cfg(all(test, not(clippy)))]
        let _guard = SKIP_CONFIG_LOAD_LOCK.with(|skip| {
            if skip.get() {
                None
            } else {
                Some(
                    CONFIG_LOAD_LOCK
                        .lock()
                        .expect("config load lock should be available"),
                )
            }
        });

        // Invariant: defaults are established first, then file values replace them, and env overrides win last.
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
            subscriptions: SubscriptionsConfig::default(),
            queue: QueueConfig::default(),
            identity_files: identity::t1_identity_files(
                crate::paths::DEFAULT_IDENTITY_TEMPLATES_DIR,
                "silas",
            ),
            agents: AgentsConfig::default(),
            models: ModelsConfig::default(),
            domains: DomainsConfig::default(),
            skills_dir: crate::paths::default_skills_dir(),
            skills_dir_resolved: crate::paths::default_skills_dir(),
            skills: SkillCatalog::default(),
            active_agent: None,
        };

        let config_path = config_path.as_ref();
        let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

        let contents = match std::fs::read_to_string(config_path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                config.skills =
                    SkillCatalog::load_from_dir(&config.skills_dir).map_err(|source| {
                        ConfigError::validation_with_source("failed to load skills catalog", source)
                    })?;
                return Ok(config);
            }
            Err(error) => {
                return Err(ConfigError::ReadFile {
                    path: config_path.to_path_buf(),
                    source: error,
                });
            }
        };

        let file_config: RuntimeFileConfig =
            toml::from_str(&contents).map_err(|source| ConfigError::ParseAgents { source })?;

        if file_config.agents.is_none() {
            return Err(ConfigError::validation(
                "agents.toml must define at least one [agents.*] table",
            ));
        }

        // Invariant: file-backed values are validated after the merged config is assembled.
        config.models = file_config.models;
        config.domains = file_config.domains;
        validate_read_tool_config(&file_config.read).map_err(|source| {
            ConfigError::validation_with_source("invalid [read] config", source)
        })?;
        config.read = file_config.read;
        validate_subscriptions_config(&file_config.subscriptions).map_err(|source| {
            ConfigError::validation_with_source("invalid [subscriptions] config", source)
        })?;
        config.subscriptions = file_config.subscriptions;

        if let Some(agents) = file_config.agents {
            let (active_name, active_agent) = select_active_agent(&agents).map_err(|source| {
                ConfigError::validation_with_source("invalid active agent selection", source)
            })?;
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
            validate_agent_identity(&selected_identity).map_err(|source| {
                ConfigError::validation_with_source("invalid active agent identity", source)
            })?;

            config.identity_files = if use_t2_files {
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
        let skills_dir = file_config
            .skills_dir
            .unwrap_or_else(crate::paths::default_skills_dir);
        // Policy: relative skills_dir values resolve against the config file directory; absolute values stay absolute.
        let resolved_skills_dir = if skills_dir.is_relative() {
            config_dir.join(&skills_dir)
        } else {
            skills_dir.clone()
        };
        config.skills_dir = skills_dir;
        config.skills_dir_resolved = resolved_skills_dir.clone();
        config.skills =
            SkillCatalog::load_from_dir(&config.skills_dir_resolved).map_err(|source| {
                ConfigError::validation_with_source("failed to load skills catalog", source)
            })?;

        // Invariant: environment-provided operator key wins over file-provided operator key.
        if let Ok(operator_key) = std::env::var("AUTOPOIESIS_OPERATOR_KEY") {
            config.operator_key = Some(operator_key);
        }

        Ok(config)
    }
}
