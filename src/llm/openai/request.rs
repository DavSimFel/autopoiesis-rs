use serde_json::{Value, json};

use crate::llm::{ChatMessage, ChatRole, FunctionTool, MessageContent};

pub(crate) fn build_input(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut instructions = None;
    let mut input = Vec::new();

    for msg in messages {
        match msg.role {
            ChatRole::System => {
                let text_blocks = msg
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        MessageContent::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();

                if instructions.is_none() {
                    if !text_blocks.is_empty() {
                        instructions = Some(text_blocks.join("\n"));
                    }
                    continue;
                }

                for text in text_blocks {
                    input.push(json!({
                        "role": "system",
                        "content": text
                    }));
                }
            }
            ChatRole::User => {
                for block in &msg.content {
                    if let MessageContent::Text { text } = block {
                        input.push(json!({
                            "role": "user",
                            "content": text
                        }));
                    }
                }
            }
            ChatRole::Assistant => {
                // Assistant messages may include text chunks and/or prior tool call stubs.
                for block in &msg.content {
                    match block {
                        MessageContent::Text { text } => {
                            input.push(json!({
                                "role": "assistant",
                                "content": text
                            }));
                        }
                        MessageContent::ToolCall { call } => {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": call.name,
                                "arguments": call.arguments
                            }));
                        }
                        _ => {}
                    }
                }
            }
            ChatRole::Tool => {
                // Tool results map to function_call_output in the responses API format.
                for block in &msg.content {
                    if let MessageContent::ToolResult { result } = block {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": result.tool_call_id,
                            "output": result.content
                        }));
                    }
                }
            }
        }
    }

    (instructions, input)
}

pub(crate) fn build_tools(tools: &[FunctionTool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters
            })
        })
        .collect()
}
