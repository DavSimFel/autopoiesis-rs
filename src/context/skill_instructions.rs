use crate::llm::{ChatMessage, ChatRole, MessageContent};
use crate::skills::SkillDefinition;

use super::ContextSource;

/// Full skill instructions for spawned T3 children.
pub struct SkillLoader {
    skills: Vec<SkillDefinition>,
}

impl SkillLoader {
    pub fn new(skills: Vec<SkillDefinition>) -> Self {
        Self { skills }
    }

    pub fn render_fragment(&self) -> String {
        self.skills
            .iter()
            .map(|skill| format!("Skill: {}\n{}", skill.name, skill.instructions))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

impl ContextSource for SkillLoader {
    fn name(&self) -> &str {
        "skill_loader"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        let rendered = self.render_fragment();
        if rendered.is_empty() {
            return;
        }

        if let Some(first) = messages.first_mut()
            && first.role == ChatRole::System
        {
            first.content.push(MessageContent::text(rendered));
            return;
        }

        messages.insert(0, ChatMessage::system(rendered));
    }
}
