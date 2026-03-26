use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};

use crate::context::ContextSource;
use crate::gate::{Guard, GuardContext, GuardEvent, Verdict};
use crate::llm::{ChatMessage, FunctionTool, ToolCall};
use crate::tool::Tool;

pub mod builders;
pub mod tiers;
pub mod verdicts;

pub use builders::{
    build_default_turn, build_spawned_t3_turn, build_t2_turn, build_t3_turn, build_turn_for_config,
    build_turn_for_config_with_subscriptions,
};
pub use tiers::{TurnTier, resolve_tier};
pub use verdicts::resolve_verdict;

#[cfg(test)]
mod tests;

/// Turn-level orchestration for context assembly, guard checks, and tools.
pub struct Turn {
    context: Vec<Box<dyn ContextSource>>,
    tools: Vec<Box<dyn Tool>>,
    guards: Vec<Box<dyn Guard>>,
    delegation: Option<crate::delegation::DelegationConfig>,
    tainted: AtomicBool,
}

impl Turn {
    pub fn new() -> Self {
        Self {
            context: Vec::new(),
            tools: Vec::new(),
            guards: Vec::new(),
            delegation: None,
            tainted: AtomicBool::new(false),
        }
    }

    pub fn context(mut self, source: impl ContextSource + 'static) -> Self {
        self.context.push(Box::new(source));
        self
    }

    pub fn tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.push(Box::new(tool));
        self
    }

    pub fn guard(mut self, guard: impl Guard + 'static) -> Self {
        self.guards.push(Box::new(guard));
        self
    }

    pub fn delegation(mut self, delegation: crate::delegation::DelegationConfig) -> Self {
        self.delegation = Some(delegation);
        self
    }

    pub fn tool_definitions(&self) -> Vec<FunctionTool> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    pub fn is_tainted(&self) -> bool {
        self.tainted.load(Ordering::Relaxed)
    }

    pub fn assemble_context(&self, messages: &mut Vec<ChatMessage>) {
        for source in &self.context {
            source.assemble(messages);
        }
    }

    pub fn has_guard(&self, name: &str) -> bool {
        self.guards.iter().any(|guard| guard.name() == name)
    }

    pub fn delegation_config(&self) -> Option<&crate::delegation::DelegationConfig> {
        self.delegation.as_ref()
    }

    pub fn needs_budget_context(&self) -> bool {
        self.has_guard(crate::gate::budget::BUDGET_GUARD_ID)
    }

    pub fn check_budget(&self, context: GuardContext) -> Option<Verdict> {
        let guard = self
            .guards
            .iter()
            .find(|guard| guard.name() == crate::gate::budget::BUDGET_GUARD_ID)?;
        let mut messages = vec![ChatMessage::user("budget probe")];
        let mut event = GuardEvent::Inbound(&mut messages);
        match guard.check(&mut event, &context) {
            Verdict::Allow => None,
            verdict => Some(verdict),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, messages, context), fields(message_count = messages.len()))]
    pub fn check_inbound(
        &self,
        messages: &mut Vec<ChatMessage>,
        context: Option<GuardContext>,
    ) -> Verdict {
        let baseline = messages.clone();
        self.assemble_context(messages);
        let tainted = messages
            .iter()
            .any(|message| message.principal.is_taint_source());
        self.tainted.store(tainted, Ordering::Relaxed);
        let mut context = context.unwrap_or_default();
        context.tainted = tainted;
        let verdict = resolve_verdict(&self.guards, GuardEvent::Inbound(messages), false, context);
        let modified = baseline.len() != messages.len()
            || baseline.iter().zip(messages.iter()).any(|(before, after)| {
                before.role != after.role
                    || before.principal != after.principal
                    || serde_json::to_string(&before.content).ok()
                        != serde_json::to_string(&after.content).ok()
            });
        if modified {
            match verdict {
                Verdict::Allow => Verdict::Modify,
                _ => verdict,
            }
        } else {
            verdict
        }
    }

    #[tracing::instrument(level = "debug", skip(self, call))]
    pub fn check_tool_call(&self, call: &ToolCall) -> Verdict {
        resolve_verdict(
            &self.guards,
            GuardEvent::ToolCall(call),
            false,
            GuardContext {
                tainted: self.is_tainted(),
                ..Default::default()
            },
        )
    }

    #[tracing::instrument(level = "debug", skip(self, calls), fields(call_count = calls.len()))]
    pub fn check_tool_batch(&self, calls: &[ToolCall]) -> Verdict {
        resolve_verdict(
            &self.guards,
            GuardEvent::ToolBatch(calls),
            false,
            GuardContext {
                tainted: self.is_tainted(),
                ..Default::default()
            },
        )
    }

    #[tracing::instrument(level = "debug", skip(self, text))]
    pub fn check_text_delta(&self, text: &mut String) -> Verdict {
        resolve_verdict(
            &self.guards,
            GuardEvent::TextDelta(text),
            false,
            GuardContext {
                tainted: self.is_tainted(),
                ..Default::default()
            },
        )
    }

    #[tracing::instrument(level = "debug", skip(self, arguments), fields(tool_name = %name))]
    pub async fn execute_tool(&self, name: &str, arguments: &str) -> Result<String> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == name)
            .ok_or_else(|| anyhow!("tool '{name}' not found"))?;
        tool.execute(arguments).await
    }
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}
