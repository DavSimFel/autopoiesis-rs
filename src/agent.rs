//! Agent orchestration loop coordinating model turns and tool execution.

use std::io::{self, Write};

use anyhow::Result;

use crate::gate::{GateResult, Pipeline};
use crate::llm::{ChatMessage, LlmProvider, StopReason, ToolCall};
use crate::session::Session;
use crate::tools;
use crate::util::utc_timestamp;

pub enum TurnVerdict {
    ExecuteAll(Vec<ToolCall>),
    Blocked { reason: String, blocked_by: String },
    RequestApproval { prompt: String, pending: Vec<ToolCall> },
}

/// Run the agent loop until the model emits a non-tool stop reason.
pub async fn run_agent_loop<F, Fut, P>(
    mut make_provider: F,
    session: &mut Session,
    user_prompt: String,
    pipeline: &Pipeline,
) -> Result<TurnVerdict>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    let tools = vec![tools::execute_tool_definition()];
    let stamped_prompt = format!("[{}] {}", utc_timestamp(), user_prompt);
    session.add_user_message(stamped_prompt)?;

    let mut executed: Vec<ToolCall> = Vec::new();

    loop {
        session.ensure_context_within_limit();
        let mut messages = session.history().to_vec();
        match pipeline.run_inbound(&mut messages) {
            GateResult::Allow => {}
            GateResult::Edit => {}
            GateResult::Block { reason, gate_id } => {
                session.append(ChatMessage::system(format!("Message blocked by {gate_id}: {reason}")), None)?;
                return Ok(TurnVerdict::Blocked {
                    reason,
                    blocked_by: gate_id,
                });
            }
            GateResult::Request { prompt, gate_id: _ } => {
                session.append(ChatMessage::system(format!("Input request from validator: {prompt}")), None)?;
                return Ok(TurnVerdict::RequestApproval {
                    prompt,
                    pending: Vec::new(),
                });
            }
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
                        GateResult::Block { reason, gate_id } => {
                            session.append(
                                ChatMessage::system(format!("Tool call blocked by {gate_id}: {reason}")),
                                None,
                            )?;
                            return Ok(TurnVerdict::Blocked {
                                reason,
                                blocked_by: gate_id,
                            });
                        }
                        GateResult::Request { prompt, gate_id: _ } => {
                            session.append(
                                ChatMessage::system(format!("Tool call request from validator: {prompt}")),
                                None,
                            )?;
                            return Ok(TurnVerdict::RequestApproval {
                                prompt,
                                pending: tool_calls,
                            });
                        }
                    }
                }

                match pipeline.check_tool_batch(&tool_calls) {
                    GateResult::Allow => {}
                    GateResult::Edit => {}
                    GateResult::Block { reason, gate_id } => {
                        session.append(
                            ChatMessage::system(format!("Tool batch blocked by {gate_id}: {reason}")),
                            None,
                        )?;
                        return Ok(TurnVerdict::Blocked {
                            reason,
                            blocked_by: gate_id,
                        });
                    }
                    GateResult::Request { prompt, gate_id: _ } => {
                        session.append(
                            ChatMessage::system(format!("Tool batch request from validator: {prompt}")),
                            None,
                        )?;
                        return Ok(TurnVerdict::RequestApproval {
                            prompt,
                            pending: tool_calls,
                        });
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
                return Ok(TurnVerdict::ExecuteAll(executed));
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
        let mut session = Session::new("system", &dir).unwrap();
        session.set_max_context_tokens(1);

        session.add_user_message("one").unwrap();
        session.add_user_message("two").unwrap();
        session.add_user_message("three").unwrap();

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
            &Pipeline::new(),
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
