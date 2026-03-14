//! OpenAI Responses API client for streaming completions and tool calls.

use std::collections::HashMap;

use anyhow::{Context, Result};
use reqwest::Client;
// serde is used by tests and by request/response payload assembly.
use serde_json::{json, Value};

use crate::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StreamedTurn, StopReason,
    ToolCall,
};

/// HTTP client and request settings for the OpenAI-compatible Responses API.
#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    model: String,
    reasoning_effort: Option<String>,
    client: Client,
}

impl OpenAIProvider {
    pub fn new(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.into(),
            model: model.into(),
            reasoning_effort,
            client: Client::new(),
        }
    }

    /// Extract the system prompt and convert prior messages to the responses API item format.
    fn build_input(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut instructions = None;
        let mut input = Vec::new();

        for msg in messages {
            match msg.role {
                ChatRole::System => {
                    // Keep only the latest system instruction message.
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

    /// Convert internal tool descriptions into Responses API `tools` payloads.
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

#[derive(Debug, Clone, PartialEq)]
enum SseEvent {
    TextDelta(String),
    FunctionCallArgumentsDelta {
        call_id: Option<String>,
        name: Option<String>,
        delta: Option<String>,
    },
    FunctionCallArgumentsDone {
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    FunctionCallOutputItemDone {
        call_id: String,
        name: String,
        arguments: String,
    },
    Completed,
    Done,
}

fn parse_sse_line(line: &str) -> Option<SseEvent> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return None;
    }
    if line == "data: [DONE]" {
        return Some(SseEvent::Done);
    }

    let data = match line.strip_prefix("data: ") {
        Some(data) => data,
        None => return None,
    };

    let event: Value = serde_json::from_str(data).ok()?;
    let event_type = event.get("type").and_then(|value| value.as_str())?;

    match event_type {
        "response.output_text.delta" => {
            event.get("delta").and_then(Value::as_str).map(|delta| {
                SseEvent::TextDelta(delta.to_string())
            })
        }
        "response.function_call_arguments.delta" => Some(SseEvent::FunctionCallArgumentsDelta {
            call_id: event
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            name: event
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            delta: event
                .get("delta")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }),
        "response.function_call_arguments.done" => Some(SseEvent::FunctionCallArgumentsDone {
            call_id: event
                .get("call_id")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            name: event
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            arguments: event
                .get("arguments")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }),
        "response.output_item.done" => {
            let item = event.get("item")?;
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return None;
            }
            Some(SseEvent::FunctionCallOutputItemDone {
                call_id: item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                arguments: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_string(),
            })
        }
        "response.completed" => Some(SseEvent::Completed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRole, MessageContent, ToolCall};

    fn collect_tokens_from_sse_chunks(chunks: &[&[u8]]) -> (Vec<String>, bool) {
        let mut buffer = String::new();
        let mut tokens = Vec::new();
        let mut done = false;

        for chunk in chunks {
            let chunk = std::str::from_utf8(chunk).expect("mock SSE chunks should be valid utf-8");
            buffer.push_str(chunk);

            loop {
                let line_end = match buffer.find('\n') {
                    Some(index) => index,
                    None => break,
                };

                let line = buffer[..line_end].to_string();
                buffer.drain(..line_end + 1);

                match parse_sse_line(&line) {
                    Some(SseEvent::TextDelta(delta)) => tokens.push(delta),
                    Some(SseEvent::Done) => {
                        done = true;
                        break;
                    }
                    Some(_) | None => {}
                }
            }

            if done {
                break;
            }
        }

        (tokens, done)
    }

    #[test]
    fn parse_sse_line_single_clean_output_text_event() {
        let (tokens, done) = collect_tokens_from_sse_chunks(&[
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
        ]);

        assert_eq!(tokens, vec!["hello".to_string()]);
        assert!(!done);
    }

    #[test]
    fn parse_sse_line_multiple_events_in_one_chunk() {
        let (tokens, _) = collect_tokens_from_sse_chunks(&[
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}\n",
        ]);

        assert_eq!(tokens, vec!["hello".to_string(), " world".to_string()]);
    }

    #[test]
    fn parse_sse_line_partial_chunk_split_across_reads() {
        let (tokens, done) = collect_tokens_from_sse_chunks(&[
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel",
            b"lo\"}\n\ndata: [DONE]\n",
        ]);

        assert_eq!(tokens, vec!["hello".to_string()]);
        assert!(done);
    }

    #[test]
    fn parse_sse_line_done_terminates_stream() {
        let (tokens, done) = collect_tokens_from_sse_chunks(&[
            b"data: [DONE]\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n",
        ]);

        assert!(done);
        assert!(tokens.is_empty());
    }

    #[test]
    fn build_input_extracts_system_message_as_instructions() {
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
    fn build_input_converts_tool_calls_to_function_call_with_call_id() {
        let messages = vec![ChatMessage {
            role: ChatRole::Assistant,
            content: vec![MessageContent::ToolCall {
                call: ToolCall {
                    id: "tc-1".to_string(),
                    name: "execute".to_string(),
                    arguments: "{\"command\":\"echo hi\"}".to_string(),
                },
            }],
        }];
        let (_, input) = OpenAIProvider::build_input(&messages);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[0]["call_id"], "tc-1");
        assert_eq!(input[0]["name"], "execute");
    }

    #[test]
    fn build_input_converts_tool_results_to_function_call_output_with_call_id() {
        let messages = vec![ChatMessage {
            role: ChatRole::Tool,
            content: vec![MessageContent::tool_result("tc-1", "execute", "ok")],
        }];
        let (_, input) = OpenAIProvider::build_input(&messages);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], "tc-1");
        assert_eq!(input[0]["output"], "ok");
    }

    #[test]
    fn build_tools_produces_flat_format_not_nested_under_function() {
        let tools = vec![FunctionTool {
            name: "execute".to_string(),
            description: "Run a command".to_string(),
            parameters: json!({"type": "object", "properties": {"command": {"type": "string"}}}),
        }];
        let result = OpenAIProvider::build_tools(&tools);
        assert_eq!(result[0]["type"], "function");
        assert_eq!(result[0]["name"], "execute");
        assert!(result[0].get("function").is_none());
    }

    #[test]
    fn build_tools_empty_vector_returns_empty_vector() {
        let result = OpenAIProvider::build_tools(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn build_input_keeps_system_instructions_for_multi_turn_tool_roundtrip() {
        let messages = vec![
            ChatMessage::system("System directive"),
            ChatMessage::user("Run the command"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: vec![MessageContent::ToolCall {
                    call: ToolCall {
                        id: "call-1".to_string(),
                        name: "execute".to_string(),
                        arguments: "{\"command\":\"echo hello\"}".to_string(),
                    },
                }],
            },
            ChatMessage {
                role: ChatRole::Tool,
                content: vec![MessageContent::tool_result(
                    "call-1",
                    "execute",
                    "{\"stdout\":\"ok\"}",
                )],
            },
        ];

        let (instructions, input) = OpenAIProvider::build_input(&messages);
        assert_eq!(instructions, Some("System directive".to_string()));
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "Run the command");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call-1");
        assert_eq!(input[1]["name"], "execute");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["call_id"], "call-1");
        assert_eq!(input[2]["output"], "{\"stdout\":\"ok\"}");
    }
}

impl LlmProvider for OpenAIProvider {
    /// Stream a completion and parse SSE events from the OpenAI Responses API.
    ///
    /// The parser understands the minimal event set needed by the current agent:
    /// - partial assistant output
    /// - tool call argument streaming and completion
    /// - final completion signal
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
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| String::from("<failed to read response body>"));
            anyhow::bail!("API error {status}: {body}");
        }

        let stream_text = response
            .text()
            .await
            .context("failed to read streamed completion response")?;

        // Track tool calls as they arrive: call_id -> (name, args).
        let mut pending_calls: HashMap<String, (String, String)> = HashMap::new();
        let mut current_call_id: Option<String> = None;
        let mut current_call_name: Option<String> = None;
        let mut current_call_args = String::new();
        let mut stop_reason = StopReason::Stop;
        let mut assistant_content = String::new();

        // Parse SSE frame-by-frame.
        // Each line is either an empty heartbeat, comment, [DONE], or `data: {json}`.
        for raw_line in stream_text.lines() {
            match parse_sse_line(raw_line) {
                Some(SseEvent::TextDelta(delta)) => {
                    assistant_content.push_str(&delta);
                    on_token(delta);
                }
                Some(SseEvent::FunctionCallArgumentsDelta { call_id, name, delta }) => {
                    if let Some(delta) = delta {
                        current_call_args.push_str(&delta);
                    }

                    if current_call_id.is_none()
                        && let Some(id) = call_id
                    {
                        current_call_id = Some(id);
                    }
                    if current_call_name.is_none()
                        && let Some(tool_name) = name
                    {
                        current_call_name = Some(tool_name);
                    }
                }
                Some(SseEvent::FunctionCallArgumentsDone { call_id, name, arguments }) => {
                    let call_id = call_id
                        .or_else(|| current_call_id.take())
                        .unwrap_or_else(|| "unknown".to_string());
                    let name = name
                        .or_else(|| current_call_name.take())
                        .unwrap_or_else(|| "unknown".to_string());
                    let arguments = arguments.unwrap_or_else(|| std::mem::take(&mut current_call_args));

                    pending_calls.insert(call_id, (name, arguments));
                    current_call_args.clear();
                    stop_reason = StopReason::ToolCalls;
                }
                Some(SseEvent::FunctionCallOutputItemDone { call_id, name, arguments }) => {
                    pending_calls.insert(call_id, (name, arguments));
                    stop_reason = StopReason::ToolCalls;
                }
                Some(SseEvent::Completed) => {
                    if pending_calls.is_empty() {
                        stop_reason = StopReason::Stop;
                    }
                }
                Some(SseEvent::Done) => break,
                None => {}
            }
        }

        // Build tool calls from pending events.
        let tool_calls: Vec<ToolCall> = pending_calls
            .into_iter()
            .map(|(id, (name, arguments))| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();

        // Build assistant message for session history.
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
