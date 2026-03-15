use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::time::timeout;
use tokio::process::Command as TokioCommand;

use crate::llm::FunctionTool;

#[cfg(unix)]
fn set_resource_limits() -> io::Result<()> {
    let limits = [
        (libc::RLIMIT_NPROC, 512u64),
        (libc::RLIMIT_FSIZE, 16 * 1024 * 1024),
        (libc::RLIMIT_CPU, 30u64),
    ];

    for (resource, value) in limits {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };

        // SAFETY: `setrlimit` is safe to call in the child process pre-exec.
        if unsafe { libc::setrlimit(resource, &limit as *const _) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn definition(&self) -> FunctionTool;
    fn execute(&self, arguments: &str) -> ToolFuture<'_>;
}

pub struct Shell;

impl Shell {
    pub fn new() -> Self {
        Self
    }

    fn parse_execute_args(raw: &str) -> Result<Value> {
        serde_json::from_str(raw).context("failed to decode tool call arguments")
    }
}

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

impl Default for Shell {
    fn default() -> Self {
        Self
    }
}

impl Tool for Shell {
    fn name(&self) -> &str {
        "execute"
    }

    fn definition(&self) -> FunctionTool {
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

    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        Box::pin(async move {
            let args = Self::parse_execute_args(&arguments)?;
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .context("tool call requires a non-empty 'command' argument")?;
            let timeout_ms = args
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS * 1000);

            // Keep command execution async and bounded by timeout.
            let output = Self::run_with_timeout(&command, timeout_ms)
                .await
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
        })
    }
}

impl Shell {
    async fn run_with_timeout(
        command_text: &str,
        timeout_ms: u64,
    ) -> Result<std::process::Output> {
        let mut command = StdCommand::new("sh");
        command.arg("-lc").arg(command_text);

        #[cfg(unix)]
        unsafe {
            // SAFETY: pre_exec is required by the request to configure per-command limits.
            command.pre_exec(set_resource_limits);
        }

        let mut command = TokioCommand::from(command);

        let duration = Duration::from_millis(timeout_ms);
        let output = timeout(duration, command.output())
            .await
            .map_err(|_| anyhow!("tool execute timed out after {timeout_ms}ms"))?;
        output.map_err(|e: std::io::Error| anyhow!(e))
    }

    fn _default_timeout_ms() -> u64 {
        DEFAULT_TIMEOUT_SECONDS * 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_tool_definition_has_execute_name() {
        let tool = Shell::new();
        let schema = tool.definition();
        assert_eq!(schema.name, "execute");
        assert_eq!(schema.parameters["type"], "object");
        let required = schema
            .parameters
            .get("required")
            .and_then(|value| value.as_array())
            .expect("schema must define required");
        assert!(required.iter().any(|value| value == "command"));

        let command_type = schema
            .parameters
            .get("properties")
            .and_then(|value| value.get("command"))
            .and_then(|value| value.get("type"))
            .expect("command property should define type");
        assert_eq!(command_type, "string");
    }

    #[test]
    fn execute_tool_definition_has_execute_name() {
        shell_tool_definition_has_execute_name();
    }
}
