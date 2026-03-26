#[path = "../context.rs"]
mod legacy;

pub use legacy::{
    ContextSource, History, Identity, SkillContext, SkillLoader, SubscriptionContext,
};

pub mod history;
pub mod identity_prompt;
pub mod skill_instructions;
pub mod skill_summaries;
pub mod subscriptions;

#[cfg(test)]
mod tests;
