//! Agent orchestration loop coordinating model turns and tool execution.

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::llm::{ChatMessage, LlmProvider, StopReason};
use crate::session::Session;
use crate::tools;

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
    session.add_user_message(stamped_prompt);

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

        match turn.stop_reason {
            // The model produced tool calls; append assistant turn and execute each in order.
            StopReason::ToolCalls => {
                session.append(turn.assistant_message);

                for call in turn.tool_calls {
                    let result = match tools::execute_tool_call(&call).await {
                        Ok(output) => output,
                        Err(err) => format!(r#"{{\"error\": \"{err}\"}}"#),
                    };

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result));
                }
            }

            // Final text output is appended and execution returns to caller.
            StopReason::Stop => {
                println!();
                session.append(turn.assistant_message);
                break;
            }
        }
    }

    Ok(())
}

fn utc_timestamp() -> String {
    const SECS_PER_MINUTE: i64 = 60;
    const SECS_PER_HOUR: i64 = 3_600;
    const SECS_PER_DAY: i64 = 86_400;

    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    let mut days = duration / SECS_PER_DAY;
    let mut rem = duration % SECS_PER_DAY;

    let hour = rem / SECS_PER_HOUR;
    rem %= SECS_PER_HOUR;
    let minute = rem / SECS_PER_MINUTE;
    let second = rem % SECS_PER_MINUTE;

    // Days since Unix epoch -> civil date.
    days += 719_468;
    let era = if days >= 0 { days / 146_097 } else { (days - 146_096) / 146_097 };
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + (if month <= 2 { 1 } else { 0 });

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        hour,
        minute,
        second
    )
}
