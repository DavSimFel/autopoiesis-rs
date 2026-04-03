use tracing::debug;

use super::Session;
use crate::llm::history_groups;
use crate::llm::{ChatRole, MessageContent};

impl Session {
    pub(crate) fn estimate_message_tokens(message: &crate::llm::ChatMessage) -> u64 {
        history_groups::estimate_message_tokens(message) as u64
    }

    pub(super) fn can_trim_after_append(message: &crate::llm::ChatMessage) -> bool {
        match message.role {
            ChatRole::Assistant => !message
                .content
                .iter()
                .any(|block| matches!(block, MessageContent::ToolCall { .. })),
            ChatRole::Tool => false,
            _ => true,
        }
    }

    pub(super) fn trim_anchor_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .position(|message| message.role == ChatRole::System)
    }

    pub(super) fn trim_group_range(&self, anchor_index: Option<usize>) -> Option<(usize, usize)> {
        let start = anchor_index.map_or(0, |index| index.saturating_add(1));

        if start >= self.messages.len() {
            return None;
        }

        history_groups::history_group_range(&self.messages, start)
    }

    /// Trim oldest conversational groups when over token limit without splitting tool round-trips.
    pub(super) fn trim_context(&mut self) {
        debug!(
            total_tokens = self.total_tokens,
            estimated_tokens = self.estimate_context_tokens(),
            max_context_tokens = self.max_context_tokens,
            "trim context if needed"
        );
        while self.estimate_context_tokens() as u64 > self.max_context_tokens {
            let Some((start, end)) = self.trim_group_range(self.trim_anchor_index()) else {
                break;
            };
            debug!(start, end, "trimming session messages");
            self.messages.drain(start..end);
            self.message_tokens.drain(start..end);
            self.total_tokens = self.message_tokens.iter().sum();
        }
    }

    /// Estimate context tokens using cl100k_base tokenizer.
    pub fn estimate_context_tokens(&self) -> usize {
        history_groups::estimate_messages_tokens(&self.messages)
    }

    /// Ensure context is trimmed before sending to the LLM using the estimated live size.
    pub fn ensure_context_within_limit(&mut self) {
        if self.estimate_context_tokens() as u64 > self.max_context_tokens {
            self.trim_context();
        }
    }
}
