//! Persistent chat sessions stored as daily JSONL files.
//!
//! Each day gets one file: `sessions/2026-03-14.jsonl`.
//! Messages are appended in real time. On load, the file is
//! replayed to rebuild in-memory state.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::llm::ChatMessage;

/// Metadata returned by the provider for a single completion.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnMeta {
    /// Model that produced this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tokens consumed by input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Tokens produced as output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Tokens used for reasoning (not injectable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// Reasoning trace text (saved but never re-injected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_trace: Option<String>,
}

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
}

impl Session {
    /// Start a session with a system prompt.
    pub fn new(system_prompt: impl Into<String>, sessions_dir: impl Into<PathBuf>) -> Self {
        Self {
            messages: vec![ChatMessage::system(system_prompt)],
            max_context_tokens: 100_000,
            total_tokens: 0,
            sessions_dir: sessions_dir.into(),
        }
    }

    /// Append a message and persist it to today's JSONL file.
    pub fn append(&mut self, message: ChatMessage, meta: Option<TurnMeta>) -> Result<()> {
        // TODO: implement
        todo!()
    }

    /// Add a user prompt message with timestamp.
    pub fn add_user_message(&mut self, message: impl Into<String>) -> Result<()> {
        // TODO: implement
        todo!()
    }

    /// Immutable access to full message history.
    pub fn history(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Load today's session from disk if it exists.
    pub fn load_today(&mut self) -> Result<()> {
        // TODO: implement
        todo!()
    }

    /// Get the path for today's JSONL file.
    pub fn today_path(&self) -> PathBuf {
        // TODO: implement
        todo!()
    }

    /// Trim oldest non-system messages when over token limit.
    fn trim_context(&mut self) {
        // TODO: implement
        todo!()
    }

    /// Get total token count from provider metadata.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// List available session files.
    pub fn list_sessions(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
        // TODO: implement
        todo!()
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

    // --- Persistence ---

    #[test]
    fn append_user_message_writes_jsonl_line() {
        let dir = temp_sessions_dir("user_msg");
        let mut session = Session::new("You are helpful.", &dir);

        session.add_user_message("hello").unwrap();

        let path = session.today_path();
        assert!(path.exists(), "JSONL file should be created");

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // system prompt + user message
        assert_eq!(lines.len(), 2);

        let entry: SessionEntry = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry.role, "user");
        assert!(entry.content.contains("hello"));
        assert!(!entry.ts.is_empty());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn append_assistant_message_includes_meta() {
        let dir = temp_sessions_dir("asst_meta");
        let mut session = Session::new("You are helpful.", &dir);

        let meta = TurnMeta {
            model: Some("gpt-5.3".to_string()),
            input_tokens: Some(50),
            output_tokens: Some(10),
            reasoning_tokens: Some(100),
            reasoning_trace: Some("I thought about it".to_string()),
        };

        session
            .append(ChatMessage::user("hi"), None)
            .unwrap();
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
        let session = Session::new("test", &dir);
        let path = session.today_path();

        let filename = path.file_name().unwrap().to_str().unwrap();
        // Format: YYYY-MM-DD.jsonl
        assert!(filename.ends_with(".jsonl"));
        assert_eq!(filename.len(), 15); // 2026-03-14.jsonl
        assert_eq!(&filename[4..5], "-");
        assert_eq!(&filename[7..8], "-");

        fs::remove_dir_all(&dir).unwrap();
    }

    // --- Load / Resume ---

    #[test]
    fn load_today_restores_messages() {
        let dir = temp_sessions_dir("load_restore");

        // Write a session
        {
            let mut session = Session::new("You are helpful.", &dir);
            session.add_user_message("first message").unwrap();
            session
                .append(ChatMessage::user("second message"), None)
                .unwrap();
        }

        // Load it in a new session
        {
            let mut session = Session::new("You are helpful.", &dir);
            session.load_today().unwrap();

            let history = session.history();
            // system + 2 user messages
            assert!(history.len() >= 3);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_today_with_no_file_is_ok() {
        let dir = temp_sessions_dir("no_file");
        let mut session = Session::new("You are helpful.", &dir);

        // Should not error — just starts empty
        session.load_today().unwrap();
        assert_eq!(session.history().len(), 1); // just system prompt

        fs::remove_dir_all(&dir).unwrap();
    }

    // --- Token Management ---

    #[test]
    fn total_tokens_accumulates_from_meta() {
        let dir = temp_sessions_dir("token_count");
        let mut session = Session::new("test", &dir);

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
            .append(ChatMessage::with_role(crate::llm::ChatRole::Assistant), Some(meta1))
            .unwrap();
        session.append(ChatMessage::user("q2"), None).unwrap();
        session
            .append(ChatMessage::with_role(crate::llm::ChatRole::Assistant), Some(meta2))
            .unwrap();

        assert_eq!(session.total_tokens(), 180); // 50+10+100+20

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trim_drops_oldest_non_system_messages() {
        let dir = temp_sessions_dir("trim");
        let mut session = Session::new("system prompt", &dir);
        // Set a very low limit to force trimming
        session.max_context_tokens = 50;

        // Add messages with token metadata that exceeds the limit
        let big_meta = TurnMeta {
            input_tokens: Some(30),
            output_tokens: Some(30),
            ..Default::default()
        };

        session.append(ChatMessage::user("old question"), None).unwrap();
        session
            .append(ChatMessage::with_role(crate::llm::ChatRole::Assistant), Some(big_meta.clone()))
            .unwrap();
        session.append(ChatMessage::user("new question"), None).unwrap();
        session
            .append(ChatMessage::with_role(crate::llm::ChatRole::Assistant), Some(big_meta))
            .unwrap();

        // After trim, system prompt should survive, oldest messages should be gone
        let history = session.history();
        assert!(history.len() < 5, "should have trimmed some messages");
        // First message is always system
        assert_eq!(history[0].role, crate::llm::ChatRole::System);

        fs::remove_dir_all(&dir).unwrap();
    }

    // --- Reasoning Traces ---

    #[test]
    fn reasoning_trace_saved_but_not_in_loaded_context() {
        let dir = temp_sessions_dir("reasoning");

        // Write a session with reasoning trace
        {
            let mut session = Session::new("system", &dir);
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

        // Verify trace is in the file
        let content = fs::read_to_string({
            let s = Session::new("system", &dir);
            s.today_path()
        })
        .unwrap();
        assert!(content.contains("deep thoughts here"));

        // Load session — reasoning trace should NOT be in the messages
        {
            let mut session = Session::new("system", &dir);
            session.load_today().unwrap();

            // The assistant message content should NOT contain the reasoning trace
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

    // --- Listing ---

    #[test]
    fn list_sessions_returns_jsonl_files_sorted() {
        let dir = temp_sessions_dir("list");

        // Create some fake session files
        fs::write(dir.join("2026-03-12.jsonl"), "").unwrap();
        fs::write(dir.join("2026-03-14.jsonl"), "").unwrap();
        fs::write(dir.join("2026-03-13.jsonl"), "").unwrap();
        fs::write(dir.join("not-a-session.txt"), "").unwrap();

        let sessions = Session::list_sessions(&dir).unwrap();
        assert_eq!(sessions.len(), 3);
        // Should be sorted
        assert!(sessions[0].to_str().unwrap().contains("2026-03-12"));
        assert!(sessions[2].to_str().unwrap().contains("2026-03-14"));

        fs::remove_dir_all(&dir).unwrap();
    }
}
