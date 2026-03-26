use std::collections::HashMap;
use std::path::PathBuf;

use crate::identity;
use crate::llm::{ChatMessage, ChatRole, MessageContent};
use tracing::warn;

use super::ContextSource;

/// Identity context loaded from markdown files.
pub struct Identity {
    identity_files: Vec<PathBuf>,
    vars: HashMap<String, String>,
    fallback_prompt: String,
    strict: bool,
}

impl Identity {
    pub fn new(
        identity_files: Vec<PathBuf>,
        vars: HashMap<String, String>,
        fallback: &str,
    ) -> Self {
        Self {
            identity_files,
            vars,
            fallback_prompt: fallback.to_string(),
            strict: false,
        }
    }

    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    fn load_prompt(&self) -> String {
        match identity::load_system_prompt_from_files(&self.identity_files, &self.vars) {
            Ok(prompt) => prompt,
            Err(error) if self.strict => {
                panic!(
                    "failed to load identity prompt from {:?}: {error}",
                    self.identity_files
                )
            }
            Err(error) => {
                warn!(
                    "warning: failed to load identity prompt from {:?}: {error}; using fallback prompt",
                    self.identity_files
                );
                self.fallback_prompt.clone()
            }
        }
    }
}

pub(crate) fn inject_identity_prompt(messages: &mut Vec<ChatMessage>, rendered: String) {
    let replacement = MessageContent::text(rendered.clone());

    if let Some(first) = messages.first_mut()
        && first.role == ChatRole::System
        && first.principal == crate::principal::Principal::Agent
    {
        if !matches!(&first.content[..], [MessageContent::Text { text }] if text == &rendered) {
            first.content.clear();
            first.content.push(replacement);
        }
        return;
    }

    messages.insert(
        0,
        ChatMessage::system_with_principal(rendered, Some(crate::principal::Principal::Agent)),
    );
}

impl ContextSource for Identity {
    fn name(&self) -> &str {
        "identity"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        inject_identity_prompt(messages, self.load_prompt());
    }
}
