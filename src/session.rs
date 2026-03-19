//! Persistent chat sessions stored as daily JSONL files.
//!
//! Each day gets one file: `sessions/2026-03-14.jsonl`.
//! Messages are appended in real time. On load, the file is
//! replayed to rebuild in-memory state.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiktoken_rs::cl100k_base_singleton;

use crate::llm::{ChatMessage, ChatRole, MessageContent, TurnMeta};
use crate::util::utc_timestamp;

/// One line in the JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Role: system, user, assistant, tool.
    pub role: String,
    /// Message content.
    pub content: String,
    /// ISO 8601 UTC timestamp.
    pub ts: String,
    /// Provider metadata (only on assistant messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<TurnMeta>,
    /// Tool call ID (only on tool messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Tool name (only on tool messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool calls made by the assistant (only on assistant messages with tool use).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<crate::llm::ToolCall>>,
}

/// Conversation state for one CLI session.
pub struct Session {
    messages: Vec<ChatMessage>,
    /// Maximum context tokens before trimming old messages.
    max_context_tokens: u64,
    /// Running token count from provider metadata.
    total_tokens: u64,
    /// Path to the sessions directory.
    sessions_dir: PathBuf,
    /// Token totals stored per message, aligned to `messages`.
    message_tokens: Vec<u64>,
}

impl Session {
    /// Start a session. Messages are loaded from persistent storage during `load_today`.
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Result<Self> {
        let session = Self {
            messages: Vec::new(),
            max_context_tokens: 100_000,
            total_tokens: 0,
            sessions_dir: sessions_dir.into(),
            message_tokens: Vec::new(),
        };

        Ok(session)
    }

    fn to_entry(message: &ChatMessage, meta: Option<&TurnMeta>) -> SessionEntry {
        let role = match message.role {
            ChatRole::System => "system",
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        };

        let content = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                MessageContent::ToolResult { result } => Some(result.content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let (call_id, tool_name) = match message.role {
            ChatRole::Tool => message
                .content
                .iter()
                .find_map(|block| match block {
                    MessageContent::ToolResult { result } => {
                        Some((Some(result.tool_call_id.clone()), Some(result.name.clone())))
                    }
                    _ => None,
                })
                .unwrap_or((None, None)),
            _ => (None, None),
        };

        let tool_calls: Vec<crate::llm::ToolCall> = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::ToolCall { call } => Some(call.clone()),
                _ => None,
            })
            .collect();

        SessionEntry {
            role: role.to_string(),
            content,
            ts: utc_timestamp(),
            meta: meta.cloned(),
            call_id,
            tool_name,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }
    }

    fn token_total(meta: Option<&TurnMeta>) -> u64 {
        meta.map_or(0, |meta| {
            meta.input_tokens.unwrap_or(0) + meta.output_tokens.unwrap_or(0)
        })
    }

    fn append_entry_to_file(path: &Path, entry: &SessionEntry) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;

        let line = serde_json::to_string(entry).context("failed to serialize session entry")?;
        writeln!(file, "{line}").context("failed to write session entry")?;
        Ok(())
    }

    fn message_from_entry(entry: SessionEntry) -> (Option<ChatMessage>, u64) {
        let token_delta = Self::token_total(entry.meta.as_ref());

        let message = match entry.role.as_str() {
            "system" => Some(ChatMessage::system(entry.content)),
            "user" => Some(ChatMessage::user(entry.content)),
            "assistant" => {
                let mut content = Vec::new();
                if !entry.content.is_empty() {
                    content.push(MessageContent::text(entry.content));
                }
                if let Some(calls) = entry.tool_calls {
                    for call in calls {
                        content.push(MessageContent::ToolCall { call });
                    }
                }
                Some(ChatMessage {
                    role: ChatRole::Assistant,
                    content,
                })
            }
            "tool" => {
                if entry.call_id.is_none() && entry.tool_name.is_none() && entry.content.is_empty()
                {
                    None
                } else {
                    Some(ChatMessage::tool_result(
                        entry.call_id.unwrap_or_default(),
                        entry.tool_name.unwrap_or_default(),
                        entry.content,
                    ))
                }
            }
            _ => None,
        };

        (message, token_delta)
    }

    fn session_paths(&self) -> Result<Vec<PathBuf>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut paths = fs::read_dir(&self.sessions_dir)
            .with_context(|| format!("failed to read {}", self.sessions_dir.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    fn message_text_for_estimation(message: &ChatMessage) -> String {
        message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                MessageContent::ToolResult { result } => Some(result.content.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn estimate_message_tokens(message: &ChatMessage) -> u64 {
        let text = Self::message_text_for_estimation(message);
        if text.is_empty() {
            0
        } else {
            cl100k_base_singleton().encode_ordinary(&text).len() as u64
        }
    }

    fn can_trim_after_append(message: &ChatMessage) -> bool {
        match message.role {
            ChatRole::Assistant => Self::tool_call_ids(message).is_empty(),
            ChatRole::Tool => false,
            _ => true,
        }
    }

    /// Append a message and persist it to today's JSONL file.
    pub fn append(&mut self, message: ChatMessage, meta: Option<TurnMeta>) -> Result<()> {
        let token_delta = Self::token_total(meta.as_ref());
        let entry = Self::to_entry(&message, meta.as_ref());
        let should_trim = Self::can_trim_after_append(&message);

        self.messages.push(message);
        self.message_tokens.push(token_delta);
        self.total_tokens += token_delta;

        Self::append_entry_to_file(&self.today_path(), &entry)?;

        if should_trim && self.total_tokens > self.max_context_tokens {
            self.trim_context();
        }

        Ok(())
    }

    /// Add a user prompt message with timestamp.
    pub fn add_user_message(&mut self, message: impl Into<String>) -> Result<()> {
        self.append(ChatMessage::user(message), None)
    }

    /// Immutable access to full message history.
    pub fn history(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Get the root sessions directory for this session.
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Load persisted session history from disk, replaying all dated JSONL files in order.
    pub fn load_today(&mut self) -> Result<()> {
        self.messages.clear();
        self.message_tokens.clear();
        self.total_tokens = 0;

        for path in self.session_paths()? {
            let file = File::open(&path)
                .with_context(|| format!("failed to open sessions file {}", path.display()))?;
            let reader = BufReader::new(file);

            for raw_line in reader.lines() {
                let raw_line = raw_line?;
                if raw_line.trim().is_empty() {
                    continue;
                }

                let entry: SessionEntry = serde_json::from_str(&raw_line).with_context(|| {
                    format!("failed to parse session entry in {}", path.display())
                })?;

                let (message, token_delta) = Self::message_from_entry(entry);
                if let Some(message) = message {
                    self.messages.push(message);
                    self.message_tokens.push(token_delta);
                    self.total_tokens += token_delta;
                }
            }
        }

        self.trim_context();

        Ok(())
    }

    /// Get the path for today's JSONL file.
    pub fn today_path(&self) -> PathBuf {
        self.sessions_dir
            .join(format!("{}.jsonl", &utc_timestamp()[..10]))
    }

    fn trim_anchor_index(&self) -> Option<usize> {
        self.messages
            .iter()
            .position(|message| message.role == ChatRole::System)
    }

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

    fn trim_group_range(&self, anchor_index: Option<usize>) -> Option<(usize, usize)> {
        let mut start = 0;
        while start < self.messages.len() {
            if Some(start) == anchor_index {
                start += 1;
                continue;
            }
            break;
        }

        if start >= self.messages.len() {
            return None;
        }

        let mut end = start + 1;
        match self.messages[start].role {
            ChatRole::Assistant => {
                let call_ids = Self::tool_call_ids(&self.messages[start]);
                if !call_ids.is_empty() {
                    while end < self.messages.len() {
                        match &self.messages[end] {
                            ChatMessage {
                                role: ChatRole::Tool,
                                content,
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
            }
            ChatRole::Tool => {
                while end < self.messages.len() && self.messages[end].role == ChatRole::Tool {
                    end += 1;
                }
            }
            _ => {}
        }

        Some((start, end))
    }

    fn current_context_tokens(&self, use_estimation: bool) -> u64 {
        if use_estimation {
            self.estimate_context_tokens() as u64
        } else {
            self.total_tokens
        }
    }

    /// Trim oldest conversational groups when over token limit without splitting tool round-trips.
    fn trim_context(&mut self) {
        let use_estimation = self.total_tokens == 0;
        while self.current_context_tokens(use_estimation) > self.max_context_tokens {
            let Some((start, end)) = self.trim_group_range(self.trim_anchor_index()) else {
                break;
            };
            self.messages.drain(start..end);
            self.message_tokens.drain(start..end);
            self.total_tokens = self.message_tokens.iter().sum();
        }
    }

    /// Estimate context tokens using cl100k_base tokenizer.
    pub fn estimate_context_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(Self::estimate_message_tokens)
            .sum::<u64>() as usize
    }

    /// Ensure context is trimmed before sending to the LLM when metadata is missing.
    pub fn ensure_context_within_limit(&mut self) {
        if self.estimate_context_tokens() as u64 > self.max_context_tokens {
            self.trim_context();
        }
    }

    /// Get total token count from provider metadata.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Update max context tokens for a session.
    pub fn set_max_context_tokens(&mut self, max_context_tokens: u64) {
        self.max_context_tokens = max_context_tokens;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_sessions_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_session_test_{prefix}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_entries(path: &Path, entries: &[SessionEntry]) {
        let mut file = File::create(path).unwrap();
        for entry in entries {
            writeln!(file, "{}", serde_json::to_string(entry).unwrap()).unwrap();
        }
    }

    // --- Persistence ---

    #[test]
    fn append_user_message_writes_jsonl_line() {
        let dir = temp_sessions_dir("user_msg");
        let mut session = Session::new(&dir).unwrap();

        session.add_user_message("hello").unwrap();

        let path = session.today_path();
        assert!(path.exists(), "JSONL file should be created");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // only user message
        assert_eq!(lines.len(), 1);

        let entry: SessionEntry = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry.role, "user");
        assert!(entry.content.contains("hello"));
        assert!(!entry.ts.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_assistant_message_includes_meta() {
        let dir = temp_sessions_dir("asst_meta");
        let mut session = Session::new(&dir).unwrap();

        let meta = TurnMeta {
            model: Some("gpt-5.3".to_string()),
            input_tokens: Some(50),
            output_tokens: Some(10),
            reasoning_tokens: Some(100),
            reasoning_trace: Some("I thought about it".to_string()),
        };

        session.append(ChatMessage::user("hi"), None).unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(meta),
            )
            .unwrap();

        let content = fs::read_to_string(session.today_path()).unwrap();
        let last_line = content.lines().last().unwrap();
        let entry: SessionEntry = serde_json::from_str(last_line).unwrap();

        assert_eq!(entry.role, "assistant");
        let m = entry.meta.expect("assistant message should have meta");
        assert_eq!(m.model, Some("gpt-5.3".to_string()));
        assert_eq!(m.input_tokens, Some(50));
        assert_eq!(m.output_tokens, Some(10));
        assert_eq!(m.reasoning_tokens, Some(100));
        assert!(m.reasoning_trace.is_some());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn today_path_is_date_based_jsonl() {
        let dir = temp_sessions_dir("path_format");
        let session = Session::new(&dir).unwrap();
        let path = session.today_path();

        let filename = path.file_name().unwrap().to_str().unwrap();
        assert!(filename.ends_with(".jsonl"));
        assert_eq!(filename.len(), 16); // 2026-03-14.jsonl
        assert_eq!(&filename[4..5], "-");
        assert_eq!(&filename[7..8], "-");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sessions_dir_getter_returns_correct_path() {
        let dir = temp_sessions_dir("sessions_dir_getter");
        let session = Session::new(&dir).unwrap();

        assert_eq!(session.sessions_dir(), dir.as_path());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_restores_messages() {
        let dir = temp_sessions_dir("load_restore");

        {
            let mut session = Session::new(&dir).unwrap();
            session.add_user_message("first message").unwrap();
            session
                .append(ChatMessage::user("second message"), None)
                .unwrap();
        }

        {
            let mut session = Session::new(&dir).unwrap();
            session.load_today().unwrap();

            let history = session.history();
            assert!(history.len() >= 2);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_replays_previous_day_files() {
        let dir = temp_sessions_dir("load_multiple_days");
        let yesterday = dir.join("2026-03-18.jsonl");
        let today = dir.join("2026-03-19.jsonl");
        write_entries(
            &yesterday,
            &[SessionEntry {
                role: "user".to_string(),
                content: "yesterday".to_string(),
                ts: "2026-03-18T00:00:00Z".to_string(),
                meta: None,
                call_id: None,
                tool_name: None,
                tool_calls: None,
            }],
        );
        write_entries(
            &today,
            &[SessionEntry {
                role: "assistant".to_string(),
                content: "today".to_string(),
                ts: "2026-03-19T00:00:00Z".to_string(),
                meta: None,
                call_id: None,
                tool_name: None,
                tool_calls: None,
            }],
        );

        let mut session = Session::new(&dir).unwrap();
        session.load_today().unwrap();

        assert_eq!(session.history().len(), 2);
        assert!(matches!(session.history()[0].role, ChatRole::User));
        assert!(matches!(session.history()[1].role, ChatRole::Assistant));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_preserves_system_entries() {
        let dir = temp_sessions_dir("load_system_entries");

        {
            let mut session = Session::new(&dir).unwrap();
            session
                .append(ChatMessage::system("system note"), None)
                .unwrap();
            session.add_user_message("hello").unwrap();
        }

        let mut session = Session::new(&dir).unwrap();
        session.load_today().unwrap();

        assert_eq!(session.history().len(), 2);
        assert!(matches!(session.history()[0].role, ChatRole::System));
        assert!(matches!(session.history()[1].role, ChatRole::User));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_with_no_file_is_ok() {
        let dir = temp_sessions_dir("no_file");
        let mut session = Session::new(&dir).unwrap();

        session.load_today().unwrap();
        assert_eq!(session.history().len(), 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_trims_context_after_restore() {
        let dir = temp_sessions_dir("load_trim");

        let mut seed = Session::new(&dir).unwrap();
        let meta = TurnMeta {
            input_tokens: Some(50),
            output_tokens: Some(50),
            ..Default::default()
        };

        for _ in 0..2 {
            seed.append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(meta.clone()),
            )
            .unwrap();
        }

        let mut session = Session::new(&dir).unwrap();
        session.max_context_tokens = 50;
        session.load_today().unwrap();

        assert!(session.history().len() <= 1);
        assert!(session.total_tokens() <= 100);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn total_tokens_accumulates_from_meta() {
        let dir = temp_sessions_dir("token_count");
        let mut session = Session::new(&dir).unwrap();

        let meta1 = TurnMeta {
            input_tokens: Some(50),
            output_tokens: Some(10),
            ..Default::default()
        };
        let meta2 = TurnMeta {
            input_tokens: Some(100),
            output_tokens: Some(20),
            ..Default::default()
        };

        session.append(ChatMessage::user("q1"), None).unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(meta1),
            )
            .unwrap();
        session.append(ChatMessage::user("q2"), None).unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(meta2),
            )
            .unwrap();

        assert_eq!(session.total_tokens(), 180); // 50+10+100+20

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn estimate_tokens_returns_nonzero() {
        let dir = temp_sessions_dir("estimate_nonzero");
        let mut session = Session::new(&dir).unwrap();

        session.add_user_message("hello world").unwrap();
        assert!(session.estimate_context_tokens() > 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trim_uses_estimation_when_no_metadata() {
        let dir = temp_sessions_dir("trim_estimate");
        let mut session = Session::new(&dir).unwrap();
        session.set_max_context_tokens(1);

        session.add_user_message("one").unwrap();
        session.add_user_message("two").unwrap();
        session.add_user_message("three").unwrap();
        session.trim_context();

        assert_eq!(session.history().len(), 1);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn estimation_is_roughly_accurate() {
        let dir = temp_sessions_dir("estimate_rough");
        let mut session = Session::new(&dir).unwrap();

        session.add_user_message("hello world").unwrap();
        let tokens = session.estimate_context_tokens();
        assert!(
            (2..=4).contains(&tokens),
            "expected roughly 2-4 tokens, got {tokens}"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trim_drops_oldest_non_system_messages() {
        let dir = temp_sessions_dir("trim");
        let mut session = Session::new(&dir).unwrap();
        session.max_context_tokens = 50;

        let big_meta = TurnMeta {
            input_tokens: Some(30),
            output_tokens: Some(30),
            ..Default::default()
        };

        session
            .append(ChatMessage::system("instructions"), None)
            .unwrap();
        session
            .append(ChatMessage::user("old question"), None)
            .unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(big_meta.clone()),
            )
            .unwrap();
        session
            .append(ChatMessage::user("new question"), None)
            .unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(big_meta),
            )
            .unwrap();

        let history = session.history();
        assert!(history.len() < 5, "should have trimmed some messages");
        assert!(matches!(
            history.first().map(|message| &message.role),
            Some(ChatRole::System)
        ));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trim_does_not_pin_oldest_user_when_history_has_no_system_message() {
        let dir = temp_sessions_dir("trim_no_system_pin");
        let mut session = Session::new(&dir).unwrap();
        session.max_context_tokens = 50;

        let big_meta = TurnMeta {
            input_tokens: Some(30),
            output_tokens: Some(30),
            ..Default::default()
        };

        session
            .append(ChatMessage::user("oldest user"), None)
            .unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(big_meta.clone()),
            )
            .unwrap();
        session
            .append(ChatMessage::user("newer user"), None)
            .unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(big_meta),
            )
            .unwrap();

        assert!(
            session
                .history()
                .iter()
                .all(|message| match &message.content[..] {
                    [MessageContent::Text { text }] => text != "oldest user",
                    _ => true,
                }),
            "oldest user message should not be pinned forever"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trim_keeps_assistant_tool_roundtrip_intact() {
        let dir = temp_sessions_dir("trim_tool_roundtrip");
        let mut session = Session::new(&dir).unwrap();
        session.max_context_tokens = 50;

        let big_meta = TurnMeta {
            input_tokens: Some(30),
            output_tokens: Some(30),
            ..Default::default()
        };

        session
            .append(ChatMessage::system("instructions"), None)
            .unwrap();
        session.append(ChatMessage::user("first"), None).unwrap();
        session
            .append(
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: vec![MessageContent::ToolCall {
                        call: crate::llm::ToolCall {
                            id: "call-1".to_string(),
                            name: "execute".to_string(),
                            arguments: "{\"command\":\"echo hi\"}".to_string(),
                        },
                    }],
                },
                Some(big_meta.clone()),
            )
            .unwrap();
        session
            .append(
                ChatMessage::tool_result("call-1", "execute", "stdout:\nhi"),
                None,
            )
            .unwrap();
        session.append(ChatMessage::user("second"), None).unwrap();
        session
            .append(
                ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                Some(big_meta),
            )
            .unwrap();

        for (index, message) in session.history().iter().enumerate() {
            if message.role != ChatRole::Tool {
                continue;
            }

            assert!(
                index > 0,
                "tool result cannot be the first retained message"
            );
            let previous = &session.history()[index - 1];
            assert!(matches!(previous.role, ChatRole::Assistant));
            let tool_call_id = match &message.content[0] {
                MessageContent::ToolResult { result } => result.tool_call_id.as_str(),
                _ => panic!("expected tool result"),
            };
            let has_matching_call = previous.content.iter().any(|block| match block {
                MessageContent::ToolCall { call } => call.id == tool_call_id,
                _ => false,
            });
            assert!(
                has_matching_call,
                "tool result must retain its assistant tool call"
            );
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reasoning_trace_saved_but_not_in_loaded_context() {
        let dir = temp_sessions_dir("reasoning");

        {
            let mut session = Session::new(&dir).unwrap();
            session.add_user_message("think hard").unwrap();
            session
                .append(
                    ChatMessage::with_role(crate::llm::ChatRole::Assistant),
                    Some(TurnMeta {
                        reasoning_trace: Some("deep thoughts here".to_string()),
                        ..Default::default()
                    }),
                )
                .unwrap();
        }

        let content = fs::read_to_string({
            let s = Session::new(&dir).unwrap();
            s.today_path()
        })
        .unwrap();
        assert!(content.contains("deep thoughts here"));

        {
            let mut session = Session::new(&dir).unwrap();
            session.load_today().unwrap();

            for msg in session.history() {
                for block in &msg.content {
                    if let crate::llm::MessageContent::Text { text } = block {
                        assert!(
                            !text.contains("deep thoughts here"),
                            "reasoning trace must not leak into context"
                        );
                    }
                }
            }
        }

        fs::remove_dir_all(&dir).unwrap();
    }
}
