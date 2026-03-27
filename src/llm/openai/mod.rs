//! OpenAI Responses API client for streaming completions and tool calls.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::trace;

use crate::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StreamedTurn, ToolCall,
};
use crate::principal::Principal;

mod request;
mod sse;

#[cfg(test)]
pub(crate) use sse::SseEvent;
pub(crate) use sse::{
    SseStreamState, apply_sse_event, note_terminal_sse_event, parse_sse_line,
    require_terminal_sse_event,
};
#[cfg(test)]
pub(crate) use sse::{finalize_function_call, finalize_output_item};

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

    fn build_input(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        request::build_input(messages)
    }

    fn build_tools(tools: &[FunctionTool]) -> Vec<Value> {
        request::build_tools(tools)
    }
}

impl LlmProvider for OpenAIProvider {
    /// Stream a completion and parse SSE events from the OpenAI Responses API.
    ///
    /// The parser understands the minimal event set needed by the current agent:
    /// - partial assistant output
    /// - tool call argument streaming and completion
    /// - final completion signal
    fn stream_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [FunctionTool],
        on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move {
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

            let mut state = SseStreamState::new();
            let mut stream_buffer = String::new();
            let mut done = false;
            let mut terminal_seen = false;

            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.context("failed to read streamed completion response chunk")?;
                let chunk = String::from_utf8_lossy(&chunk);
                trace!(chunk_len = chunk.len(), "received sse chunk");
                stream_buffer.push_str(&chunk);

                while let Some(line_end) = stream_buffer.find('\n') {
                    let raw_line = stream_buffer[..line_end].to_string();
                    stream_buffer.drain(..line_end + 1);
                    trace!(line_len = raw_line.len(), "parsing sse line from stream");
                    if let Some(event) = parse_sse_line(&raw_line)
                        && {
                            note_terminal_sse_event(&event, &mut terminal_seen);
                            apply_sse_event(event, &mut state, on_token)
                        }
                    {
                        done = true;
                        break;
                    }
                }

                if done {
                    break;
                }
            }

            if !stream_buffer.is_empty()
                && let Some(event) = parse_sse_line(stream_buffer.trim_end())
            {
                trace!(
                    buffer_len = stream_buffer.len(),
                    "parsing trailing sse buffer"
                );
                note_terminal_sse_event(&event, &mut terminal_seen);
                let _ = apply_sse_event(event, &mut state, on_token);
            }

            require_terminal_sse_event(terminal_seen)?;

            let tool_calls: Vec<ToolCall> = state
                .tool_calls
                .into_iter()
                .map(|(id, name, arguments)| ToolCall {
                    id,
                    name,
                    arguments,
                })
                .collect();

            let mut assistant_msg =
                ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
            if !state.assistant_content.is_empty() {
                assistant_msg.content.push(MessageContent::Text {
                    text: state.assistant_content,
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
                meta: state.completion_meta,
                stop_reason: state.stop_reason,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::llm::{ChatMessage, ChatRole, MessageContent, StopReason, ToolCall, TurnMeta};
    use crate::principal::Principal;

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

    fn collect_stream_from_sse_chunks(
        chunks: &[&[u8]],
    ) -> (
        Vec<String>,
        Vec<ToolCall>,
        Option<TurnMeta>,
        StopReason,
        bool,
    ) {
        let mut buffer = String::new();
        let mut tokens = Vec::new();
        let mut state = SseStreamState::new();
        let mut done = false;
        let mut token_sink = |delta: String| tokens.push(delta);

        for chunk in chunks {
            let chunk = std::str::from_utf8(chunk).expect("mock SSE chunks should be valid utf-8");
            buffer.push_str(chunk);

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].to_string();
                buffer.drain(..line_end + 1);

                if let Some(event) = parse_sse_line(&line)
                    && apply_sse_event(event, &mut state, &mut token_sink)
                {
                    done = true;
                    break;
                }
            }

            if done {
                break;
            }
        }

        if !buffer.is_empty()
            && let Some(event) = parse_sse_line(buffer.trim_end())
        {
            let _ = apply_sse_event(event, &mut state, &mut token_sink);
        }

        let tool_calls = state
            .tool_calls
            .into_iter()
            .map(|(id, name, arguments)| ToolCall {
                id,
                name,
                arguments,
            })
            .collect();

        (
            tokens,
            tool_calls,
            state.completion_meta,
            state.stop_reason,
            done,
        )
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
    fn stream_completion_requires_terminal_sse_event() {
        assert!(require_terminal_sse_event(true).is_ok());
        assert!(require_terminal_sse_event(false).is_err());
    }

    #[test]
    fn note_terminal_sse_event_marks_terminal_events() {
        let mut terminal_seen = false;
        note_terminal_sse_event(&SseEvent::Done, &mut terminal_seen);
        assert!(terminal_seen);

        terminal_seen = false;
        note_terminal_sse_event(&SseEvent::Completed { meta: None }, &mut terminal_seen);
        assert!(terminal_seen);
    }

    #[test]
    fn collect_stream_from_sse_chunks_without_terminal_event_is_rejected() {
        let (tokens, tool_calls, meta, stop_reason, done) = collect_stream_from_sse_chunks(&[
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n",
            b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n",
        ]);

        assert_eq!(tokens.concat(), "hello");
        assert!(tool_calls.is_empty());
        assert!(meta.is_none());
        assert_eq!(stop_reason, StopReason::Stop);
        assert!(!done);
        assert!(require_terminal_sse_event(done).is_err());
    }

    #[test]
    fn parse_sse_line_trailing_function_call_output_item_without_newline_is_parsed() {
        let (_, tool_calls, meta, stop_reason, done) = collect_stream_from_sse_chunks(&[
            b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"execute\",\"arguments\":\"{\\\"command\\\":\\\"echo hi\\\"}\"}}",
        ]);

        assert!(!done);
        assert_eq!(stop_reason, StopReason::ToolCalls);
        assert!(meta.is_none());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "execute");
        assert_eq!(tool_calls[0].arguments, "{\"command\":\"echo hi\"}");
    }

    #[test]
    fn parse_sse_line_trailing_completed_without_newline_retains_metadata() {
        let (_, tool_calls, meta, stop_reason, done) = collect_stream_from_sse_chunks(&[
            b"data: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-5.3-codex-spark\",\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"reasoning_tokens\":25}}}",
        ]);

        assert!(!done);
        assert_eq!(stop_reason, StopReason::Stop);
        assert!(tool_calls.is_empty());
        let meta = meta.expect("completed event should include metadata");
        assert_eq!(meta.model, Some("gpt-5.3-codex-spark".to_string()));
        assert_eq!(meta.input_tokens, Some(100));
        assert_eq!(meta.output_tokens, Some(50));
        assert_eq!(meta.reasoning_tokens, Some(25));
    }

    #[tokio::test]
    async fn stream_completion_requires_terminal_sse_event_through_http() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|| async {
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("cache-control", "no-cache"),
                    ],
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hel\"}\n\n\
                     data: {\"type\":\"response.output_text.delta\",\"delta\":\"lo\"}\n\n",
                )
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let provider = OpenAIProvider::new(
            "mock-api-key",
            format!("http://{}", addr),
            "gpt-4o-mini",
            None,
        );
        let messages = vec![ChatMessage::user("hello")];
        let mut on_token = |_| {};
        let err = provider
            .stream_completion(&messages, &[], &mut on_token)
            .await
            .expect_err("missing terminal SSE event must fail");

        assert!(err.to_string().contains("terminal"));
        server.abort();
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
    fn build_input_preserves_multiple_text_blocks_in_the_first_system_message() {
        let mut system = ChatMessage::system("Primary system instructions");
        system.content.push(MessageContent::text(
            "Available skills: code-review (Reviews code changes)",
        ));
        let messages = vec![system, ChatMessage::user("Hello")];
        let (instructions, input) = OpenAIProvider::build_input(&messages);

        assert_eq!(
            instructions,
            Some(
                "Primary system instructions\nAvailable skills: code-review (Reviews code changes)"
                    .to_string()
            )
        );
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "Hello");
    }

    #[test]
    fn build_input_keeps_audit_notes_out_of_system_replay_path() {
        let messages = vec![
            ChatMessage::system("Primary system instructions"),
            ChatMessage::user("Hello"),
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::System)),
        ];
        let mut messages = messages;
        messages[2].content.push(MessageContent::text(
            "Tool execution rejected after approval by shell-policy",
        ));

        let (instructions, input) = OpenAIProvider::build_input(&messages);

        assert_eq!(
            instructions,
            Some("Primary system instructions".to_string())
        );
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(
            input[1]["content"],
            "Tool execution rejected after approval by shell-policy"
        );
    }

    #[test]
    fn build_input_converts_tool_calls_to_function_call_with_call_id() {
        let messages = vec![ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
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
            principal: Principal::System,
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
                principal: Principal::Agent,
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
                principal: Principal::System,
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

    #[test]
    fn build_input_handles_denied_tool_call_placeholder_roundtrip() {
        let messages = vec![
            ChatMessage::user("deny tool call"),
            ChatMessage {
                role: ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::Text {
                    text: String::new(),
                }],
            },
            ChatMessage {
                role: ChatRole::Assistant,
                principal: Principal::System,
                content: vec![MessageContent::text(
                    "Tool execution rejected after approval by shell-policy",
                )],
            },
        ];

        let (instructions, input) = OpenAIProvider::build_input(&messages);
        assert_eq!(instructions, None);
        assert_eq!(input.len(), 3);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"], "deny tool call");
        assert_eq!(input[1]["role"], "assistant");
        assert_eq!(input[1]["content"], "");
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(
            input[2]["content"],
            "Tool execution rejected after approval by shell-policy"
        );
    }
}
