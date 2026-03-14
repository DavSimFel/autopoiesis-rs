//! Agent orchestration loop coordinating model turns and tool execution.

use std::io::{self, Write};

use anyhow::Result;
use serde_json::{from_str, Value};

use crate::gate::{GateResult, Pipeline, Severity};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall};
use crate::session::Session;
use crate::tools;
use crate::util::utc_timestamp;

pub enum TurnVerdict {
    Executed(Vec<ToolCall>),
    Denied { reason: String, gate_id: String },
    Approved { tool_calls: Vec<ToolCall> },
}

fn prompt_approval(severity: &Severity, reason: &str, command: &str) -> bool {
    let prefix = match severity {
        Severity::Low => "⚠️",
        Severity::High => "🔴",
    };

    eprintln!("\n{prefix} {reason}");
    eprintln!("  Command: {command}");
    eprint!("  Approve? [y/n]: ");
    io::stdout().flush().expect("failed to flush prompt");

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap_or(0);
    input.trim().eq_ignore_ascii_case("y")
}

fn command_from_tool_call(call: &ToolCall) -> Option<String> {
    let value = from_str::<Value>(&call.arguments).ok()?;
    value.get("command").and_then(Value::as_str).map(ToString::to_string)
}

fn append_approval_denied(session: &mut Session, reason: &str, command: &str) -> Result<()> {
    session.append(
        ChatMessage::system(format!("Tool execution rejected by user: {reason}. Command: {command}")),
        None,
    )
}

fn append_hard_deny(session: &mut Session, by: &str, reason: &str) -> Result<()> {
    session.append(ChatMessage::system(format!("Tool execution hard-denied by {by}: {reason}")), None)
}

/// Run the agent loop until the model emits a non-tool stop reason.
pub async fn run_agent_loop<F, Fut, P>(
    mut make_provider: F,
    session: &mut Session,
    user_prompt: String,
    pipeline: &mut Pipeline,
) -> Result<TurnVerdict>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    let user_prompt = format!("[{}] {}", utc_timestamp(), user_prompt);
    let tools = vec![tools::execute_tool_definition()];
    session.add_user_message(user_prompt)?;

    let mut executed: Vec<ToolCall> = Vec::new();
    let mut had_user_approval = false;

    'agent_turn: loop {
        session.ensure_context_within_limit();
        pipeline.update_context(session.history());
        let mut messages = Vec::new();
        match pipeline.run_inbound(&mut messages) {
            GateResult::Allow => {}
            GateResult::Edit => {}
            GateResult::Deny { reason, gate_id } => {
                session.append(ChatMessage::system(format!("Message hard-denied by {gate_id}: {reason}")), None)?;
                continue;
            }
            GateResult::Approve { reason, gate_id: _, severity } => {
                let command = messages
                    .iter()
                    .find_map(|message| {
                        message
                            .content
                            .iter()
                            .find_map(|block| match block {
                                MessageContent::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                    })
                    .unwrap_or("<inbound message>");
                let approved = prompt_approval(&severity, &reason, &command);
                if !approved {
                    append_approval_denied(session, &reason, &command)?;
                    continue;
                }
            }
        }

        if messages.is_empty() {
            continue;
        }

        let mut on_token = |token: String| {
            print!("{}", token);
            if let Err(err) = io::stdout().flush() {
                eprintln!("failed to flush stdout: {err}");
            }
        };

        let provider = make_provider().await?;
        let turn = provider
            .stream_completion(&messages, &tools, &mut on_token)
            .await?;
        let turn_meta = turn.meta;

        match turn.stop_reason {
            // The model produced tool calls; append assistant turn and execute each in order.
            StopReason::ToolCalls => {
                let tool_calls = turn.tool_calls.clone();
                session.append(turn.assistant_message, turn_meta)?;

                for call in &tool_calls {
                    match pipeline.check_tool_call(call) {
                        GateResult::Allow => {}
                        GateResult::Edit => {}
                        GateResult::Deny { reason, gate_id } => {
                            append_hard_deny(session, &gate_id, &reason)?;
                            continue 'agent_turn;
                        }
                        GateResult::Approve {
                            reason,
                            gate_id: _,
                            severity,
                        } => {
                            let command = command_from_tool_call(call)
                                .unwrap_or_else(|| "<command unavailable>".to_string());
                            let approved = prompt_approval(&severity, &reason, &command);
                            if !approved {
                                append_approval_denied(
                                    session,
                                    &reason,
                                    &command,
                                )?;
                                continue 'agent_turn;
                            }
                            had_user_approval = true;
                        }
                    }
                }

                match pipeline.check_tool_batch(&tool_calls) {
                    GateResult::Allow => {}
                    GateResult::Edit => {}
                    GateResult::Deny { reason, gate_id } => {
                        append_hard_deny(session, &gate_id, &reason)?;
                        continue 'agent_turn;
                    }
                    GateResult::Approve {
                        reason,
                        gate_id: _,
                        severity,
                    } => {
                        let command = tool_calls
                            .first()
                            .and_then(command_from_tool_call)
                            .unwrap_or_else(|| "<command unavailable>".to_string());
                        if !prompt_approval(&severity, &reason, &command) {
                            append_approval_denied(session, &reason, &command)?;
                            continue 'agent_turn;
                        }
                        had_user_approval = true;
                    }
                }

                for call in &tool_calls {
                    let result = match tools::execute_tool_call(call).await {
                        Ok(output) => output,
                        Err(err) => format!(r#"{{"error": "{err}"}}"#),
                    };

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result), None)?;
                    executed.push(call.clone());
                }
            }

            // Final text output is appended and execution returns to caller.
            StopReason::Stop => {
                println!();
                session.append(turn.assistant_message, turn_meta)?;
                if had_user_approval {
                    return Ok(TurnVerdict::Approved { tool_calls: executed });
                }
                return Ok(TurnVerdict::Executed(executed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::llm::FunctionTool;
    use crate::llm::StreamedTurn;

    #[derive(Clone)]
    struct InspectingProvider {
        observed_message_counts: Arc<Mutex<Vec<usize>>>,
    }

    #[allow(dead_code)]
    impl InspectingProvider {
        fn new() -> (Self, Arc<Mutex<Vec<usize>>>) {
            let observed_message_counts = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    observed_message_counts: observed_message_counts.clone(),
                },
                observed_message_counts,
            )
        }
    }

    #[allow(dead_code)]
    impl LlmProvider for InspectingProvider {
        async fn stream_completion(
            &self,
            messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.observed_message_counts
                .lock()
                .expect("observed message count mutex poisoned")
                .push(messages.len());

            Ok(StreamedTurn {
                assistant_message: ChatMessage::system("ok"),
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            })
        }
    }

    fn temp_sessions_dir(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_agent_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn trims_context_before_stream_completion_when_over_estimated_limit() {
        let dir = temp_sessions_dir("pre_call_trim");
        let (provider, observed_message_counts) = InspectingProvider::new();
        let mut session = Session::new(&dir).unwrap();
        session.set_max_context_tokens(1);

        session.add_user_message("one").unwrap();
        session.add_user_message("two").unwrap();
        session.add_user_message("three").unwrap();

        let mut pipeline = Pipeline::new();
        let _verdict = run_agent_loop(
            {
                let provider = provider.clone();
                move || {
                    let provider = provider.clone();
                    async move { Ok::<_, anyhow::Error>(provider) }
                }
            },
            &mut session,
            "new command".to_string(),
            &mut pipeline,
        )
        .await
        .unwrap();

        let observed = observed_message_counts.lock().expect("observed mutex poisoned");
        assert!(
            observed.first().cloned().is_some_and(|count| count <= 3),
            "expected pre-call trimming to run before stream completion"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
