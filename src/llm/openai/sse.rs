use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;
use tracing::trace;

use crate::llm::{StopReason, TurnMeta};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SseEvent {
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

pub(crate) fn parse_sse_line(line: &str) -> Option<SseEvent> {
    let line = line.trim();
    trace!(line_len = line.len(), "parsing sse line");
    if line.is_empty() || line.starts_with(':') {
        trace!(line_len = line.len(), "ignoring empty or comment sse line");
        return None;
    }
    if line == "data: [DONE]" {
        trace!("parsed sse done frame");
        return Some(SseEvent::Done);
    }

    let data = line.strip_prefix("data: ")?;
    trace!(data_len = data.len(), "parsed sse data frame");

    let event: Value = serde_json::from_str(data).ok()?;
    let event_type = event.get("type").and_then(|value| value.as_str())?;
    trace!(event_type = %event_type, "decoded sse event type");

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

pub(crate) fn finalize_function_call(
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

pub(crate) fn finalize_output_item(
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

pub(crate) struct SseStreamState {
    pub(crate) current_call_id: Option<String>,
    pub(crate) pending_calls: HashMap<String, (Option<String>, String)>,
    pub(crate) tool_calls: Vec<(String, String, String)>,
    pub(crate) stop_reason: StopReason,
    pub(crate) completion_meta: Option<TurnMeta>,
    pub(crate) assistant_content: String,
}

impl SseStreamState {
    pub(crate) fn new() -> Self {
        Self {
            current_call_id: None,
            pending_calls: HashMap::new(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::Stop,
            completion_meta: None,
            assistant_content: String::new(),
        }
    }
}

pub(crate) fn apply_sse_event(
    event: SseEvent,
    state: &mut SseStreamState,
    on_token: &mut (dyn FnMut(String) + Send),
) -> bool {
    match event {
        SseEvent::TextDelta(delta) => {
            state.assistant_content.push_str(&delta);
            on_token(delta);
            false
        }
        SseEvent::FunctionCallArgumentsDelta {
            call_id,
            name,
            delta,
        } => {
            let call_id = match call_id {
                Some(id) => {
                    state.current_call_id = Some(id.clone());
                    id
                }
                None => match state.current_call_id.clone() {
                    Some(id) => id,
                    None => return false,
                },
            };

            let entry = state
                .pending_calls
                .entry(call_id)
                .or_insert((None, String::new()));
            if let Some(tool_name) = name {
                entry.0 = Some(tool_name);
            }

            if let Some(delta) = delta {
                entry.1.push_str(&delta);
            }
            false
        }
        SseEvent::FunctionCallArgumentsDone {
            call_id,
            name,
            arguments,
        } => {
            let previous_len = state.tool_calls.len();
            finalize_function_call(
                &mut state.pending_calls,
                &mut state.tool_calls,
                &state.current_call_id,
                call_id,
                name,
                arguments,
            );
            if state.tool_calls.len() != previous_len {
                state.stop_reason = StopReason::ToolCalls;
            }
            false
        }
        SseEvent::FunctionCallOutputItemDone {
            call_id,
            name,
            arguments,
        } => {
            let previous_len = state.tool_calls.len();
            finalize_output_item(
                &mut state.pending_calls,
                &mut state.tool_calls,
                call_id,
                name,
                arguments,
            );
            if state.tool_calls.len() != previous_len {
                state.stop_reason = StopReason::ToolCalls;
            }
            false
        }
        SseEvent::Completed { meta } => {
            if let Some(meta) = meta {
                state.completion_meta = Some(meta);
            }

            if state.tool_calls.is_empty() {
                state.stop_reason = StopReason::Stop;
            }
            false
        }
        SseEvent::Done => true,
    }
}

pub(crate) fn require_terminal_sse_event(terminal_seen: bool) -> Result<()> {
    if terminal_seen {
        Ok(())
    } else {
        anyhow::bail!("stream ended before terminal SSE event was received");
    }
}

pub(crate) fn note_terminal_sse_event(event: &SseEvent, terminal_seen: &mut bool) {
    if matches!(event, SseEvent::Done | SseEvent::Completed { .. }) {
        *terminal_seen = true;
    }
}
