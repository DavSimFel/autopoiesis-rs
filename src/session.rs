//! In-memory chat transcript used by the agent loop.

use crate::llm::ChatMessage;

/// Conversation state for one CLI session.
#[derive(Debug, Default)]
pub struct Session {
    messages: Vec<ChatMessage>,
}

impl Session {
    /// Start a session with a system prompt.
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![ChatMessage::system(system_prompt)],
        }
    }

    /// Append a new message to the transcript.
    pub fn append(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    /// Immutable access to full message history.
    pub fn history(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Add a user prompt message.
    pub fn add_user_message(&mut self, message: impl Into<String>) {
        self.messages.push(ChatMessage::user(message));
    }
}
