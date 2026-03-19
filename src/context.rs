use std::collections::HashMap;

use crate::identity;
use crate::llm::{ChatMessage, ChatRole, MessageContent};

/// Source for messages inserted into each turn before model invocation.
pub trait ContextSource: Send + Sync {
    fn name(&self) -> &str;
    fn assemble(&self, messages: &mut Vec<ChatMessage>);
}

/// Identity context loaded from markdown files.
pub struct Identity {
    identity_dir: String,
    vars: HashMap<String, String>,
    fallback_prompt: String,
    strict: bool,
}

impl Identity {
    pub fn new(identity_dir: &str, vars: HashMap<String, String>, fallback: &str) -> Self {
        Self {
            identity_dir: identity_dir.to_string(),
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
        match identity::load_system_prompt(&self.identity_dir, &self.vars) {
            Ok(prompt) => prompt,
            Err(error) if self.strict => {
                panic!(
                    "failed to load identity prompt from {}: {error}",
                    self.identity_dir
                )
            }
            Err(error) => {
                eprintln!(
                    "warning: failed to load identity prompt from {}: {error}; using fallback prompt",
                    self.identity_dir
                );
                self.fallback_prompt.clone()
            }
        }
    }
}

impl ContextSource for Identity {
    fn name(&self) -> &str {
        "identity"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        let rendered = self.load_prompt();
        let replacement = MessageContent::text(rendered.clone());

        if messages.is_empty() {
            messages.push(ChatMessage::system(rendered));
            return;
        }

        let first = &mut messages[0];
        if first.role != ChatRole::System {
            messages.insert(0, ChatMessage::system(rendered));
            return;
        }

        let needs_edit = match &first.content[..] {
            [MessageContent::Text { text }] if text == &rendered => false,
            _ => true,
        };

        if needs_edit {
            first.content.clear();
            first.content.push(replacement);
        }
    }
}

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

    fn estimate_message_tokens(message: &ChatMessage) -> usize {
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                MessageContent::ToolResult { result } => Some(result.content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            0
        } else {
            tiktoken_rs::cl100k_base_singleton()
                .encode_ordinary(&text)
                .len()
        }
    }
}

impl ContextSource for History {
    fn name(&self) -> &str {
        "history"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.history.is_empty() {
            return;
        }

        let mut current_tokens = messages
            .iter()
            .map(Self::estimate_message_tokens)
            .sum::<usize>();
        let mut selected = Vec::new();

        for message in self.history.iter().rev() {
            if message.role == ChatRole::System {
                continue;
            }

            let message_tokens = Self::estimate_message_tokens(message);
            if current_tokens + message_tokens > self.max_tokens {
                break;
            }

            selected.push(message.clone());
            current_tokens += message_tokens;
        }

        if selected.is_empty() {
            return;
        }

        selected.reverse();
        messages.extend(selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, time::{SystemTime, UNIX_EPOCH}};

    struct TempIdentityDir {
        path: std::path::PathBuf,
    }

    impl Drop for TempIdentityDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    impl TempIdentityDir {
        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    fn temp_identity_dir(prefix: &str) -> TempIdentityDir {
        let path = env::temp_dir().join(format!(
            "autopoiesis_gate_identity_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("failed to create temp identity directory");
        TempIdentityDir { path }
    }

    fn make_identity_files(dir: &TempIdentityDir, files: &[(&str, &str)]) {
        for (name, contents) in files {
            fs::write(dir.path().join(name), contents).expect("failed to write identity file");
        }
    }

    #[test]
    fn identity_replaces_system_message() {
        let dir = temp_identity_dir("replaces");
        make_identity_files(
            &dir,
            &[
                ("constitution.md", "constitution"),
                ("identity.md", "identity"),
                ("context.md", "context"),
            ],
        );

        let mut vars = HashMap::new();
        vars.insert("model".to_string(), "gpt-5.4".to_string());
        let source = Identity::new(
            dir.path().to_str().expect("temp path should be utf-8"),
            vars,
            "fallback",
        );
        let mut messages = vec![
            ChatMessage::system("old"),
            ChatMessage::user("ask"),
        ];
        source.assemble(&mut messages);

        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "constitution\n\nidentity\n\ncontext");
    }

    #[test]
    fn identity_uses_fallback_on_missing_dir() {
        let mut vars = HashMap::new();
        vars.insert("model".to_string(), "gpt-5.4".to_string());
        let source = Identity::new("/does/not/exist", vars, "fallback prompt");
        let mut messages = vec![ChatMessage::system("old prompt")];
        source.assemble(&mut messages);

        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "fallback prompt");
    }

    #[test]
    #[should_panic(expected = "failed to load identity prompt")]
    fn identity_strict_mode_panics_on_missing_dir() {
        let source = Identity::new("/does/not/exist", HashMap::new(), "fallback prompt").strict();
        let mut messages = vec![ChatMessage::system("old prompt")];
        source.assemble(&mut messages);
    }

    #[test]
    fn identity_applies_template_vars() {
        let dir = temp_identity_dir("template_vars");
        make_identity_files(
            &dir,
            &[
                ("constitution.md", "model: {{model}}"),
                ("identity.md", "cwd: {{cwd}}"),
                ("context.md", "tool: {{tool}}"),
            ],
        );

        let mut vars = HashMap::new();
        vars.insert("model".to_string(), "gpt-4".to_string());
        vars.insert("cwd".to_string(), "/tmp/proj".to_string());
        vars.insert("tool".to_string(), "execute".to_string());
        let source = Identity::new(
            dir.path().to_str().expect("temp path should be utf-8"),
            vars,
            "fallback",
        );
        let mut messages = vec![ChatMessage::system("old")];
        source.assemble(&mut messages);

        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "model: gpt-4\n\ncwd: /tmp/proj\n\ntool: execute");
    }

    #[test]
    fn history_adds_history_to_messages() {
        let mut source = History::new(1000);
        let history = vec![
            ChatMessage::user("first"),
            ChatMessage::user("middle"),
            ChatMessage::user("last"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble(&mut messages);

        assert_eq!(messages.len(), 3);
        assert_eq!(message_text(&messages[0]), "first");
        assert_eq!(message_text(&messages[1]), "middle");
        assert_eq!(message_text(&messages[2]), "last");
    }

    #[test]
    fn history_respects_token_budget() {
        let mut source = History::new(8);
        let history = vec![
            ChatMessage::user("alpha beta gamma delta epsilon"),
            ChatMessage::user("one two three four five six"),
            ChatMessage::user("the quick brown fox jumps"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble(&mut messages);

        // Tiny budget should keep only the newest context message.
        assert_eq!(messages.len(), 1);
        assert_eq!(message_text(&messages[0]), "the quick brown fox jumps");
    }

    #[test]
    fn history_skips_system_messages() {
        let mut source = History::new(1000);
        let history = vec![
            ChatMessage::system("system message should skip"),
            ChatMessage::user("first"),
            ChatMessage::system("another skip"),
            ChatMessage::user("last"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble(&mut messages);

        assert_eq!(messages.len(), 2);
        for message in &messages {
            assert_ne!(message.role, ChatRole::System);
        }
        assert_eq!(message_text(&messages[0]), "first");
        assert_eq!(message_text(&messages[1]), "last");
    }

    #[test]
    fn history_newest_first() {
        let mut source = History::new(6);
        let history = vec![
            ChatMessage::user("one two three"),
            ChatMessage::user("four five six"),
            ChatMessage::user("seven eight nine"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble(&mut messages);

        assert_eq!(messages.len(), 2);
        assert_eq!(message_text(&messages[0]), "four five six");
        assert_eq!(message_text(&messages[1]), "seven eight nine");
    }

    fn message_text(message: &ChatMessage) -> &str {
        match message.content.first() {
            Some(MessageContent::Text { text }) => text,
            _ => panic!("expected text message content"),
        }
    }

    #[test]
    fn history_handles_empty_messages() {
        let mut source = History::new(10);
        let mut messages = vec![ChatMessage::system("existing")];
        source.set_history(&[]);
        source.assemble(&mut messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(message_text(&messages[0]), "existing");
    }
}
