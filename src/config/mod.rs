//! Configuration loading for runtime defaults and optional `agents.toml` overrides.

mod agents;
mod domains;
mod file_schema;
mod load;
mod models;
mod policy;
mod runtime;
mod spawn_runtime;

// Invariant: load.rs owns file/env/default precedence while spawn_runtime.rs owns child-runtime overlay precedence.
// Policy: agents.rs, domains.rs, and load.rs own trusted-path validation and skills_dir resolution boundaries.
pub use agents::{
    AgentDefinition, AgentTierConfig, AgentsConfig, select_active_agent, validate_agent_identity,
};
pub use domains::{DomainConfig, DomainsConfig, validate_domain_context_extend};
pub use file_schema::{AuthFileSection, RuntimeFileConfig};
pub use load::ConfigError;
pub use models::{ModelDefinition, ModelRoute, ModelsConfig};
pub use policy::{
    ShellDefaultAction, ShellDefaultSeverity, ShellPolicy, validate_read_tool_config,
    validate_subscriptions_config,
};
pub use runtime::{BudgetConfig, Config, QueueConfig, ReadToolConfig, SubscriptionsConfig};
pub(crate) const DEFAULT_SHELL_MAX_OUTPUT_BYTES: usize = runtime::DEFAULT_SHELL_MAX_OUTPUT_BYTES;
pub(crate) const DEFAULT_SHELL_MAX_TIMEOUT_MS: u64 = runtime::DEFAULT_SHELL_MAX_TIMEOUT_MS;
pub(crate) const DEFAULT_STALE_PROCESSING_TIMEOUT_SECS: u64 =
    runtime::DEFAULT_STALE_PROCESSING_TIMEOUT_SECS;
#[cfg(all(test, not(clippy)))]
pub(crate) use load::with_config_load_lock;

#[cfg(test)]
mod tests;
