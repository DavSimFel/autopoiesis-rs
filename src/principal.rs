use serde::{Deserialize, Serialize};

/// Trust level attached to a message or queue source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Principal {
    #[default]
    Operator,
    User,
    System,
    Agent,
}

impl Principal {
    /// Only the operator is treated as fully trusted.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Operator)
    }

    /// Whether this principal represents external (untrusted) input that should
    /// taint a conversation. Only User and System are taint sources — Agent
    /// messages (assistant replies, tool results) are internally generated and
    /// should not disable standing approvals in operator-only sessions.
    pub fn is_taint_source(&self) -> bool {
        matches!(self, Self::User | Self::System)
    }

    /// Map a queue source string to a principal.
    pub fn from_source(source: &str) -> Self {
        if source == "cli" {
            return Self::Operator;
        }

        if source.starts_with("agent-") {
            return Self::Agent;
        }

        if source.ends_with("-operator") {
            return Self::Operator;
        }

        if source.ends_with("-user") {
            return Self::User;
        }

        Self::System
    }

    /// Determine the request role allowed for this principal.
    pub fn role_for_request(self, requested_role: Option<&str>) -> &str {
        match self {
            Self::Operator => requested_role.unwrap_or("user"),
            _ => "user",
        }
    }

    /// Build a transport-specific queue source string.
    pub fn source_for_transport(self, transport: &str) -> String {
        let suffix = match self {
            Self::Operator => "operator",
            Self::User => "user",
            Self::System => "system",
            Self::Agent => "agent",
        };
        format!("{transport}-{suffix}")
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::Principal;

    #[test]
    fn source_mapping_is_stable() {
        assert_eq!(Principal::from_source("cli"), Principal::Operator);
        assert_eq!(Principal::from_source("http-operator"), Principal::Operator);
        assert_eq!(Principal::from_source("ws-user"), Principal::User);
        assert_eq!(Principal::from_source("agent-child-123"), Principal::Agent);
        assert_eq!(Principal::from_source("webhook"), Principal::System);
    }

    #[test]
    fn trusted_only_matches_operator() {
        assert!(Principal::Operator.is_trusted());
        assert!(!Principal::User.is_trusted());
        assert!(!Principal::System.is_trusted());
        assert!(!Principal::Agent.is_trusted());
    }

    #[test]
    fn taint_sources_are_user_and_system_only() {
        assert!(!Principal::Operator.is_taint_source());
        assert!(Principal::User.is_taint_source());
        assert!(Principal::System.is_taint_source());
        assert!(!Principal::Agent.is_taint_source());
    }
}
