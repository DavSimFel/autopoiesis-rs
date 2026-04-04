use crate::session::Session;

pub const DELEGATION_HINT: &str = "Consider delegating to T2 for deeper analysis.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DelegationConfig {
    pub token_threshold: Option<u64>,
    pub tool_depth_threshold: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationAdvice {
    ActDirectly,
    SuggestDelegation { reason: String },
}

pub fn delegation_enabled(config: Option<&DelegationConfig>) -> bool {
    config
        .map(|config| config.token_threshold.is_some() || config.tool_depth_threshold.is_some())
        .unwrap_or(false)
}

pub fn check_delegation(
    session: &Session,
    tool_call_count: usize,
    config: Option<&DelegationConfig>,
) -> DelegationAdvice {
    let Some(config) = config else {
        return DelegationAdvice::ActDirectly;
    };

    let mut reasons = Vec::new();

    if let Some(threshold) = config.token_threshold {
        let token_count = session.session_total_tokens();
        if token_count > threshold {
            reasons.push(format!(
                "session has {token_count} tokens, above threshold {threshold}"
            ));
        }
    }

    if let Some(threshold) = config.tool_depth_threshold {
        let tool_depth = tool_call_count as u32;
        if tool_depth > threshold {
            reasons.push(format!(
                "last turn used {tool_depth} tool calls, above threshold {threshold}"
            ));
        }
    }

    if reasons.is_empty() {
        DelegationAdvice::ActDirectly
    } else {
        DelegationAdvice::SuggestDelegation {
            reason: reasons.join("; "),
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sessions_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&dir).expect("temp dir should be creatable");
        dir
    }

    #[test]
    fn suggests_delegation_when_token_threshold_is_exceeded() {
        let dir = temp_sessions_dir("delegation_tokens");
        let mut session = Session::new(&dir).expect("session should open");
        session
            .append(
                ChatMessage::user("this is enough content to create tokens"),
                Some(crate::llm::TurnMeta {
                    input_tokens: Some(10),
                    ..Default::default()
                }),
            )
            .expect("message should append");

        let advice = check_delegation(
            &session,
            0,
            Some(&DelegationConfig {
                token_threshold: Some(0),
                tool_depth_threshold: None,
            }),
        );

        assert!(matches!(advice, DelegationAdvice::SuggestDelegation { .. }));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn suggests_delegation_when_tool_depth_is_exceeded() {
        let dir = temp_sessions_dir("delegation_tools");
        let session = Session::new(&dir).expect("session should open");

        let advice = check_delegation(
            &session,
            2,
            Some(&DelegationConfig {
                token_threshold: None,
                tool_depth_threshold: Some(1),
            }),
        );

        assert!(matches!(advice, DelegationAdvice::SuggestDelegation { .. }));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn acts_directly_when_thresholds_are_not_exceeded() {
        let dir = temp_sessions_dir("delegation_direct");
        let session = Session::new(&dir).expect("session should open");

        let advice = check_delegation(
            &session,
            0,
            Some(&DelegationConfig {
                token_threshold: Some(u64::MAX),
                tool_depth_threshold: Some(u32::MAX),
            }),
        );

        assert_eq!(advice, DelegationAdvice::ActDirectly);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn uses_cumulative_session_tokens_after_trim() {
        let dir = temp_sessions_dir("delegation_trimmed");
        let mut session = Session::new(&dir).expect("session should open");
        session
            .append(
                ChatMessage::user("first seed message"),
                Some(crate::llm::TurnMeta {
                    input_tokens: Some(8),
                    ..Default::default()
                }),
            )
            .expect("first message should append");
        session
            .append(
                ChatMessage::user("second seed message"),
                Some(crate::llm::TurnMeta {
                    input_tokens: Some(8),
                    ..Default::default()
                }),
            )
            .expect("second message should append");
        session.set_max_context_tokens(1);
        session.ensure_context_within_limit();

        let advice = check_delegation(
            &session,
            0,
            Some(&DelegationConfig {
                token_threshold: Some(10),
                tool_depth_threshold: None,
            }),
        );

        assert!(matches!(advice, DelegationAdvice::SuggestDelegation { .. }));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_delegation_config_is_disabled() {
        assert!(!delegation_enabled(Some(&DelegationConfig::default())));
        assert!(!delegation_enabled(None));
        assert!(delegation_enabled(Some(&DelegationConfig {
            token_threshold: Some(1),
            tool_depth_threshold: None,
        })));
    }
}
