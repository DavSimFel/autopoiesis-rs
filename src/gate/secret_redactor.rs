use crate::gate::secret_patterns::SECRET_PATTERNS;
use crate::gate::{Guard, GuardContext, GuardEvent, Verdict};
use crate::llm::{ChatMessage, MessageContent};

const SECRET_REDACTOR_ID: &str = "secret-redactor";
pub(crate) const REDACTION_MARKER: &str = "[REDACTED]";

/// Secret redaction guard. Replaces matching substrings with `[REDACTED]`.
pub struct SecretRedactor {
    id: String,
    patterns: Vec<regex::Regex>,
}

impl SecretRedactor {
    pub fn new(patterns: &[&str]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(|pattern| regex::Regex::new(pattern).ok())
            .collect();

        Self {
            id: SECRET_REDACTOR_ID.to_string(),
            patterns,
        }
    }

    pub(crate) fn default_catalog() -> Self {
        let patterns: Vec<&str> = SECRET_PATTERNS
            .iter()
            .map(|pattern| pattern.regex)
            .collect();
        Self::new(&patterns)
    }

    fn redact_text(&self, text: &mut String) -> bool {
        let original = text.clone();
        let mut next = text.clone();

        for pattern in &self.patterns {
            next = pattern.replace_all(&next, REDACTION_MARKER).to_string();
        }

        if next != original {
            *text = next;
            true
        } else {
            false
        }
    }

    fn redact_messages(&self, messages: &mut Vec<ChatMessage>) -> bool {
        let mut edited = false;

        for message in messages {
            for block in &mut message.content {
                match block {
                    MessageContent::Text { text } => {
                        if self.redact_text(text) {
                            edited = true;
                        }
                    }
                    MessageContent::ToolResult { result } => {
                        if self.redact_text(&mut result.content) {
                            edited = true;
                        }
                    }
                    MessageContent::ToolCall { .. } => {}
                }
            }
        }

        edited
    }
}

impl Guard for SecretRedactor {
    fn name(&self) -> &str {
        &self.id
    }

    fn check(&self, event: &mut GuardEvent, _context: &GuardContext) -> Verdict {
        match event {
            GuardEvent::Inbound(messages) => {
                if self.redact_messages(messages) {
                    Verdict::Modify
                } else {
                    Verdict::Allow
                }
            }
            GuardEvent::TextDelta(content) => {
                let mut mutated = String::new();
                mutated.push_str(content);

                if self.redact_text(&mut mutated) {
                    **content = mutated;
                    Verdict::Modify
                } else {
                    Verdict::Allow
                }
            }
            _ => Verdict::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::GuardEvent;
    use crate::gate::secret_patterns::SECRET_PATTERNS;
    use crate::llm::{ChatMessage, ChatRole, MessageContent, ToolResult};
    use crate::principal::Principal;

    fn make_secret_gate() -> SecretRedactor {
        SecretRedactor::default_catalog()
    }

    fn make_messages(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage::user(text)]
    }

    fn secret_string(pattern_index: usize, suffix: &str) -> String {
        format!("{}{}", SECRET_PATTERNS[pattern_index].prefix, suffix)
    }

    #[test]
    fn redacts_openai_api_key() {
        let gate = make_secret_gate();
        let mut messages = make_messages(&secret_string(0, "proj-ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        let mut event = GuardEvent::Inbound(&mut messages);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));
        assert_eq!(
            match &messages[0].content[0] {
                MessageContent::Text { text } => text,
                _ => panic!("expected text content"),
            },
            "[REDACTED]"
        );
    }

    #[test]
    fn redacts_github_pat() {
        let gate = make_secret_gate();
        let mut messages = make_messages(&secret_string(1, "0123456789abcdefghijklmnopqrstuvwxyz"));
        let mut event = GuardEvent::Inbound(&mut messages);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));
        assert_eq!(
            match &messages[0].content[0] {
                MessageContent::Text { text } => text,
                _ => panic!("expected text content"),
            },
            "[REDACTED]"
        );
    }

    #[test]
    fn redacts_aws_key() {
        let gate = make_secret_gate();
        let mut messages = make_messages(&secret_string(2, "1234567890ABCDEF"));
        let mut event = GuardEvent::Inbound(&mut messages);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));
        assert_eq!(
            match &messages[0].content[0] {
                MessageContent::Text { text } => text,
                _ => panic!("expected text content"),
            },
            "[REDACTED]"
        );
    }

    #[test]
    fn preserves_normal_text() {
        let gate = make_secret_gate();
        let mut messages = make_messages("hello world");
        let mut event = GuardEvent::Inbound(&mut messages);
        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
    }

    #[test]
    fn redacts_in_both_directions() {
        let inbound_gate = make_secret_gate();
        let outbound_gate = make_secret_gate();

        let mut inbound = make_messages(&secret_string(2, "1234567890ABCDEF"));
        let mut outbound = make_messages(&secret_string(2, "1234567890ABCDEF"));
        let mut inbound_event = GuardEvent::Inbound(&mut inbound);
        let mut outbound_event = GuardEvent::Inbound(&mut outbound);

        assert!(matches!(
            inbound_gate.check(&mut inbound_event, &GuardContext::default()),
            Verdict::Modify
        ));
        assert!(matches!(
            outbound_gate.check(&mut outbound_event, &GuardContext::default()),
            Verdict::Modify
        ));
    }

    #[test]
    fn redacts_multiple_secrets_in_one_message() {
        let gate = make_secret_gate();
        let mut messages = make_messages(&format!(
            "token {} and github {}",
            secret_string(0, "proj-ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            secret_string(1, "0123456789abcdefghijklmnopqrstuvwxyz")
        ));
        let mut event = GuardEvent::Inbound(&mut messages);
        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));

        let redacted = match &messages[0].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text"),
        };
        assert!(!redacted.contains("sk-proj-"));
        assert!(!redacted.contains("ghp_"));
    }

    #[test]
    fn redacts_tool_result_content() {
        let gate = make_secret_gate();
        let mut messages = vec![ChatMessage {
            role: ChatRole::Tool,
            principal: Principal::System,
            content: vec![MessageContent::ToolResult {
                result: ToolResult {
                    tool_call_id: "call-1".to_string(),
                    name: "execute".to_string(),
                    content: secret_string(2, "1234567890ABCDEF"),
                },
            }],
        }];
        let mut event = GuardEvent::Inbound(&mut messages);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));
        let tool_result = match &messages[0].content[0] {
            MessageContent::ToolResult { result } => &result.content,
            _ => panic!("expected tool result"),
        };
        assert_eq!(tool_result, "[REDACTED]");
    }

    #[test]
    fn text_delta_is_redacted_when_modified() {
        let gate = make_secret_gate();
        let mut delta = format!(
            "before {} after",
            secret_string(0, "proj-ABCDEFGHIJKLMNOPQRSTUVWXYZ")
        );
        let mut event = GuardEvent::TextDelta(&mut delta);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Modify
        ));
        assert_eq!(delta, "before [REDACTED] after");
    }
}
