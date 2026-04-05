use std::path::PathBuf;

use serde::Deserialize;

use crate::skills::SkillCatalog;

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
    pub shell_policy: super::ShellPolicy,
    /// Optional budget ceilings loaded from the optional `[budget]` table.
    pub budget: Option<BudgetConfig>,
    /// Optional structured read policy loaded from the optional `[read]` table.
    pub read: ReadToolConfig,
    /// Optional subscription context policy loaded from the optional `[subscriptions]` table.
    pub subscriptions: SubscriptionsConfig,
    /// Queue recovery settings loaded from the optional `[queue]` table.
    pub queue: QueueConfig,
    /// Resolved identity prompt files used to assemble the system prompt.
    pub identity_files: Vec<PathBuf>,
    /// Parsed `[agents]` catalog, if present.
    pub agents: super::AgentsConfig,
    /// Parsed `[models]` catalog and routes.
    pub models: super::ModelsConfig,
    /// Parsed `[domains]` packs.
    pub domains: super::DomainsConfig,
    /// Directory containing local TOML skill definitions.
    pub skills_dir: PathBuf,
    /// Directory used to load local TOML skill definitions from disk.
    pub skills_dir_resolved: PathBuf,
    /// Loaded local skill catalog.
    pub skills: SkillCatalog,
    /// Selected named brain, if v2 config is active.
    pub active_agent: Option<String>,
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

/// Queue recovery defaults loaded from `[queue]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct QueueConfig {
    /// Requeue `processing` rows only after this many seconds since claim.
    #[serde(default = "default_stale_processing_timeout_secs")]
    pub stale_processing_timeout_secs: u64,
}

/// Structured read tool policy loaded from `[read]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReadToolConfig {
    /// Root-relative paths allowed for structured file reads.
    pub allowed_paths: Vec<String>,
    /// Maximum bytes returned by one read.
    pub max_read_bytes: usize,
}

/// Subscription context policy loaded from `[subscriptions]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SubscriptionsConfig {
    /// Maximum tokens to spend materializing subscription content into a turn.
    pub context_token_budget: usize,
}

pub(crate) const DEFAULT_SHELL_MAX_OUTPUT_BYTES: usize = 1_048_576;
pub(crate) const DEFAULT_SHELL_MAX_TIMEOUT_MS: u64 = 120_000;
pub(crate) const DEFAULT_STALE_PROCESSING_TIMEOUT_SECS: u64 = 300;

pub(crate) const fn default_shell_max_output_bytes() -> usize {
    super::DEFAULT_SHELL_MAX_OUTPUT_BYTES
}

pub(crate) const fn default_shell_max_timeout_ms() -> u64 {
    super::DEFAULT_SHELL_MAX_TIMEOUT_MS
}

pub(crate) const fn default_stale_processing_timeout_secs() -> u64 {
    super::DEFAULT_STALE_PROCESSING_TIMEOUT_SECS
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            stale_processing_timeout_secs: super::DEFAULT_STALE_PROCESSING_TIMEOUT_SECS,
        }
    }
}

impl Default for ReadToolConfig {
    fn default() -> Self {
        Self {
            allowed_paths: vec![crate::paths::DEFAULT_IDENTITY_TEMPLATES_DIR.to_string()],
            max_read_bytes: 65_536,
        }
    }
}

impl Default for SubscriptionsConfig {
    fn default() -> Self {
        Self {
            context_token_budget: 4_096,
        }
    }
}

impl Config {
    /// Resolve the active agent definition, if v2 config loaded successfully.
    pub fn active_agent_definition(&self) -> Option<&super::AgentDefinition> {
        let active_name = self.active_agent.as_ref()?;
        self.agents.entries.get(active_name)
    }

    /// Resolve the active T1 tier config for the current brain, if available.
    pub fn active_t1_config(&self) -> Option<&super::AgentTierConfig> {
        self.active_agent_definition().map(|agent| &agent.t1)
    }
}
