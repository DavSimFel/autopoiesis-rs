pub mod budget;
mod command_path_analysis;
pub mod exfil_detector;
pub(crate) mod output_cap;
mod protected_paths;
mod secret_catalog;
pub mod secret_redactor;
pub mod shell_safety;
pub(crate) mod streaming_redact;

use crate::llm::{ChatMessage, MessageContent};
use crate::turn::Turn;

pub use budget::BudgetGuard;
pub use exfil_detector::ExfilDetector;
pub(crate) use output_cap::{DEFAULT_OUTPUT_CAP_BYTES, cap_tool_output};
pub(crate) use protected_paths::path_is_protected;
#[cfg(test)]
pub(crate) use secret_catalog::SECRET_PATTERNS;
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
    // Policy: denied outbound text collapses to empty output instead of partially persisting unsafe content.
    let mut text = text;
    match turn.check_text_delta(&mut text) {
        Verdict::Deny { .. } => String::new(),
        Verdict::Allow | Verdict::Modify | Verdict::Approve { .. } => text,
    }
}

/// Guard message content before persistence.
pub(crate) fn guard_message_output(turn: &Turn, message: &mut ChatMessage) {
    // Policy: message text and tool-call arguments are guarded before persistence.
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
            serde_json::to_string(&value)
                .unwrap_or_else(|_| "{\"redacted\":\"[REDACTED]\"}".to_string())
        }
        Err(_) => serde_json::json!({
            "redacted": guard_text_output(turn, arguments)
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests;
