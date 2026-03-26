use crate::llm::ChatMessage;
use crate::llm::history_groups::{
    collect_newest_group_ranges_within_budget, estimate_message_tokens,
};

use super::ContextSource;

/// Session history replay into the model context with token budget.
pub struct History {
    max_tokens: usize,
    history: Vec<ChatMessage>,
}

impl History {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            history: Vec::new(),
        }
    }

    /// Update the history used to assemble context.
    pub fn set_history(&mut self, history: &[ChatMessage]) {
        self.history = history.to_vec();
    }

    /// Invariant: assistant/tool round-trips are the replay unit.
    fn assemble_pair_aware(&self, messages: &mut Vec<ChatMessage>) {
        if self.history.is_empty() {
            return;
        }

        let current_tokens = messages.iter().map(estimate_message_tokens).sum::<usize>();
        let selected = collect_newest_group_ranges_within_budget(
            &self.history,
            self.max_tokens.saturating_sub(current_tokens),
            |start, end| {
                self.history[start..end]
                    .iter()
                    .map(estimate_message_tokens)
                    .sum::<usize>()
            },
        );

        if selected.is_empty() {
            return;
        }

        for (start, end) in selected {
            messages.extend(self.history[start..end].iter().cloned());
        }
    }
}

impl ContextSource for History {
    fn name(&self) -> &str {
        "history"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        self.assemble_pair_aware(messages);
    }
}
