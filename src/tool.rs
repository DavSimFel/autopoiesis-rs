//! Shell tool execution with timeout and RLIMIT resource caps.
//!
//! RLIMITs are not a security sandbox: commands still run with the current user's
//! filesystem, network, and process privileges.
//! TODO: replace RLIMIT-only containment with real sandboxing (uid drop, filesystem
//! isolation, network isolation, seccomp, or equivalent).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::io::AsyncReadExt;
use tokio::process::Child as TokioChild;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

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

#[cfg(unix)]
fn signal_process_group(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let rc = unsafe { libc::killpg(pid as libc::pid_t, signal) };
    if rc == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
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
const TERMINATION_GRACE_MS: u64 = 250;

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
            if command.trim().is_empty() {
                return Err(anyhow!("tool call requires a non-empty 'command' argument"));
            }
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
            let exit_code = match output.status.code() {
                Some(code) => code.to_string(),
                None => "signal".to_string(),
            };
            result.push_str(&exit_code);

            Ok(result)
        })
    }
}

impl Shell {
    async fn kill_with_fallback(child: &mut TokioChild, pid: Option<u32>) -> Result<()> {
        if child.id().is_none() {
            return Ok(());
        }

        #[cfg(unix)]
        if let Some(pid) = pid {
            let _ = signal_process_group(pid, libc::SIGTERM);
            match timeout(Duration::from_millis(TERMINATION_GRACE_MS), child.wait()).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    let _ = signal_process_group(pid, libc::SIGKILL);
                }
            }
        }

        #[cfg(not(unix))]
        child
            .kill()
            .await
            .context("failed to kill timed-out child process")?;

        let _ = child.wait().await;
        Ok(())
    }

    async fn run_with_timeout(command_text: &str, timeout_ms: u64) -> Result<std::process::Output> {
        let mut command = StdCommand::new("sh");
        command.arg("-lc").arg(command_text);

        #[cfg(unix)]
        unsafe {
            // SAFETY: pre_exec sets resource limits and creates a new process group
            // so that timeout kill can terminate all descendant processes.
            command.pre_exec(|| {
                // New process group so killpg reaches all descendants
                libc::setpgid(0, 0);
                set_resource_limits()
            });
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut command = TokioCommand::from(command);

        let duration = Duration::from_millis(timeout_ms);
        let mut child = command.spawn().context("failed to spawn shell command")?;
        let child_id = child.id();

        let stdout = child.stdout.take().context("failed to capture stdout")?;
        let stderr = child.stderr.take().context("failed to capture stderr")?;

        let output = match timeout(duration, async {
            let mut stdout = stdout;
            let mut stderr = stderr;

            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();

            let (status_result, stdout_result, stderr_result) = tokio::join!(
                child.wait(),
                stdout.read_to_end(&mut stdout_buf),
                stderr.read_to_end(&mut stderr_buf),
            );

            let status = status_result?;
            stdout_result?;
            stderr_result?;

            Ok::<std::process::Output, io::Error>(std::process::Output {
                status,
                stdout: stdout_buf,
                stderr: stderr_buf,
            })
        })
        .await
        {
            Ok(result) => result.map_err(|error| anyhow!(error))?,
            Err(_) => {
                Self::kill_with_fallback(&mut child, child_id).await?;
                return Err(anyhow!("tool execute timed out after {timeout_ms}ms"));
            }
        };

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

    #[tokio::test]
    async fn execute_rejects_empty_commands() {
        let tool = Shell::new();
        let error = tool
            .execute("{\"command\":\"   \"}")
            .await
            .expect_err("empty command should fail");
        assert!(error.to_string().contains("non-empty 'command' argument"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_sigkills_entire_process_group_after_grace_period() {
        let marker = std::env::temp_dir().join(format!(
            "autopoiesis_timeout_marker_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let command = format!(
            "trap '' TERM; (sleep 1; echo survived > {}) & wait",
            marker.display()
        );

        let start = Instant::now();
        let error = Shell::run_with_timeout(&command, 100)
            .await
            .expect_err("command should time out");
        assert!(error.to_string().contains("timed out"));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "timeout cleanup should not hang waiting on descendants"
        );

        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(
            !marker.exists(),
            "descendant process should not survive long enough to write marker"
        );
    }
}
