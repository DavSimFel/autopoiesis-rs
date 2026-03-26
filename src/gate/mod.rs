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
        } else if let MessageContent::ToolCall { call } = block {
            call.arguments = redact_tool_call_arguments(turn, std::mem::take(&mut call.arguments));
        }
    }

    message
        .content
        .retain(|block| !matches!(block, MessageContent::Text { text } if text.is_empty()));
}

fn redact_tool_call_arguments(turn: &Turn, arguments: String) -> String {
    if arguments.is_empty() {
        return arguments;
    }

    match serde_json::from_str::<serde_json::Value>(&arguments) {
        Ok(mut value) => {
            fn redact_value(turn: &Turn, value: &mut serde_json::Value) {
                match value {
                    serde_json::Value::String(text) => {
                        *text = guard_text_output(turn, std::mem::take(text));
                    }
                    serde_json::Value::Array(items) => {
                        for item in items {
                            redact_value(turn, item);
                        }
                    }
                    serde_json::Value::Object(map) => {
                        let mut redacted = serde_json::Map::new();
                        for (key, mut value) in std::mem::take(map) {
                            let redacted_key = guard_text_output(turn, key);
                            redact_value(turn, &mut value);
                            redacted.insert(redacted_key, value);
                        }
                        *map = redacted;
                    }
                    _ => {}
                }
            }

            redact_value(turn, &mut value);
            serde_json::to_string(&value).unwrap_or(arguments)
        }
        Err(_) => serde_json::to_string(&serde_json::json!({
            "redacted": guard_text_output(turn, arguments)
        }))
        .unwrap_or_else(|_| "{\"redacted\":\"\"}".to_string()),
    }
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

    #[test]
    fn guard_message_output_redacts_tool_call_arguments() {
        let turn = Turn::new().guard(
            crate::gate::secret_redactor::SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"])
                .expect("test secret redaction regex should be valid"),
        );
        let mut message = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-1".to_string(),
                    name: "execute".to_string(),
                    arguments: json!({"command":"echo sk-proj-abcdefghijklmnopqrstuvwxyz012345"})
                        .to_string(),
                },
            }],
        };

        guard_message_output(&turn, &mut message);

        match &message.content[0] {
            MessageContent::ToolCall { call } => {
                assert!(call.arguments.contains("[REDACTED]"));
                assert!(
                    !call
                        .arguments
                        .contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345")
                );
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn guard_message_output_redacts_tool_call_argument_keys() {
        let turn = Turn::new().guard(
            crate::gate::secret_redactor::SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"])
                .expect("test secret redaction regex should be valid"),
        );
        let mut message = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-2".to_string(),
                    name: "execute".to_string(),
                    arguments: json!({
                        "sk-proj-abcdefghijklmnopqrstuvwxyz012345": "value"
                    })
                    .to_string(),
                },
            }],
        };

        guard_message_output(&turn, &mut message);

        match &message.content[0] {
            MessageContent::ToolCall { call } => {
                let parsed: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
                let object = parsed.as_object().expect("expected object arguments");
                assert!(!object.contains_key("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
                assert!(object.contains_key("[REDACTED]"));
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn guard_message_output_deny_keeps_tool_call_arguments_valid_json() {
        let turn = Turn::new().guard(DenyTextGuard);
        let mut message = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-1".to_string(),
                    name: "execute".to_string(),
                    arguments: "{\"command\":\"echo secret\"}".to_string(),
                },
            }],
        };

        guard_message_output(&turn, &mut message);

        match &message.content[0] {
            MessageContent::ToolCall { call } => {
                let _parsed: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
                assert!(!call.arguments.contains("secret"));
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn guard_message_output_preserves_valid_json_for_malformed_tool_call_arguments() {
        let turn = Turn::new().guard(DenyTextGuard);
        let mut message = ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "call-3".to_string(),
                    name: "execute".to_string(),
                    arguments: "{not valid json".to_string(),
                },
            }],
        };

        guard_message_output(&turn, &mut message);

        match &message.content[0] {
            MessageContent::ToolCall { call } => {
                let parsed: serde_json::Value = serde_json::from_str(&call.arguments).unwrap();
                let object = parsed.as_object().expect("expected object arguments");
                assert_eq!(
                    object.get("redacted").and_then(|value| value.as_str()),
                    Some("")
                );
            }
            _ => panic!("expected tool call"),
        }
    }
}
