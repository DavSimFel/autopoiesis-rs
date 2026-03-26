use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::identity;
use crate::llm::history_groups::{
    collect_newest_group_ranges_within_budget, estimate_message_tokens,
};
use crate::llm::{ChatMessage, ChatRole, MessageContent};
use crate::skills::SkillDefinition;
use crate::skills::SkillSummary;
use crate::subscription::{SubscriptionRecord, estimate_tokens};
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

/// Session-scoped file subscriptions materialized into the model context.
pub struct SubscriptionContext {
    subscriptions: Vec<SubscriptionRecord>,
    token_budget: usize,
}

impl SubscriptionContext {
    pub fn new(subscriptions: Vec<SubscriptionRecord>, token_budget: usize) -> Self {
        Self {
            subscriptions,
            token_budget,
        }
    }

    fn effective_timestamp(record: &SubscriptionRecord) -> &str {
        record.effective_at()
    }

    fn provenance_tag(record: &SubscriptionRecord, mtime_unix: Option<u64>) -> String {
        let filter = record.filter.label();
        let mtime = mtime_unix
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "[subscription path={} filter={} mtime={}]",
            record.path.display(),
            filter,
            mtime
        )
    }

    fn char_boundary_at_or_before(text: &str, index: usize) -> usize {
        if text.is_char_boundary(index) {
            return index;
        }

        text.char_indices()
            .map(|(boundary, _)| boundary)
            .take_while(|boundary| *boundary <= index)
            .last()
            .unwrap_or(0)
    }

    fn build_body(prefix: &str, rendered: &str, truncated: bool, rendered_len: usize) -> String {
        let mut body = String::with_capacity(prefix.len() + rendered_len + 32);
        body.push_str(prefix);
        if rendered_len > 0 {
            body.push('\n');
            body.push_str(&rendered[..rendered_len]);
        }
        if truncated {
            body.push('\n');
            body.push_str("[truncated]");
        }
        body
    }

    fn fit_body(prefix: &str, rendered: &str, budget: usize) -> Option<(String, usize, bool)> {
        let full = Self::build_body(prefix, rendered, false, rendered.len());
        let full_tokens = estimate_tokens(&full);
        if full_tokens <= budget {
            return Some((full, full_tokens, false));
        }

        let truncated_marker = Self::build_body(prefix, rendered, true, 0);
        if estimate_tokens(&truncated_marker) > budget {
            return None;
        }

        let mut low = 0usize;
        let mut high = rendered.len();
        let mut best = truncated_marker;
        let mut best_len = 0usize;

        while low <= high {
            let mid = low + (high - low) / 2;
            let boundary = Self::char_boundary_at_or_before(rendered, mid);
            let candidate = Self::build_body(prefix, rendered, true, boundary);
            let candidate_tokens = estimate_tokens(&candidate);
            if candidate_tokens <= budget {
                best = candidate;
                best_len = boundary;
                low = boundary.saturating_add(1);
            } else if boundary == 0 {
                break;
            } else {
                high = boundary.saturating_sub(1);
            }
        }

        let tokens = estimate_tokens(&best);
        Some((best, tokens, best_len < rendered.len()))
    }

    fn materialize_record(
        &self,
        record: &SubscriptionRecord,
        remaining_budget: usize,
    ) -> Option<(ChatMessage, usize)> {
        let raw = match fs::read_to_string(&record.path) {
            Ok(raw) => raw,
            Err(error) => {
                warn!(
                    path = %record.path.display(),
                    error = %error,
                    "failed to read subscription file; skipping"
                );
                return None;
            }
        };

        let rendered = match record.filter.render(&raw) {
            Ok(rendered) => rendered,
            Err(error) => {
                warn!(
                    path = %record.path.display(),
                    error = %error,
                    "failed to render subscription file; skipping"
                );
                return None;
            }
        };

        let mtime_unix = fs::metadata(&record.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        let prefix = Self::provenance_tag(record, mtime_unix);
        let (body, tokens, _was_truncated) = Self::fit_body(&prefix, &rendered, remaining_budget)?;
        Some((
            ChatMessage::system_with_principal(body, Some(crate::principal::Principal::System)),
            tokens,
        ))
    }
}

impl ContextSource for SubscriptionContext {
    fn name(&self) -> &str {
        "subscriptions"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.subscriptions.is_empty() {
            return;
        }

        let mut subscriptions = self.subscriptions.clone();
        subscriptions.sort_by(|left, right| {
            Self::effective_timestamp(left)
                .cmp(Self::effective_timestamp(right))
                .then_with(|| left.id.cmp(&right.id))
        });

        let insert_at = if messages
            .first()
            .is_some_and(|message| message.role == ChatRole::System)
        {
            1
        } else {
            0
        };

        let mut remaining = self.token_budget;
        let mut offset = 0usize;
        for record in subscriptions {
            if remaining == 0 {
                break;
            }

            let Some((message, tokens)) = self.materialize_record(&record, remaining) else {
                continue;
            };
            messages.insert(insert_at + offset, message);
            offset += 1;
            remaining = remaining.saturating_sub(tokens);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Principal;
    use crate::subscription::{SubscriptionFilter, SubscriptionRecord};
    use std::{
        env, fs,
        io::Write,
        sync::{Arc, Mutex},
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
    fn identity_preserves_existing_system_message() {
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

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].principal, crate::principal::Principal::Agent);
        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "constitution\n\nidentity\n\ncontext");
        let preserved = match &messages[1].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(preserved, "old");
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

        assert_eq!(messages.len(), 2);
        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "fallback prompt");
        let preserved = match &messages[1].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(preserved, "old prompt");
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

        assert_eq!(messages.len(), 2);
        let content = match &messages[0].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(content, "model: gpt-4\n\ncwd: /tmp/proj\n\ntool: execute");
        let preserved = match &messages[1].content[0] {
            MessageContent::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(preserved, "old");
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
        let mut source = History::new(64);
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

        let assistant_index = messages
            .iter()
            .position(|message| matches!(message.role, ChatRole::Assistant))
            .expect("assistant round-trip should be retained");
        assert!(matches!(
            messages[assistant_index].role,
            ChatRole::Assistant
        ));
        assert!(matches!(
            &messages[assistant_index].content[..],
            [
                MessageContent::Text { text },
                MessageContent::ToolCall { call },
            ] if text == "alpha beta" && call.id == "call-1"
        ));
        assert!(matches!(
            messages
                .get(assistant_index + 1)
                .map(|message| &message.role),
            Some(ChatRole::Tool)
        ));
        assert!(matches!(
            &messages[assistant_index + 1].content[..],
            [MessageContent::ToolResult { result }] if result.tool_call_id == "call-1"
        ));
        assert!(matches!(
            messages.last().map(|message| &message.role),
            Some(ChatRole::User)
        ));
        assert!(matches!(
            &messages
                .last()
                .expect("newest user message should remain")
                .content[..],
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

    fn temp_file_dir(prefix: &str) -> std::path::PathBuf {
        let path = env::temp_dir().join(format!(
            "autopoiesis_context_subscription_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_temp_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    fn subscription_record(
        id: i64,
        session_id: Option<&str>,
        path: std::path::PathBuf,
        filter: SubscriptionFilter,
        activated_at: &str,
        updated_at: &str,
    ) -> SubscriptionRecord {
        SubscriptionRecord {
            id,
            session_id: session_id.map(ToString::to_string),
            topic: "_default".to_string(),
            path,
            filter,
            activated_at: activated_at.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    fn capture_warnings<F>(f: F) -> String
    where
        F: FnOnce(),
    {
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let output = output.clone();
                move || SharedWriter(output.clone())
            })
            .with_max_level(tracing::Level::WARN)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        f();

        String::from_utf8(output.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn subscription_context_with_no_subscriptions_adds_no_messages() {
        let mut messages = vec![ChatMessage::system("base")];
        SubscriptionContext::new(Vec::new(), 100).assemble(&mut messages);

        assert_eq!(messages.len(), 1);
        assert_eq!(message_text(&messages[0]), "base");
    }

    #[test]
    fn subscription_context_materializes_file_content() {
        let dir = temp_file_dir("materializes");
        let path = write_temp_file(&dir, "notes.txt", "line one\nline two");
        let record = subscription_record(
            1,
            Some("session-a"),
            path.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        );
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(vec![record], 200).assemble(&mut messages);

        assert_eq!(messages.len(), 2);
        let rendered = message_text(&messages[1]);
        assert!(rendered.contains("line one"));
        assert!(rendered.contains("line two"));
        assert!(rendered.contains(&path.display().to_string()));
        assert!(rendered.contains("filter=full"));
    }

    #[test]
    fn subscription_context_applies_lines_regex_head_and_tail_filters() {
        let dir = temp_file_dir("filters");
        let path = write_temp_file(&dir, "data.txt", "one\ntwo\nthree\nfour\nfive");
        let base = vec![ChatMessage::system("identity")];

        let cases = vec![
            (
                SubscriptionFilter::Lines { start: 2, end: 4 },
                "two\nthree\nfour",
            ),
            (
                SubscriptionFilter::Regex {
                    pattern: "^t".to_string(),
                },
                "two\nthree",
            ),
            (SubscriptionFilter::Head { count: 2 }, "one\ntwo"),
            (SubscriptionFilter::Tail { count: 2 }, "four\nfive"),
        ];

        for (index, (filter, expected)) in cases.into_iter().enumerate() {
            let record = subscription_record(
                index as i64 + 1,
                Some("session-a"),
                path.clone(),
                filter,
                "2026-03-25T00:00:00Z",
                "2026-03-25T00:00:01Z",
            );
            let mut messages = base.clone();
            SubscriptionContext::new(vec![record], 200).assemble(&mut messages);
            let rendered = message_text(&messages[1]);
            assert!(
                rendered.contains(expected),
                "case {index} rendered {rendered:?}"
            );
        }
    }

    #[test]
    fn subscription_context_respects_token_budget_and_truncates_last_message() {
        let dir = temp_file_dir("budget");
        let path = write_temp_file(
            &dir,
            "big.txt",
            &"alpha beta gamma delta epsilon ".repeat(200),
        );
        let record = subscription_record(
            1,
            Some("session-a"),
            path,
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        );
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(vec![record], 40).assemble(&mut messages);

        assert_eq!(messages.len(), 2);
        let rendered = message_text(&messages[1]);
        assert!(rendered.contains("[truncated]"));
        assert!(estimate_tokens(rendered) <= 40);
    }

    #[test]
    fn subscription_context_missing_file_warns_and_skips() {
        let dir = temp_file_dir("missing");
        let missing = dir.join("missing.txt");
        let record = subscription_record(
            1,
            Some("session-a"),
            missing.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        );
        let warnings = capture_warnings(|| {
            let mut messages = vec![ChatMessage::system("identity")];
            SubscriptionContext::new(vec![record], 100).assemble(&mut messages);
            assert_eq!(messages.len(), 1);
        });

        assert!(warnings.contains("failed to read subscription file"));
        assert!(warnings.contains(&missing.display().to_string()));
    }

    #[test]
    fn subscription_context_skips_bad_subscription_and_keeps_later_valid_ones() {
        let dir = temp_file_dir("mixed");
        let missing = dir.join("missing.txt");
        let valid = write_temp_file(&dir, "valid.txt", "kept");
        let records = vec![
            subscription_record(
                1,
                Some("session-a"),
                missing.clone(),
                SubscriptionFilter::Full,
                "2026-03-25T00:00:00Z",
                "2026-03-25T00:00:01Z",
            ),
            subscription_record(
                2,
                Some("session-a"),
                valid.clone(),
                SubscriptionFilter::Full,
                "2026-03-25T00:00:02Z",
                "2026-03-25T00:00:03Z",
            ),
        ];

        let warnings = capture_warnings(|| {
            let mut messages = vec![ChatMessage::system("identity")];
            SubscriptionContext::new(records, 100).assemble(&mut messages);
            assert_eq!(messages.len(), 2);
            assert!(message_text(&messages[1]).contains("kept"));
        });

        assert!(warnings.contains("failed to read subscription file"));
        assert!(warnings.contains(&missing.display().to_string()));
    }

    #[test]
    fn subscription_context_includes_provenance_tags() {
        let dir = temp_file_dir("provenance");
        let path = write_temp_file(&dir, "prov.txt", "hello provenance");
        let record = subscription_record(
            1,
            Some("session-a"),
            path.clone(),
            SubscriptionFilter::Tail { count: 1 },
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        );
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(vec![record], 200).assemble(&mut messages);

        let rendered = message_text(&messages[1]);
        assert!(rendered.contains(&path.display().to_string()));
        assert!(rendered.contains("filter=tail"));
        assert!(rendered.contains("mtime="));
    }

    #[test]
    fn subscription_context_orders_messages_by_effective_timestamp() {
        let dir = temp_file_dir("ordering");
        let first = write_temp_file(&dir, "first.txt", "first");
        let second = write_temp_file(&dir, "second.txt", "second");
        let records = vec![
            subscription_record(
                2,
                Some("session-a"),
                second.clone(),
                SubscriptionFilter::Full,
                "2026-03-25T00:00:00Z",
                "2026-03-25T00:00:10Z",
            ),
            subscription_record(
                1,
                Some("session-a"),
                first.clone(),
                SubscriptionFilter::Full,
                "2026-03-25T00:00:00Z",
                "2026-03-25T00:00:01Z",
            ),
        ];
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(records, 200).assemble(&mut messages);

        assert_eq!(message_text(&messages[1]).contains("first"), true);
        assert_eq!(message_text(&messages[2]).contains("second"), true);
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
