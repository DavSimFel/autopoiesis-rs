use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio::time::timeout;

use crate::llm::{FunctionTool, ToolCall};

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

pub fn execute_tool_definition() -> FunctionTool {
    FunctionTool {
        name: "execute".to_string(),
        description: "Execute a shell command with optional timeout".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Command to execute with sh -lc"
                },
                "timeout_ms": {
                    "type": "number",
                    "description": "Optional timeout in milliseconds"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        }),
    }
}

pub async fn execute_tool_call(call: &ToolCall) -> Result<String> {
    let args = parse_execute_args(&call.arguments)?;
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .context("tool call requires a non-empty 'command' argument")?;

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS * 1000);

    let output = timeout(
        Duration::from_millis(timeout_ms),
        Command::new("sh").arg("-lc").arg(&command).output(),
    )
    .await
    .map_err(|_| anyhow!("tool execute timed out after {timeout_ms}ms"))?
    .context("failed to run shell command")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut result = String::new();
    result.push_str("stdout:\n");
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    result.push_str("\nstderr:\n");
    if !stderr.is_empty() {
        result.push_str(&stderr);
    }
    result.push_str("\nexit_code=");
    result.push_str(&output.status.code().unwrap_or(-1).to_string());

    Ok(result)
}

fn parse_execute_args(raw: &str) -> Result<Value> {
    serde_json::from_str(raw).context("failed to decode tool call arguments")
}
