use crate::llm::{ChatMessage, ChatRole, MessageContent};
use crate::principal::Principal;
use crate::session_registry::{SessionRegistry, SessionSpec};

use super::ContextSource;

#[derive(Clone, Debug, Default)]
pub struct SessionManifest {
    entries: Vec<SessionManifestEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionManifestEntry {
    pub session_id: String,
    pub tier: String,
    pub description: String,
    pub always_on: bool,
}

impl SessionManifestEntry {
    fn from_spec(spec: &SessionSpec) -> Self {
        Self {
            session_id: spec.session_id.clone(),
            tier: spec.tier.clone(),
            description: spec.description.clone(),
            always_on: spec.always_on,
        }
    }
}

impl SessionManifest {
    pub fn from_registry(registry: &SessionRegistry) -> Self {
        Self {
            entries: registry
                .sessions()
                .into_iter()
                .map(SessionManifestEntry::from_spec)
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn render(&self) -> String {
        let mut rendered = String::from("## Available Sessions");
        for entry in &self.entries {
            let state = if entry.always_on {
                "always-on"
            } else {
                "request-owned"
            };
            rendered.push('\n');
            rendered.push_str(&format!(
                "- {} (tier {}, {}): {}",
                entry.session_id, entry.tier, state, entry.description
            ));
        }
        rendered
    }

    fn inject(messages: &mut Vec<ChatMessage>, rendered: String) {
        if let Some(first) = messages.first_mut()
            && first.role == ChatRole::System
            && first.principal == Principal::Agent
        {
            first.content.push(MessageContent::text(rendered));
            return;
        }

        messages.insert(
            0,
            ChatMessage::system_with_principal(rendered, Some(Principal::Agent)),
        );
    }
}

impl ContextSource for SessionManifest {
    fn name(&self) -> &str {
        "session_manifest"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.is_empty() {
            return;
        }

        Self::inject(messages, self.render());
    }
}
