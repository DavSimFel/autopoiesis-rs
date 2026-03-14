//! Agent orchestration loop coordinating model turns and tool execution.

use std::io::{self, Write};

use anyhow::Result;

use crate::llm::{ChatMessage, LlmProvider, StopReason};
use crate::session::Session;
use crate::tools;
use crate::util::utc_timestamp;

/// Run the agent loop until the model emits a non-tool stop reason.
pub async fn run_agent_loop<F, Fut, P>(
    mut make_provider: F,
    session: &mut Session,
    user_prompt: String,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    let tools = vec![tools::execute_tool_definition()];
    let stamped_prompt = format!("[{}] {}", utc_timestamp(), user_prompt);
    session.add_user_message(stamped_prompt)?;

    loop {
        let mut on_token = |token: String| {
            print!("{}", token);
            if let Err(err) = io::stdout().flush() {
                eprintln!("failed to flush stdout: {err}");
            }
        };

        let provider = make_provider().await?;
        let turn = provider
            .stream_completion(session.history(), &tools, &mut on_token)
            .await?;
        let turn_meta = turn.meta;

        match turn.stop_reason {
            // The model produced tool calls; append assistant turn and execute each in order.
            StopReason::ToolCalls => {
                session.append(turn.assistant_message, turn_meta)?;

                for call in turn.tool_calls {
                    let result = match tools::execute_tool_call(&call).await {
                        Ok(output) => output,
                        Err(err) => format!(r#"{{\"error\": \"{err}\"}}"#),
                    };

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result), None)?;
                }
            }

            // Final text output is appended and execution returns to caller.
            StopReason::Stop => {
                println!();
                session.append(turn.assistant_message, turn_meta)?;
                break;
            }
        }
    }

    Ok(())
}
