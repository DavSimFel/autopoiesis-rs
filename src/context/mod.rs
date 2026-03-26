use crate::llm::ChatMessage;

/// Source for messages inserted into each turn before model invocation.
pub trait ContextSource: Send + Sync {
    fn name(&self) -> &str;
    fn assemble(&self, messages: &mut Vec<ChatMessage>);
}

pub mod history;
pub mod identity_prompt;
pub mod skill_instructions;
pub mod skill_summaries;
pub mod subscriptions;

pub use history::History;
pub use identity_prompt::Identity;
pub use skill_instructions::SkillLoader;
pub use skill_summaries::SkillContext;
pub use subscriptions::SubscriptionContext;

#[cfg(test)]
mod tests;
