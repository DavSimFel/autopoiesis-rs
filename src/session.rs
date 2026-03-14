use crate::llm::ChatMessage;

#[derive(Debug, Default)]
pub struct Session {
    messages: Vec<ChatMessage>,
}

impl Session {
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![ChatMessage::system(system_prompt)],
        }
    }

    pub fn append(&mut self, message: ChatMessage) {
        self.messages.push(message);
    }

    pub fn history(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn add_user_message(&mut self, message: impl Into<String>) {
        self.messages.push(ChatMessage::user(message));
    }
}
