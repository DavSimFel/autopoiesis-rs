use crate::llm::{ChatMessage, ChatRole, MessageContent};
use crate::skills::SkillSummary;

use super::ContextSource;

/// Skill summary context for local discovery in T1/T2 turns.
pub struct SkillContext {
    summaries: Vec<SkillSummary>,
}

impl SkillContext {
    pub fn new(summaries: Vec<SkillSummary>) -> Self {
        Self { summaries }
    }
}

impl ContextSource for SkillContext {
    fn name(&self) -> &str {
        "skills"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.summaries.is_empty() {
            return;
        }

        let rendered = format!(
            "Available skills: {}",
            self.summaries
                .iter()
                .map(|skill| format!("{} ({})", skill.name, skill.description))
                .collect::<Vec<_>>()
                .join(", ")
        );

        if messages.is_empty() {
            messages.push(ChatMessage::system(rendered));
            return;
        }

        let first = &mut messages[0];
        if first.role != ChatRole::System {
            messages.insert(0, ChatMessage::system(rendered));
            return;
        }

        first.content.push(MessageContent::text(rendered));
    }
}
