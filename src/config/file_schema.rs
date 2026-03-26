use std::path::PathBuf;

use serde::Deserialize;

use super::{
    AgentsConfig, BudgetConfig, DomainsConfig, ModelsConfig, QueueConfig, ReadToolConfig,
    ShellPolicy, SubscriptionsConfig,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFileConfig {
    #[serde(default)]
    pub agents: Option<AgentsConfig>,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub domains: DomainsConfig,
    #[serde(default)]
    pub skills_dir: Option<PathBuf>,
    pub auth: Option<AuthFileSection>,
    #[serde(default)]
    pub shell: ShellPolicy,
    pub budget: Option<BudgetConfig>,
    #[serde(default)]
    pub read: ReadToolConfig,
    #[serde(default)]
    pub subscriptions: SubscriptionsConfig,
    #[serde(default)]
    pub queue: QueueConfig,
}

#[derive(Debug, Deserialize)]
pub struct AuthFileSection {
    pub operator_key: Option<String>,
}
