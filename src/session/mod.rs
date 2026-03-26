//! Persistent chat sessions stored as daily JSONL files.
//!
//! Each day gets one file: `sessions/2026-03-14.jsonl`.
//! Messages are appended in real time. On load, the file is
//! replayed to rebuild in-memory state.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::llm::{ChatMessage, MessageContent, TurnMeta};
use crate::principal::Principal;

mod budget;
mod delegation_hint;
mod jsonl;
#[cfg(test)]
mod tests;
mod trimming;

/// One line in the JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    /// Role: system, user, assistant, tool.
    pub role: String,
    /// Message content.
    pub content: String,
    /// Ordered structured content blocks, persisted as the canonical transcript form.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<MessageContent>,
    /// ISO 8601 UTC timestamp.
    pub ts: String,
    /// Provider metadata (only on assistant messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<TurnMeta>,
    /// Message trust level. Legacy entries may omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<Principal>,
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
    /// Cumulative session token count including trimmed history.
    session_total_tokens: u64,
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
            session_total_tokens: 0,
            sessions_dir: sessions_dir.into(),
            message_tokens: Vec::new(),
        };

        Ok(session)
    }

    /// Append a message and persist it to today's JSONL file.
    pub fn append(&mut self, message: ChatMessage, meta: Option<TurnMeta>) -> Result<()> {
        let token_delta = Self::token_total(meta.as_ref());
        let entry = Self::to_entry(&message, meta.as_ref());
        let should_trim = Self::can_trim_after_append(&message);

        debug!(
            role = ?message.role,
            principal = ?message.principal,
            token_delta,
            should_trim,
            "append session message"
        );

        Self::append_entry_to_file(&self.today_path(), &entry)?;
        self.messages.push(message);
        self.message_tokens.push(token_delta);
        self.total_tokens += token_delta;
        self.session_total_tokens += token_delta;

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

    /// Update max context tokens for a session.
    pub fn set_max_context_tokens(&mut self, max_context_tokens: u64) {
        self.max_context_tokens = max_context_tokens;
    }
}
