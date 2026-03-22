pub mod budget;
pub mod exfil_detector;
pub(crate) mod output_cap;
pub(crate) mod secret_patterns;
pub mod secret_redactor;
pub mod shell_safety;
pub(crate) mod streaming_redact;

use crate::llm::{ChatMessage, MessageContent};
use crate::turn::Turn;

pub use budget::BudgetGuard;
pub use exfil_detector::ExfilDetector;
pub(crate) use output_cap::{DEFAULT_OUTPUT_CAP_BYTES, cap_tool_output};
pub use secret_redactor::SecretRedactor;
pub use shell_safety::ShellSafety;
pub(crate) use streaming_redact::StreamingTextBuffer;

/// Severity level when execution needs explicit approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Low,
    Medium,
    High,
}

/// Guard decision for an evaluated event.
#[derive(Clone, Debug)]
pub enum Verdict {
    Allow,
    Approve {
        reason: String,
        gate_id: String,
        severity: Severity,
    },
    Deny {
        reason: String,
        gate_id: String,
    },
    Modify,
}

/// Events passed to guards during turn lifecycle.
pub enum GuardEvent<'a> {
    Inbound(&'a mut Vec<ChatMessage>),
    ToolCall(&'a crate::llm::ToolCall),
    ToolBatch(&'a [crate::llm::ToolCall]),
    TextDelta(&'a mut String),
}

/// Budget counters shared with guards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub turn_tokens: u64,
    pub session_tokens: u64,
    pub day_tokens: u64,
}

/// Shared guard context for a turn evaluation.
#[derive(Clone, Copy, Debug, Default)]
pub struct GuardContext {
    pub tainted: bool,
    pub budget: BudgetSnapshot,
}

/// Generic guard interface for inbound and outbound checks.
pub trait Guard: Send + Sync {
    fn name(&self) -> &str;
    fn check(&self, event: &mut GuardEvent, context: &GuardContext) -> Verdict;
}

/// Guard outbound text emitted by the current turn.
pub(crate) fn guard_text_output(turn: &Turn, text: String) -> String {
    let mut text = text;
    match turn.check_text_delta(&mut text) {
        Verdict::Deny { .. } => String::new(),
        Verdict::Allow | Verdict::Modify | Verdict::Approve { .. } => text,
    }
}

/// Guard assistant message content before persistence.
pub(crate) fn guard_message_output(turn: &Turn, message: &mut ChatMessage) {
    for block in &mut message.content {
        if let MessageContent::Text { text } = block {
            *text = guard_text_output(turn, std::mem::take(text));
        }
    }

    message
        .content
        .retain(|block| !matches!(block, MessageContent::Text { text } if text.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GuardContext;
    use crate::llm::{ChatMessage, ChatRole, MessageContent, ToolCall};
    use crate::principal::Principal;
    use crate::turn::Turn;
    use serde_json::json;

    struct DenyTextGuard;

    impl Guard for DenyTextGuard {
        fn name(&self) -> &str {
            "deny-text"
        }

        fn check(&self, event: &mut GuardEvent, _context: &GuardContext) -> Verdict {
            match event {
                GuardEvent::TextDelta(_) => Verdict::Deny {
                    reason: "blocked".to_string(),
                    gate_id: "deny-text".to_string(),
                },
                _ => Verdict::Allow,
            }
        }
    }

    #[test]
    fn guard_text_output_returns_empty_string_on_deny() {
        let turn = Turn::new().guard(DenyTextGuard);
        assert!(guard_text_output(&turn, "secret".to_string()).is_empty());
    }

    #[test]
    fn guard_message_output_removes_empty_text_blocks_and_keeps_tool_blocks() {
        let turn = Turn::new().guard(DenyTextGuard);
        let mut message = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![
                MessageContent::text("secret"),
                MessageContent::ToolCall {
                    call: ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: json!({"command":"echo ok"}).to_string(),
                    },
                },
                MessageContent::tool_result("call-1", "execute", "kept"),
            ],
        };

        guard_message_output(&turn, &mut message);

        assert_eq!(message.content.len(), 2);
        assert!(matches!(
            message.content[0],
            MessageContent::ToolCall { .. }
        ));
        match &message.content[1] {
            MessageContent::ToolResult { result } => {
                assert_eq!(result.content, "kept");
            }
            _ => panic!("expected tool result"),
        }
    }
}
