use std::collections::HashMap;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StreamedTurn, StopReason, ToolCall,
};

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    api_key: String,
    model: String,
    max_tokens: Option<u32>,
    client: Client,
}

impl OpenAIProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>, max_tokens: Option<u32>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            max_tokens,
            client: Client::new(),
        }
    }

    fn map_stop_reason(reason: Option<&str>) -> StopReason {
        match reason {
            Some("stop") => StopReason::Stop,
            Some("tool_calls") => StopReason::ToolCalls,
            Some("length") => StopReason::Length,
            Some("content_filter") => StopReason::ContentFilter,
            Some(other) => StopReason::Other(other.to_string()),
            None => StopReason::Stop,
        }
    }

    fn to_openai_messages(messages: &[ChatMessage]) -> Vec<OpenAIMessage> {
        messages
            .iter()
            .map(|message| {
                let role = match message.role {
                    ChatRole::System => "system",
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                    ChatRole::Tool => "tool",
                };

                let mut content = String::new();
                let mut tool_calls = Vec::<OpenAIMessageToolCall>::new();
                let mut tool_call_id = None;
                let mut tool_name = None;

                for (index, block) in message.content.iter().enumerate() {
                    match block {
                        MessageContent::Text { text } => {
                            content.push_str(text);
                        }
                        MessageContent::ToolCall { call } => {
                            tool_calls.push(OpenAIMessageToolCall {
                                index,
                                id: call.id.clone(),
                                kind: "function".to_string(),
                                function: OpenAIFunctionCall {
                                    name: call.name.clone(),
                                    arguments: call.arguments.clone(),
                                },
                            });
                        }
                        MessageContent::ToolResult { result } => {
                            tool_call_id = Some(result.tool_call_id.clone());
                            tool_name = Some(result.name.clone());
                            content = result.content.clone();
                        }
                    }
                }

                OpenAIMessage {
                    role: role.to_string(),
                    content: (!content.is_empty()).then_some(content),
                    tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                    name: tool_name,
                    tool_call_id,
                }
            })
            .collect()
    }

    fn build_tools(tools: &[FunctionTool]) -> Vec<OpenAIProviderTool> {
        tools
            .iter()
            .map(|tool| OpenAIProviderTool {
                kind: "function".to_string(),
                function: OpenAIProviderFunction {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.parameters.clone(),
                },
            })
            .collect()
    }

    fn stream_turn_from_pending(
        tool_calls: HashMap<usize, StreamingToolCall>,
        assistant_content: String,
        stop_reason: StopReason,
    ) -> StreamedTurn {
        if tool_calls.is_empty() {
            return StreamedTurn {
                assistant_message: ChatMessage::assistant_text(assistant_content),
                tool_calls: Vec::new(),
                stop_reason,
            };
        }

        let calls: Vec<ToolCall> = tool_calls
            .into_values()
            .map(|entry| ToolCall {
                id: entry.id,
                name: entry.name,
                arguments: entry.arguments,
            })
            .collect();

        StreamedTurn {
            assistant_message: ChatMessage::assistant_tool_calls(calls.clone()),
            tool_calls: calls,
            stop_reason,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAIProvider {
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        on_token: &mut (dyn FnMut(String) + Send),
    ) -> Result<StreamedTurn> {
        let request = OpenAIStreamRequest {
            model: self.model.clone(),
            messages: Self::to_openai_messages(messages),
            stream: true,
            tools: Self::build_tools(tools),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            max_tokens: self.max_tokens,
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .context("failed to send request to OpenAI")?;

        let response = response
            .error_for_status()
            .context("OpenAI API returned an error response")?;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut pending_tool_calls: HashMap<usize, StreamingToolCall> = HashMap::new();
        let mut assistant_content = String::new();
        let mut stop_reason = StopReason::Stop;

        while let Some(chunk) = stream.try_next().await.context("failed to read OpenAI stream")? {
            let chunk_text = std::str::from_utf8(&chunk)
                .context("received non-utf8 data from OpenAI stream")?;
            buffer.push_str(chunk_text);

            while let Some(line_break) = buffer.find('\n') {
                let line = buffer[..line_break].trim().to_string();
                buffer.drain(0..line_break + 1);

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if !line.starts_with("data:") {
                    continue;
                }

                let data = line.trim_start_matches("data:").trim();
                if data == "[DONE]" {
                    return Ok(Self::stream_turn_from_pending(
                        pending_tool_calls,
                        assistant_content,
                        stop_reason,
                    ));
                }

                let chunk: OpenAIStreamResponse = serde_json::from_str(data)
                    .with_context(|| format!("failed to parse OpenAI stream chunk: {data}"))?;

                for choice in chunk.choices {
                    if let Some(reason) = choice.finish_reason.as_deref() {
                        stop_reason = Self::map_stop_reason(Some(reason));
                    }

                    if let Some(delta) = choice.delta {
                        if let Some(text) = delta.content {
                            assistant_content.push_str(&text);
                            on_token(text.clone());
                        }

                        if let Some(calls) = delta.tool_calls {
                            for call in calls {
                                let entry = pending_tool_calls.entry(call.index).or_insert_with(|| {
                                    StreamingToolCall {
                                        id: call.id.clone().unwrap_or_else(|| format!("tool-call-{}", call.index)),
                                        name: String::new(),
                                        arguments: String::new(),
                                    }
                                });

                                if let Some(id) = call.id {
                                    entry.id = id;
                                }

                                if let Some(function) = call.function {
                                    if let Some(name) = function.name {
                                        entry.name = name;
                                    }
                                    if let Some(arguments) = function.arguments {
                                        entry.arguments.push_str(&arguments);
                                    }
                                }
                            }
                        }
                    }
                }

                if stop_reason == StopReason::ToolCalls {
                    return Ok(Self::stream_turn_from_pending(
                        pending_tool_calls,
                        assistant_content,
                        StopReason::ToolCalls,
                    ));
                }
            }
        }

        Ok(Self::stream_turn_from_pending(
            pending_tool_calls,
            assistant_content,
            stop_reason,
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIStreamRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    stream: bool,
    tools: Vec<OpenAIProviderTool>,
    #[serde(rename = "tool_choice")]
    tool_choice: Option<String>,
    #[serde(rename = "max_tokens")]
    max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIProviderTool {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAIProviderFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIProviderFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(rename = "tool_calls", skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIMessageToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "tool_call_id", skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIMessageToolCall {
    index: usize,
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: Option<OpenAIStreamDelta>,
    #[serde(rename = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamDelta {
    content: Option<String>,
    #[serde(rename = "tool_calls")]
    tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<OpenAIStreamFunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamFunctionCall {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug)]
struct StreamingToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_openai_request_uses_function_tools() {
        let tool = FunctionTool {
            name: "execute".to_string(),
            description: "run shell".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        };

        let request_tools = OpenAIStreamRequest {
            model: "gpt-4o".to_string(),
            messages: vec![],
            stream: true,
            tools: OpenAIProvider::build_tools(&[tool]),
            tool_choice: Some("auto".to_string()),
            max_tokens: None,
        };

        let serialized = serde_json::to_value(request_tools).expect("serialize request");
        assert_eq!(serialized["tools"][0]["type"], json!("function"));
        assert_eq!(serialized["tools"][0]["function"]["name"], json!("execute"));
    }

    #[test]
    fn test_tool_messages_convert() {
        let tool_call = ToolCall {
            id: "call-123".to_string(),
            name: "execute".to_string(),
            arguments: "{\"command\":\"ls\"}".to_string(),
        };

        let message = ChatMessage::assistant_tool_calls(vec![tool_call]);
        let converted = OpenAIProvider::to_openai_messages(std::slice::from_ref(&message));

        assert_eq!(converted[0].tool_calls.as_ref().unwrap()[0].id, "call-123");
        assert_eq!(converted[0].tool_calls.as_ref().unwrap()[0].function.name, "execute");
    }
}
