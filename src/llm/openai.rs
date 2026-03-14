use std::collections::HashMap;

use anyhow::{Context, Result};
use futures::TryStreamExt;
use reqwest::Client;
// serde used by tests
use serde_json::{json, Value};

use crate::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StopReason, StreamedTurn,
    ToolCall,
};

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    model: String,
    max_output_tokens: Option<u32>,
    reasoning_effort: Option<String>,
    client: Client,
}

impl OpenAIProvider {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        max_output_tokens: Option<u32>,
        reasoning_effort: Option<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            max_output_tokens,
            reasoning_effort,
            client: Client::new(),
        }
    }

    /// Extract system prompt from messages and convert remaining to Responses API input format
    fn build_input(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut instructions = None;
        let mut input = Vec::new();

        for msg in messages {
            match msg.role {
                ChatRole::System => {
                    // Extract system prompt as instructions
                    for block in &msg.content {
                        if let MessageContent::Text { text } = block {
                            instructions = Some(text.clone());
                        }
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
                    // Assistant messages may contain text and/or tool calls
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
                                    "id": call.id,
                                    "name": call.name,
                                    "arguments": call.arguments
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                ChatRole::Tool => {
                    // Tool results become function_call_output
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

    /// Convert FunctionTool to Responses API tool format
    fn build_tools(tools: &[FunctionTool]) -> Vec<Value> {
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
}

#[async_trait::async_trait]
impl LlmProvider for OpenAIProvider {
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        on_token: &mut (dyn FnMut(String) + Send),
    ) -> Result<StreamedTurn> {
        let (instructions, input) = Self::build_input(messages);
        let tools_json = Self::build_tools(tools);

        let mut request = json!({
            "model": self.model,
            "input": input,
            "stream": true,
            "store": false,
        });

        if let Some(ref instructions) = instructions {
            request["instructions"] = json!(instructions);
        }

        if !tools_json.is_empty() {
            request["tools"] = json!(tools_json);
        }

        if let Some(ref effort) = self.reasoning_effort {
            request["reasoning"] = json!({"effort": effort});
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&request)
            .send()
            .await
            .context("failed to send request to OpenAI")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut assistant_content = String::new();

        // Track function calls: call_id -> (name, arguments)
        let mut pending_calls: HashMap<String, (String, String)> = HashMap::new();
        let mut current_call_id: Option<String> = None;
        let mut current_call_name: Option<String> = None;
        let mut current_call_args = String::new();
        let mut stop_reason = StopReason::Stop;

        while let Some(chunk) = stream.try_next().await.context("stream read error")? {
            let chunk_text = std::str::from_utf8(&chunk)
                .context("received non-utf8 data from stream")?;
            buffer.push_str(chunk_text);

            while let Some(line_break) = buffer.find('\n') {
                let line = buffer[..line_break].trim().to_string();
                buffer.drain(0..line_break + 1);

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }

                if line == "data: [DONE]" {
                    break;
                }

                let data = if let Some(stripped) = line.strip_prefix("data: ") {
                    stripped
                } else {
                    continue;
                };

                let event: Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match event_type {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                            assistant_content.push_str(delta);
                            on_token(delta.to_string());
                        }
                    }

                    "response.function_call_arguments.delta" => {
                        if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                            current_call_args.push_str(delta);
                        }
                        // Capture call_id and name from the first delta if present
                        if current_call_id.is_none() {
                            if let Some(id) = event.get("call_id").and_then(|v| v.as_str()) {
                                current_call_id = Some(id.to_string());
                            }
                        }
                        if current_call_name.is_none() {
                            if let Some(name) = event.get("name").and_then(|v| v.as_str()) {
                                current_call_name = Some(name.to_string());
                            }
                        }
                    }

                    "response.function_call_arguments.done" => {
                        let call_id = event
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .or(current_call_id.take())
                            .unwrap_or_else(|| "unknown".to_string());

                        let name = event
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .or(current_call_name.take())
                            .unwrap_or_else(|| "unknown".to_string());

                        let arguments = event
                            .get("arguments")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| std::mem::take(&mut current_call_args));

                        pending_calls.insert(call_id, (name, arguments));
                        current_call_args.clear();
                        stop_reason = StopReason::ToolCalls;
                    }

                    "response.output_item.done" => {
                        if let Some(item) = event.get("item") {
                            if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                                let call_id = item
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let name = item
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let arguments = item
                                    .get("arguments")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("{}")
                                    .to_string();

                                pending_calls.insert(call_id, (name, arguments));
                                stop_reason = StopReason::ToolCalls;
                            }
                        }
                    }

                    "response.completed" => {
                        // Final event — we can extract output from here too if needed
                        if pending_calls.is_empty() {
                            stop_reason = StopReason::Stop;
                        }
                    }

                    _ => {
                        // Ignore other event types (response.created, response.in_progress, etc.)
                    }
                }
            }
        }

        // Build tool calls from pending
        let tool_calls: Vec<ToolCall> = pending_calls
            .into_iter()
            .map(|(id, (name, arguments))| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();

        // Build assistant message
        let mut assistant_msg = ChatMessage::with_role(ChatRole::Assistant);
        if !assistant_content.is_empty() {
            assistant_msg
                .content
                .push(MessageContent::Text { text: assistant_content });
        }
        for tc in &tool_calls {
            assistant_msg.content.push(MessageContent::ToolCall {
                call: tc.clone(),
            });
        }

        Ok(StreamedTurn {
            assistant_message: assistant_msg,
            tool_calls,
            stop_reason,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_input_extracts_instructions() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];
        let (instructions, input) = OpenAIProvider::build_input(&messages);
        assert_eq!(instructions, Some("You are helpful".to_string()));
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "Hello");
    }

    #[test]
    fn test_build_tools_flat_format() {
        let tools = vec![FunctionTool {
            name: "execute".to_string(),
            description: "Run a command".to_string(),
            parameters: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }];
        let result = OpenAIProvider::build_tools(&tools);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["name"], "execute");
        // NOT nested under "function"
        assert!(result[0].get("function").is_none());
    }
}
