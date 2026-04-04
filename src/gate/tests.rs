#![cfg(not(clippy))]

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
