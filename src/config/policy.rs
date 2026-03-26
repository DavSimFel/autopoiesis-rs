use anyhow::{Result, anyhow};
use serde::Deserialize;

use super::runtime::{default_shell_max_output_bytes, default_shell_max_timeout_ms};

/// Shell execution policy loaded from `[shell]` in `agents.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
#[serde(rename_all = "lowercase")]
pub enum ShellDefaultAction {
    Allow,
    #[default]
    Approve,
}

impl TryFrom<String> for ShellDefaultAction {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(Self::Allow),
            "approve" => Ok(Self::Approve),
            other => Err(format!("invalid shell default action: {other}")),
        }
    }
}

/// Shell severity for unmatched commands loaded from `[shell]`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
#[serde(rename_all = "lowercase")]
pub enum ShellDefaultSeverity {
    Low,
    #[default]
    Medium,
    High,
}

impl TryFrom<String> for ShellDefaultSeverity {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.trim().to_ascii_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(format!("invalid shell severity: {other}")),
        }
    }
}

/// Shell execution policy loaded from `[shell]` in `agents.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ShellPolicy {
    /// "approve" (default) or "allow".
    pub default: ShellDefaultAction,
    /// Glob patterns that are auto-allowed.
    pub allow_patterns: Vec<String>,
    /// Glob patterns that are always denied.
    pub deny_patterns: Vec<String>,
    /// Glob patterns that bypass approval prompts but remain auditable.
    pub standing_approvals: Vec<String>,
    /// Severity for commands that don't match any pattern.
    pub default_severity: ShellDefaultSeverity,
    /// Maximum combined stdout/stderr bytes captured from a shell command.
    #[serde(default = "default_shell_max_output_bytes")]
    pub max_output_bytes: usize,
    /// Maximum shell command runtime allowed, even if the model requests more.
    #[serde(default = "default_shell_max_timeout_ms")]
    pub max_timeout_ms: u64,
}

impl Default for ShellPolicy {
    fn default() -> Self {
        Self {
            default: ShellDefaultAction::Approve,
            allow_patterns: Vec::new(),
            deny_patterns: Vec::new(),
            standing_approvals: Vec::new(),
            default_severity: ShellDefaultSeverity::Medium,
            max_output_bytes: default_shell_max_output_bytes(),
            max_timeout_ms: default_shell_max_timeout_ms(),
        }
    }
}

pub fn validate_read_tool_config(read: &super::ReadToolConfig) -> Result<()> {
    if read.allowed_paths.iter().any(|path| path.trim().is_empty()) {
        return Err(anyhow!("read.allowed_paths entries must not be empty"));
    }

    if read.max_read_bytes == 0 {
        return Err(anyhow!("read.max_read_bytes must be greater than zero"));
    }

    Ok(())
}

pub fn validate_subscriptions_config(subscriptions: &super::SubscriptionsConfig) -> Result<()> {
    if subscriptions.context_token_budget == 0 {
        return Err(anyhow!(
            "subscriptions.context_token_budget must be greater than zero"
        ));
    }

    Ok(())
}
