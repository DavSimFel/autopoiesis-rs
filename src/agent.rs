use std::io::{self, Write};

use anyhow::Result;

use crate::llm::{ChatMessage, LlmProvider, StopReason};
use crate::session::Session;
use crate::tools;

pub async fn run_agent_loop<P: LlmProvider>(provider: &P, session: &mut Session, user_prompt: String) -> Result<()> {
    let tools = vec![tools::execute_tool_definition()];

    session.add_user_message(user_prompt);

    loop {
        let mut on_token = |token: String| {
            print!("{}", token);
            if let Err(err) = io::stdout().flush() {
                eprintln!("failed to flush stdout: {err}");
            }
        };

        let turn = provider
            .stream_completion(session.history(), &tools, &mut on_token)
            .await?;

        match turn.stop_reason {
            StopReason::ToolCalls => {
                session.append(turn.assistant_message);

                for call in turn.tool_calls {
                    let result = match tools::execute_tool_call(&call).await {
                        Ok(output) => output,
                        Err(err) => format!("{{\"error\": \"{err}\"}}"),
                    };

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result));
                }
            }
            _ => {
                println!();
                session.append(turn.assistant_message);
                break;
            }
        }
    }

    Ok(())
}
