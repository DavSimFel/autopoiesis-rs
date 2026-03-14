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
