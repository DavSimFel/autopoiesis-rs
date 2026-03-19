//! OpenAI Responses API client for streaming completions and tool calls.

use std::collections::HashMap;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
// serde is used by tests and by request/response payload assembly.
use serde_json::{Value, json};

use crate::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StopReason, StreamedTurn,
    ToolCall, TurnMeta,
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

    pub fn with_client(
        client: Client,
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
            client,
        }
    }

    /// Extract the system prompt and convert prior messages to the responses API item format.
    fn build_input(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
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
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Completed {
        meta: Option<TurnMeta>,
    },
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

    let data = line.strip_prefix("data: ")?;

    let event: Value = serde_json::from_str(data).ok()?;
    let event_type = event.get("type").and_then(|value| value.as_str())?;

    match event_type {
        "response.output_text.delta" => event
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| SseEvent::TextDelta(delta.to_string())),
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
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                arguments: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            })
        }
        "response.completed" => {
            let response = event.get("response").unwrap_or(&event);
            let usage = response.get("usage").or_else(|| event.get("usage"));

            let meta = TurnMeta {
                model: response
                    .get("model")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                input_tokens: usage
                    .and_then(|usage| usage.get("input_tokens").and_then(Value::as_u64)),
                output_tokens: usage
                    .and_then(|usage| usage.get("output_tokens").and_then(Value::as_u64)),
                reasoning_tokens: usage
                    .and_then(|usage| usage.get("reasoning_tokens").and_then(Value::as_u64)),
                reasoning_trace: None,
            };

            let has_meta = meta.model.is_some()
                || meta.input_tokens.is_some()
                || meta.output_tokens.is_some()
                || meta.reasoning_tokens.is_some();

            Some(SseEvent::Completed {
                meta: if has_meta { Some(meta) } else { None },
            })
        }
        _ => None,
    }
}

fn upsert_tool_call(
    tool_calls: &mut Vec<(String, String, String)>,
    call_id: String,
    name: String,
    arguments: String,
) {
    if let Some((_, existing_name, existing_args)) =
        tool_calls.iter_mut().find(|(id, _, _)| id == &call_id)
    {
        *existing_name = name;
        *existing_args = arguments;
    } else {
        tool_calls.push((call_id, name, arguments));
    }
}

fn finalize_function_call(
    pending_calls: &mut HashMap<String, (Option<String>, String)>,
    tool_calls: &mut Vec<(String, String, String)>,
    current_call_id: &Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) {
    let call_id = match call_id.or_else(|| current_call_id.clone()) {
        Some(id) => id,
        None => return,
    };
    let mut entry = pending_calls
        .remove(&call_id)
        .unwrap_or((None, String::new()));
    if let Some(tool_name) = name {
        entry.0 = Some(tool_name);
    }
    let Some(tool_name) = entry.0 else {
        return;
    };
    let arguments = arguments.unwrap_or(entry.1);

    upsert_tool_call(tool_calls, call_id, tool_name, arguments);
}

fn finalize_output_item(
    pending_calls: &mut HashMap<String, (Option<String>, String)>,
    tool_calls: &mut Vec<(String, String, String)>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
) {
    let Some(call_id) = call_id else {
        return;
    };
    let mut entry = pending_calls
        .remove(&call_id)
        .unwrap_or((None, String::new()));
    if let Some(tool_name) = name {
        entry.0 = Some(tool_name);
    }
    let Some(tool_name) = entry.0 else {
        return;
    };
    let arguments = arguments.unwrap_or(entry.1);

    upsert_tool_call(tool_calls, call_id, tool_name, arguments);
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

        let mut tool_calls: Vec<(String, String, String)> = Vec::new();
        let mut pending_calls: HashMap<String, (Option<String>, String)> = HashMap::new();
        let mut current_call_id: Option<String> = None;
        let mut stop_reason = StopReason::Stop;
        let mut completion_meta = None;
        let mut assistant_content = String::new();
        let mut stream_buffer = String::new();
        let mut done = false;

        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("failed to read streamed completion response chunk")?;
            let chunk = String::from_utf8_lossy(&chunk);
            stream_buffer.push_str(&chunk);

            while let Some(line_end) = stream_buffer.find('\n') {
                let raw_line = stream_buffer[..line_end].to_string();
                stream_buffer.drain(..line_end + 1);
                match parse_sse_line(&raw_line) {
                    Some(SseEvent::TextDelta(delta)) => {
                        assistant_content.push_str(&delta);
                        on_token(delta);
                    }
                    Some(SseEvent::FunctionCallArgumentsDelta {
                        call_id,
                        name,
                        delta,
                    }) => {
                        let call_id = match call_id {
                            Some(id) => {
                                current_call_id = Some(id.clone());
                                id
                            }
                            None => match current_call_id.clone() {
                                Some(id) => id,
                                None => continue,
                            },
                        };

                        let entry = pending_calls
                            .entry(call_id)
                            .or_insert((None, String::new()));
                        if let Some(tool_name) = name {
                            entry.0 = Some(tool_name);
                        }

                        if let Some(delta) = delta {
                            entry.1.push_str(&delta);
                        }
                    }
                    Some(SseEvent::FunctionCallArgumentsDone {
                        call_id,
                        name,
                        arguments,
                    }) => {
                        let previous_len = tool_calls.len();
                        finalize_function_call(
                            &mut pending_calls,
                            &mut tool_calls,
                            &current_call_id,
                            call_id,
                            name,
                            arguments,
                        );
                        if tool_calls.len() != previous_len {
                            stop_reason = StopReason::ToolCalls;
                        }
                    }
                    Some(SseEvent::FunctionCallOutputItemDone {
                        call_id,
                        name,
                        arguments,
                    }) => {
                        let previous_len = tool_calls.len();
                        finalize_output_item(
                            &mut pending_calls,
                            &mut tool_calls,
                            call_id,
                            name,
                            arguments,
                        );
                        if tool_calls.len() != previous_len {
                            stop_reason = StopReason::ToolCalls;
                        }
                    }
                    Some(SseEvent::Completed { meta }) => {
                        if let Some(meta) = meta {
                            completion_meta = Some(meta);
                        }

                        if tool_calls.is_empty() {
                            stop_reason = StopReason::Stop;
                        }
                    }
                    Some(SseEvent::Done) => {
                        done = true;
                        break;
                    }
                    None => {}
                }
            }

            if done {
                break;
            }
        }

        if !stream_buffer.is_empty()
            && let Some(event) = parse_sse_line(stream_buffer.trim_end())
        {
            match event {
                SseEvent::TextDelta(delta) => {
                    assistant_content.push_str(&delta);
                    on_token(delta);
                }
                SseEvent::FunctionCallArgumentsDelta { .. }
                | SseEvent::FunctionCallArgumentsDone { .. }
                | SseEvent::FunctionCallOutputItemDone { .. }
                | SseEvent::Completed { .. }
                | SseEvent::Done => {}
            }
        }

        let tool_calls: Vec<ToolCall> = tool_calls
            .into_iter()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();

        let mut assistant_msg = ChatMessage::with_role(ChatRole::Assistant);
        if !assistant_content.is_empty() {
            assistant_msg.content.push(MessageContent::Text {
                text: assistant_content,
            });
        }
        for tc in &tool_calls {
            assistant_msg
                .content
                .push(MessageContent::ToolCall { call: tc.clone() });
        }

        Ok(StreamedTurn {
            assistant_message: assistant_msg,
            tool_calls,
            meta: completion_meta,
            stop_reason,
        })
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

            while let Some(line_end) = buffer.find('\n') {
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

    fn collect_tool_calls_from_events(events: Vec<SseEvent>) -> Vec<ToolCall> {
        let mut current_call_id = None;
        let mut pending_calls: HashMap<String, (Option<String>, String)> = HashMap::new();
        let mut tool_calls: Vec<(String, String, String)> = Vec::new();

        for event in events {
            match event {
                SseEvent::FunctionCallArgumentsDelta {
                    call_id,
                    name,
                    delta,
                } => {
                    let call_id = match call_id {
                        Some(id) => {
                            current_call_id = Some(id.clone());
                            id
                        }
                        None => match current_call_id.clone() {
                            Some(id) => id,
                            None => continue,
                        },
                    };

                    let entry = pending_calls
                        .entry(call_id)
                        .or_insert((None, String::new()));
                    if let Some(tool_name) = name {
                        entry.0 = Some(tool_name);
                    }

                    if let Some(delta) = delta {
                        entry.1.push_str(&delta);
                    }
                }
                SseEvent::FunctionCallArgumentsDone {
                    call_id,
                    name,
                    arguments,
                } => {
                    finalize_function_call(
                        &mut pending_calls,
                        &mut tool_calls,
                        &current_call_id,
                        call_id,
                        name,
                        arguments,
                    );
                }
                SseEvent::FunctionCallOutputItemDone {
                    call_id,
                    name,
                    arguments,
                } => {
                    finalize_output_item(
                        &mut pending_calls,
                        &mut tool_calls,
                        call_id,
                        name,
                        arguments,
                    );
                }
                _ => {}
            }
        }

        tool_calls
            .into_iter()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect()
    }

    fn sse_event(line: &str) -> SseEvent {
        parse_sse_line(line).expect("valid SSE test event")
    }

    #[test]
    fn function_call_order_is_preserved() {
        let events = vec![
            sse_event(
                "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_1\",\"name\":\"first\",\"arguments\":\"{\\\"a\\\":1}\"}",
            ),
            sse_event(
                "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_2\",\"name\":\"second\",\"arguments\":\"{\\\"b\\\":2}\"}",
            ),
            sse_event(
                "data: {\"type\":\"response.function_call_arguments.done\",\"call_id\":\"call_3\",\"name\":\"third\",\"arguments\":\"{\\\"c\\\":3}\"}",
            ),
        ];
        let calls = collect_tool_calls_from_events(events);

        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "first");
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].name, "second");
        assert_eq!(calls[2].id, "call_3");
        assert_eq!(calls[2].name, "third");
    }

    #[test]
    fn function_call_without_identifiable_id_is_dropped() {
        let events = vec![SseEvent::FunctionCallArgumentsDone {
            call_id: None,
            name: Some("shell".to_string()),
            arguments: Some("{\"command\":\"ls\"}".to_string()),
        }];

        let calls = collect_tool_calls_from_events(events);

        assert!(calls.is_empty());
    }

    #[test]
    fn function_call_without_identifiable_name_is_dropped() {
        let events = vec![SseEvent::FunctionCallArgumentsDone {
            call_id: Some("call_1".to_string()),
            name: None,
            arguments: Some("{\"command\":\"ls\"}".to_string()),
        }];

        let calls = collect_tool_calls_from_events(events);

        assert!(calls.is_empty());
    }

    #[test]
    fn output_item_without_identifiable_id_is_dropped() {
        let events = vec![SseEvent::FunctionCallOutputItemDone {
            call_id: None,
            name: Some("shell".to_string()),
            arguments: Some("{\"command\":\"ls\"}".to_string()),
        }];

        let calls = collect_tool_calls_from_events(events);

        assert!(calls.is_empty());
    }

    #[test]
    fn function_calls_interleaved_deltas_are_kept_isolated() {
        let events = vec![
            SseEvent::FunctionCallArgumentsDelta {
                call_id: Some("call_1".to_string()),
                name: Some("first".to_string()),
                delta: Some("{\"a\":".to_string()),
            },
            SseEvent::FunctionCallArgumentsDelta {
                call_id: Some("call_2".to_string()),
                name: Some("second".to_string()),
                delta: Some("{\"b\":".to_string()),
            },
            SseEvent::FunctionCallArgumentsDelta {
                call_id: Some("call_1".to_string()),
                name: None,
                delta: Some("1}".to_string()),
            },
            SseEvent::FunctionCallArgumentsDone {
                call_id: Some("call_1".to_string()),
                name: None,
                arguments: None,
            },
            SseEvent::FunctionCallArgumentsDone {
                call_id: Some("call_2".to_string()),
                name: None,
                arguments: Some("{\"b\":2}".to_string()),
            },
        ];

        let calls = collect_tool_calls_from_events(events);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "first");
        assert_eq!(calls[0].arguments, "{\"a\":1}");
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].name, "second");
        assert_eq!(calls[1].arguments, "{\"b\":2}");
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
    fn parse_sse_line_response_completed_contains_usage_metadata() {
        let line = "data: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-5.3-codex-spark\",\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"reasoning_tokens\":25}}}";
        let event = parse_sse_line(line).expect("completed event should parse");

        match event {
            SseEvent::Completed { meta } => {
                let meta = meta.expect("completed event should include metadata");
                assert_eq!(meta.model, Some("gpt-5.3-codex-spark".to_string()));
                assert_eq!(meta.input_tokens, Some(100));
                assert_eq!(meta.output_tokens, Some(50));
                assert_eq!(meta.reasoning_tokens, Some(25));
            }
            _ => panic!("expected completed event"),
        }
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
    fn build_input_preserves_first_system_message_and_replays_later_system_messages() {
        let messages = vec![
            ChatMessage::system("Primary system instructions"),
            ChatMessage::user("Hello"),
            ChatMessage::system("Tool execution rejected by user"),
        ];
        let (instructions, input) = OpenAIProvider::build_input(&messages);

        assert_eq!(
            instructions,
            Some("Primary system instructions".to_string())
        );
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "Hello");
        assert_eq!(input[1]["role"], "system");
        assert_eq!(input[1]["content"], "Tool execution rejected by user");
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
