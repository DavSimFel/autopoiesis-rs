use std::collections::HashSet;

use tiktoken_rs::cl100k_base_singleton;

use crate::llm::{ChatMessage, ChatRole, MessageContent};

fn message_token_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .map(|block| match block {
            MessageContent::Text { text } => text.as_str(),
            MessageContent::ToolCall { call } => call.arguments.as_str(),
            MessageContent::ToolResult { result } => result.content.as_str(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Estimate tokens for plain text using the shared tokenizer.
pub fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        cl100k_base_singleton().encode_ordinary(text).len()
    }
}

/// Estimate tokens for a single structured chat message.
pub fn estimate_message_tokens(message: &ChatMessage) -> usize {
    estimate_text_tokens(&message_token_text(message))
}

/// Estimate tokens for a whole message slice.
pub fn estimate_messages_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
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

fn tool_result_call_id(message: &ChatMessage) -> Option<&str> {
    message.content.iter().find_map(|block| match block {
        MessageContent::ToolResult { result } => Some(result.tool_call_id.as_str()),
        _ => None,
    })
}

/// Return the assistant/tool round-trip covering `index`, if any.
pub fn history_group_range(history: &[ChatMessage], index: usize) -> Option<(usize, usize)> {
    match history.get(index)?.role {
        ChatRole::System => None,
        ChatRole::User => Some((index, index + 1)),
        ChatRole::Assistant => {
            let call_ids = tool_call_ids(&history[index]);
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
            let call_id = tool_result_call_id(&history[index])?;
            let mut start = index;

            for candidate in (0..index).rev() {
                if history[candidate].role != ChatRole::Assistant {
                    continue;
                }

                let call_ids = tool_call_ids(&history[candidate]);
                if call_ids.contains(call_id) {
                    start = candidate;
                    break;
                }
            }

            let call_ids = tool_call_ids(&history[start]);
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

/// Collect the newest whole history groups that fit within `max_tokens`.
///
/// Invariant: assistant/tool round-trips are the unit of replay. Callers must
/// only include or exclude a group as a whole.
pub fn collect_newest_group_ranges_within_budget<F>(
    history: &[ChatMessage],
    max_tokens: usize,
    mut group_token_count: F,
) -> Vec<(usize, usize)>
where
    F: FnMut(usize, usize) -> usize,
{
    if history.is_empty() {
        return Vec::new();
    }

    let mut index = history.len();
    let mut current_tokens = 0usize;
    let mut selected = Vec::new();

    while index > 0 {
        index -= 1;

        let Some((start, end)) = history_group_range(history, index) else {
            continue;
        };

        let group_tokens = group_token_count(start, end);
        if current_tokens + group_tokens > max_tokens {
            break;
        }

        selected.push((start, end));
        current_tokens += group_tokens;
        index = start;
    }

    selected.reverse();
    selected
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::principal::Principal;

    fn assistant_with_blocks(blocks: Vec<MessageContent>) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: blocks,
        }
    }

    #[test]
    fn estimate_message_tokens_counts_tool_call_arguments() {
        let message = assistant_with_blocks(vec![MessageContent::ToolCall {
            call: crate::llm::ToolCall {
                id: "call-1".to_string(),
                name: "execute".to_string(),
                arguments: "{\"command\":\"echo hi\"}".to_string(),
            },
        }]);

        assert!(estimate_message_tokens(&message) > 0);
    }

    #[test]
    fn history_group_range_keeps_assistant_tool_roundtrip_intact() {
        let history = vec![
            ChatMessage::user("before"),
            assistant_with_blocks(vec![
                MessageContent::text("alpha"),
                MessageContent::ToolCall {
                    call: crate::llm::ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hi\"}".to_string(),
                    },
                },
            ]),
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            ChatMessage::user("after"),
        ];

        assert_eq!(history_group_range(&history, 1), Some((1, 3)));
        assert_eq!(history_group_range(&history, 2), Some((1, 3)));
    }

    #[test]
    fn collect_newest_group_ranges_within_budget_returns_whole_groups() {
        let history = vec![
            ChatMessage::user("old"),
            assistant_with_blocks(vec![
                MessageContent::text("alpha"),
                MessageContent::ToolCall {
                    call: crate::llm::ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hi\"}".to_string(),
                    },
                },
            ]),
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            ChatMessage::user("new"),
        ];

        let selected =
            collect_newest_group_ranges_within_budget(&history, usize::MAX, |start, end| {
                estimate_messages_tokens(&history[start..end])
            });

        assert_eq!(selected, vec![(0, 1), (1, 3), (3, 4)]);
    }

    #[test]
    fn collect_newest_group_ranges_within_budget_stops_on_budget_cutoff() {
        let history = vec![
            ChatMessage::user("old"),
            assistant_with_blocks(vec![
                MessageContent::text("alpha"),
                MessageContent::ToolCall {
                    call: crate::llm::ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hi\"}".to_string(),
                    },
                },
            ]),
            ChatMessage::tool_result_with_principal(
                "call-1",
                "execute",
                "ok",
                Some(Principal::System),
            ),
            ChatMessage::user("new"),
        ];

        let newest_group_tokens = estimate_messages_tokens(&history[3..4]);
        let selected = collect_newest_group_ranges_within_budget(
            &history,
            newest_group_tokens,
            |start, end| estimate_messages_tokens(&history[start..end]),
        );

        assert_eq!(selected, vec![(3, 4)]);
    }

    #[test]
    fn estimate_messages_tokens_uses_shared_message_estimator() {
        let messages = vec![
            ChatMessage::user("hello"),
            assistant_with_blocks(vec![
                MessageContent::text("alpha"),
                MessageContent::ToolCall {
                    call: crate::llm::ToolCall {
                        id: "call-2".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hi\"}".to_string(),
                    },
                },
            ]),
        ];

        assert!(estimate_messages_tokens(&messages) >= estimate_message_tokens(&messages[0]));
    }
}
