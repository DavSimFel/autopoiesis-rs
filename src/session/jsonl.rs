use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{trace, warn};

use super::{Session, SessionEntry};
use crate::llm::{ChatMessage, ChatRole, MessageContent, TurnMeta};
use crate::principal::Principal;
use crate::util::utc_timestamp;

impl Session {
    // Persistence boundary: serialize and replay ordered blocks exactly for canonical rows;
    // only fall back to the flattened legacy shape when the stored row predates `blocks`.
    pub(super) fn token_total(meta: Option<&TurnMeta>) -> u64 {
        meta.map_or(0, |meta| {
            meta.input_tokens.unwrap_or(0) + meta.output_tokens.unwrap_or(0)
        })
    }

    pub(super) fn to_entry(message: &ChatMessage, meta: Option<&TurnMeta>) -> SessionEntry {
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
                MessageContent::ToolCall { .. } => None,
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
            blocks: message.content.clone(),
            ts: utc_timestamp(),
            meta: meta.cloned(),
            principal: Some(message.principal),
            call_id,
            tool_name,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }
    }

    pub(super) fn append_entry_to_file(path: &Path, entry: &SessionEntry) -> Result<()> {
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

    pub(super) fn replay_principal(role: &str, principal: Option<Principal>) -> Principal {
        principal.unwrap_or(match role {
            "user" => Principal::User,
            "system" | "tool" => Principal::System,
            "assistant" => Principal::Agent,
            _ => Principal::System,
        })
    }

    pub(super) fn message_from_entry(entry: SessionEntry) -> (Option<ChatMessage>, u64) {
        let SessionEntry {
            role,
            content: entry_content,
            blocks,
            ts: _,
            meta,
            principal,
            call_id,
            tool_name,
            tool_calls,
        } = entry;

        let token_delta = Self::token_total(meta.as_ref());
        let principal = Self::replay_principal(role.as_str(), principal);

        let message = match role.as_str() {
            "system" => Some(if !blocks.is_empty() {
                ChatMessage {
                    role: ChatRole::System,
                    principal,
                    content: blocks,
                }
            } else {
                ChatMessage::system_with_principal(entry_content, Some(principal))
            }),
            "user" => Some(if !blocks.is_empty() {
                ChatMessage {
                    role: ChatRole::User,
                    principal,
                    content: blocks,
                }
            } else {
                ChatMessage::user_with_principal(entry_content, Some(principal))
            }),
            "assistant" => Some(if !blocks.is_empty() {
                ChatMessage {
                    role: ChatRole::Assistant,
                    principal,
                    content: blocks,
                }
            } else {
                let mut content = Vec::new();
                if !entry_content.is_empty() {
                    content.push(MessageContent::text(entry_content));
                }
                if let Some(calls) = tool_calls {
                    for call in calls {
                        content.push(MessageContent::ToolCall { call });
                    }
                }
                let mut message =
                    ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(principal));
                message.content = content;
                message
            }),
            "tool" => {
                if !blocks.is_empty() {
                    let valid_blocks = blocks
                        .iter()
                        .all(|block| matches!(block, MessageContent::ToolResult { .. }));
                    if !valid_blocks {
                        warn!("warning: dropping malformed tool entry with non-tool-result blocks");
                        return (None, token_delta);
                    }

                    Some(ChatMessage {
                        role: ChatRole::Tool,
                        principal,
                        content: blocks,
                    })
                } else {
                    match (&call_id, &tool_name) {
                        (None, None) if entry_content.is_empty() => None,
                        (None, _) | (_, None) => {
                            warn!(
                                "warning: dropping tool entry with missing call_id or tool_name \
                                     (call_id={:?}, tool_name={:?})",
                                call_id, tool_name
                            );
                            None
                        }
                        (Some(call_id), Some(tool_name)) => {
                            Some(ChatMessage::tool_result_with_principal(
                                call_id.clone(),
                                tool_name.clone(),
                                entry_content,
                                Some(principal),
                            ))
                        }
                    }
                }
            }
            _ => {
                warn!(role = %role, "warning: dropping session entry with unknown role");
                None
            }
        };

        (message, token_delta)
    }

    pub(super) fn session_paths(&self) -> Result<Vec<PathBuf>> {
        if !self.sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.sessions_dir)
            .with_context(|| format!("failed to read {}", self.sessions_dir.display()))?
        {
            let entry = entry.with_context(|| {
                format!(
                    "failed to read directory entry in {}",
                    self.sessions_dir.display()
                )
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// Load persisted session history from disk, replaying all dated JSONL files in order.
    pub fn load_today(&mut self) -> Result<()> {
        self.messages.clear();
        self.message_tokens.clear();
        self.total_tokens = 0;
        self.session_total_tokens = 0;

        for path in self.session_paths()? {
            trace!(path = %path.display(), "replaying session file");
            let file = File::open(&path)
                .with_context(|| format!("failed to open sessions file {}", path.display()))?;
            let reader = BufReader::new(file);
            let mut line_index = 0usize;

            for raw_line in reader.lines() {
                line_index += 1;
                let raw_line = raw_line?;
                if raw_line.trim().is_empty() {
                    continue;
                }

                trace!(path = %path.display(), line = line_index, "replaying session line");

                let entry: SessionEntry = serde_json::from_str(&raw_line).with_context(|| {
                    format!("failed to parse session entry in {}", path.display())
                })?;

                let (message, token_delta) = Self::message_from_entry(entry);
                if let Some(message) = message {
                    self.messages.push(message);
                    self.message_tokens.push(token_delta);
                    self.total_tokens += token_delta;
                    self.session_total_tokens += token_delta;
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
}
