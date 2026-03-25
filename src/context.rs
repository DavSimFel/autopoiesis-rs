use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::path::PathBuf;

use crate::identity;
use crate::llm::{ChatMessage, ChatRole, MessageContent};
use crate::skills::SkillDefinition;
use crate::skills::SkillSummary;
use tracing::warn;

/// Source for messages inserted into each turn before model invocation.
pub trait ContextSource: Send + Sync {
    fn name(&self) -> &str;
    fn assemble(&self, messages: &mut Vec<ChatMessage>);
}

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

        let needs_edit =
            !matches!(&first.content[..], [MessageContent::Text { text }] if text == &rendered);

        if needs_edit {
            first.content.clear();
            first.content.push(replacement);
        }
    }
}

/// Skill summary context for local discovery in T1/T2 turns.
pub struct SkillContext {
    summaries: Vec<SkillSummary>,
}

impl SkillContext {
    pub fn new(summaries: Vec<SkillSummary>) -> Self {
        Self { summaries }
    }
}

/// Full skill instructions for spawned T3 children.
pub struct SkillLoader {
    skills: Vec<SkillDefinition>,
}

impl SkillLoader {
    pub fn new(skills: Vec<SkillDefinition>) -> Self {
        Self { skills }
    }

    pub fn render_fragment(&self) -> String {
        self.skills
            .iter()
            .map(|skill| format!("Skill: {}\n{}", skill.name, skill.instructions))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

impl ContextSource for SkillLoader {
    fn name(&self) -> &str {
        "skill_loader"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        let rendered = self.render_fragment();
        if rendered.is_empty() {
            return;
        }

        if let Some(first) = messages.first_mut()
            && first.role == ChatRole::System
        {
            first.content.push(MessageContent::text(rendered));
            return;
        }

        messages.insert(0, ChatMessage::system(rendered));
    }
}

impl ContextSource for SkillContext {
    fn name(&self) -> &str {
        "skills"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.summaries.is_empty() {
            return;
        }

        let rendered = format!(
            "Available skills: {}",
            self.summaries
                .iter()
                .map(|skill| format!("{} ({})", skill.name, skill.description))
                .collect::<Vec<_>>()
                .join(", ")
        );

        if messages.is_empty() {
            messages.push(ChatMessage::system(rendered));
            return;
        }

        let first = &mut messages[0];
        if first.role != ChatRole::System {
            messages.insert(0, ChatMessage::system(rendered));
            return;
        }

        first.content.push(MessageContent::text(rendered));
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

    #[cfg(test)]
    fn tool_call_ids(message: &ChatMessage) -> HashSet<&str> {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::ToolCall { call } => Some(call.id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[cfg(test)]
    fn tool_result_call_id(message: &ChatMessage) -> Option<&str> {
        message.content.iter().find_map(|block| match block {
            MessageContent::ToolResult { result } => Some(result.tool_call_id.as_str()),
            _ => None,
        })
    }

    #[cfg(test)]
    fn history_group_range(history: &[ChatMessage], index: usize) -> Option<(usize, usize)> {
        match history.get(index)?.role {
            ChatRole::System => None,
            ChatRole::User => Some((index, index + 1)),
            ChatRole::Assistant => {
                let call_ids = Self::tool_call_ids(&history[index]);
                let mut end = index + 1;

                if !call_ids.is_empty() {
                    while end < history.len() {
                        match &history[end] {
                            ChatMessage {
                                role: ChatRole::Tool,
                                content,
                                ..
                            } => {
                                let matches_call = content.iter().any(|block| match block {
                                    MessageContent::ToolResult { result } => {
                                        call_ids.contains(result.tool_call_id.as_str())
                                    }
                                    _ => false,
                                });
                                if matches_call {
                                    end += 1;
                                } else {
                                    break;
                                }
                            }
                            _ => break,
                        }
                    }
                }

                Some((index, end))
            }
            ChatRole::Tool => {
                let call_id = Self::tool_result_call_id(&history[index])?;
                let mut start = index;

                for candidate in (0..index).rev() {
                    if history[candidate].role != ChatRole::Assistant {
                        continue;
                    }

                    let call_ids = Self::tool_call_ids(&history[candidate]);
                    if call_ids.contains(call_id) {
                        start = candidate;
                        break;
                    }
                }

                let call_ids = Self::tool_call_ids(&history[start]);
                let mut end = start + 1;

                while end < history.len() {
                    match &history[end] {
                        ChatMessage {
                            role: ChatRole::Tool,
                            content,
                            ..
                        } => {
                            let matches_call = content.iter().any(|block| match block {
                                MessageContent::ToolResult { result } => {
                                    call_ids.contains(result.tool_call_id.as_str())
                                }
                                _ => false,
                            });
                            if matches_call {
                                end += 1;
                            } else {
                                break;
                            }
                        }
                        _ => break,
                    }
                }

                Some((start, end))
            }
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

impl History {
    #[cfg(test)]
    fn assemble_pair_aware(&self, messages: &mut Vec<ChatMessage>) {
        if self.history.is_empty() {
            return;
        }

        let mut current_tokens = messages
            .iter()
            .map(Self::estimate_message_tokens)
            .sum::<usize>();
        let mut selected = Vec::new();

        let mut index = self.history.len();
        while index > 0 {
            index -= 1;

            let Some((start, end)) = Self::history_group_range(&self.history, index) else {
                continue;
            };

            let group_tokens = self.history[start..end]
                .iter()
                .map(Self::estimate_message_tokens)
                .sum::<usize>();

            if current_tokens + group_tokens > self.max_tokens {
                break;
            }

            selected.splice(0..0, self.history[start..end].iter().cloned());
            current_tokens += group_tokens;
            index = start;
        }

        if selected.is_empty() {
            return;
        }

        messages.extend(selected);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Principal;
    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

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
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("failed to create identity file parent");
            }
            fs::write(path, contents).expect("failed to write identity file");
        }
    }

    #[test]
    fn identity_replaces_system_message() {
        let dir = temp_identity_dir("replaces");
        make_identity_files(
            &dir,
            &[
                ("constitution.md", "constitution"),
                ("agents/silas/agent.md", "identity"),
                ("context.md", "context"),
            ],
        );

        let mut vars = HashMap::new();
        vars.insert("model".to_string(), "gpt-5.4".to_string());
        let source = Identity::new(
            identity::t1_identity_files(dir.path(), "silas"),
            vars,
            "fallback",
        );
        let mut messages = vec![ChatMessage::system("old"), ChatMessage::user("ask")];
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
        let source = Identity::new(
            identity::t1_identity_files("/does/not/exist", "silas"),
            vars,
            "fallback prompt",
        );
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
        let source = Identity::new(
            identity::t1_identity_files("/does/not/exist", "silas"),
            HashMap::new(),
            "fallback prompt",
        )
        .strict();
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
                ("agents/silas/agent.md", "cwd: {{cwd}}"),
                ("context.md", "tool: {{tool}}"),
            ],
        );

        let mut vars = HashMap::new();
        vars.insert("model".to_string(), "gpt-4".to_string());
        vars.insert("cwd".to_string(), "/tmp/proj".to_string());
        vars.insert("tool".to_string(), "execute".to_string());
        let source = Identity::new(
            identity::t1_identity_files(dir.path(), "silas"),
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
    fn skill_context_appends_summary_to_existing_system_message() {
        let source = SkillContext::new(vec![SkillSummary {
            name: "code-review".to_string(),
            description: "Reviews code changes".to_string(),
        }]);
        let mut messages = vec![ChatMessage::system("base prompt")];

        source.assemble(&mut messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::System);
        assert!(
            messages[0].content.iter().any(
                |block| matches!(block, MessageContent::Text { text } if text == "base prompt")
            )
        );
        assert!(messages[0].content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == "Available skills: code-review (Reviews code changes)")));
    }

    #[test]
    fn skill_loader_renders_single_skill_fragment() {
        let source = SkillLoader::new(vec![SkillDefinition {
            name: "code-review".to_string(),
            description: "Reviews code changes".to_string(),
            instructions: "Review carefully.".to_string(),
            required_caps: vec!["code".to_string()],
            token_estimate: 100,
        }]);

        assert_eq!(
            source.render_fragment(),
            "Skill: code-review\nReview carefully."
        );

        let mut messages = Vec::new();
        source.assemble(&mut messages);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, ChatRole::System);
        assert!(matches!(
            &messages[0].content[..],
            [MessageContent::Text { text }] if text == "Skill: code-review\nReview carefully."
        ));
    }

    #[test]
    fn skill_loader_renders_multiple_skills_in_request_order() {
        let source = SkillLoader::new(vec![
            SkillDefinition {
                name: "planning".to_string(),
                description: "Plans work".to_string(),
                instructions: "Plan carefully.".to_string(),
                required_caps: vec!["reasoning".to_string()],
                token_estimate: 10,
            },
            SkillDefinition {
                name: "code-review".to_string(),
                description: "Reviews code changes".to_string(),
                instructions: "Review carefully.".to_string(),
                required_caps: vec!["code".to_string()],
                token_estimate: 20,
            },
        ]);

        assert_eq!(
            source.render_fragment(),
            "Skill: planning\nPlan carefully.\n\nSkill: code-review\nReview carefully."
        );
    }

    #[test]
    fn skill_loader_merges_into_existing_system_message() {
        let source = SkillLoader::new(vec![SkillDefinition {
            name: "code-review".to_string(),
            description: "Reviews code changes".to_string(),
            instructions: "Review carefully.".to_string(),
            required_caps: vec!["code".to_string()],
            token_estimate: 100,
        }]);

        let mut messages = vec![ChatMessage::system("base".to_string())];
        source.assemble(&mut messages);
        assert_eq!(messages.len(), 1);
        let text = messages[0]
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, "base\nSkill: code-review\nReview carefully.");
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
    fn history_keeps_assistant_tool_roundtrip_when_budget_allows_group() {
        let mut source = History::new(6);
        let mut assistant =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
        assistant.content.push(MessageContent::Text {
            text: "alpha beta".to_string(),
        });
        assistant.content.push(MessageContent::ToolCall {
            call: crate::llm::ToolCall {
                id: "call-1".to_string(),
                name: "execute".to_string(),
                arguments: "{\"command\":\"echo hi\"}".to_string(),
            },
        });

        let history = vec![
            ChatMessage::user("very long old message that should be trimmed"),
            assistant,
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            ChatMessage::user("new"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble_pair_aware(&mut messages);

        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0].role, ChatRole::Assistant));
        assert!(matches!(
            &messages[0].content[..],
            [
                MessageContent::Text { text },
                MessageContent::ToolCall { call },
            ] if text == "alpha beta" && call.id == "call-1"
        ));
        assert!(matches!(messages[1].role, ChatRole::Tool));
        assert!(matches!(
            &messages[1].content[..],
            [MessageContent::ToolResult { result }] if result.tool_call_id == "call-1"
        ));
        assert!(matches!(messages[2].role, ChatRole::User));
        assert!(matches!(
            &messages[2].content[..],
            [MessageContent::Text { text }] if text == "new"
        ));
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

    #[test]
    fn history_keeps_assistant_tool_roundtrip_intact() {
        let mut source = History::new(2);
        let mut assistant =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
        assistant.content.push(MessageContent::Text {
            text: "alpha beta".to_string(),
        });
        assistant.content.push(MessageContent::ToolCall {
            call: crate::llm::ToolCall {
                id: "call-1".to_string(),
                name: "execute".to_string(),
                arguments: "{\"command\":\"echo hi\"}".to_string(),
            },
        });
        let assistant_tool_call_count = assistant
            .content
            .iter()
            .filter(|block| matches!(**block, MessageContent::ToolCall { .. }))
            .count();

        let history = vec![
            ChatMessage::user("old"),
            assistant,
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            ChatMessage::user("new"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble_pair_aware(&mut messages);

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].role, ChatRole::User));
        assert!(matches!(
            &messages[0].content[..],
            [MessageContent::Text { text }] if text == "new"
        ));
        assert!(
            messages
                .iter()
                .all(|message| !matches!(message.role, ChatRole::Assistant | ChatRole::Tool))
        );
        assert_eq!(assistant_tool_call_count, 1);
    }

    #[test]
    fn history_keeps_multi_call_tool_roundtrip_intact() {
        let mut source = History::new(3);
        let mut assistant =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
        assistant.content.push(MessageContent::Text {
            text: "alpha beta".to_string(),
        });
        assistant.content.push(MessageContent::ToolCall {
            call: crate::llm::ToolCall {
                id: "call-1".to_string(),
                name: "first".to_string(),
                arguments: "{}".to_string(),
            },
        });
        assistant.content.push(MessageContent::ToolCall {
            call: crate::llm::ToolCall {
                id: "call-2".to_string(),
                name: "second".to_string(),
                arguments: "{}".to_string(),
            },
        });
        let assistant_tool_call_count = assistant
            .content
            .iter()
            .filter(|block| matches!(**block, MessageContent::ToolCall { .. }))
            .count();

        let history = vec![
            ChatMessage::user("old"),
            assistant,
            ChatMessage::tool_result_with_principal(
                "call-1",
                "first",
                "ok-1",
                Some(Principal::System),
            ),
            ChatMessage::tool_result_with_principal(
                "call-2",
                "second",
                "ok-2",
                Some(Principal::System),
            ),
            ChatMessage::user("new"),
        ];
        source.set_history(&history);

        let mut messages = Vec::new();
        source.assemble_pair_aware(&mut messages);

        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0].role, ChatRole::User));
        assert!(matches!(
            &messages[0].content[..],
            [MessageContent::Text { text }] if text == "new"
        ));
        assert_eq!(assistant_tool_call_count, 2);
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
