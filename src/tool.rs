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
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child as TokioChild;
use tokio::process::Command as TokioCommand;
use tokio::sync::watch;
use tokio::time::{Instant, timeout};

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

pub(crate) const SHELL_OUTPUT_TRUNCATED_PREFIX: &str = "output_truncated=";

#[derive(Debug, Clone, Copy)]
pub struct Shell {
    max_output_bytes: usize,
}

#[derive(Debug)]
struct ShellOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct DrainResult {
    captured: Vec<u8>,
    truncated: bool,
}

#[derive(Debug)]
struct CaptureBudget {
    remaining: usize,
}

impl Shell {
    pub fn new() -> Self {
        Self::with_max_output_bytes(crate::config::DEFAULT_SHELL_MAX_OUTPUT_BYTES)
    }

    pub fn with_max_output_bytes(max_output_bytes: usize) -> Self {
        Self { max_output_bytes }
    }

    fn parse_execute_args(raw: &str) -> Result<Value> {
        serde_json::from_str(raw).context("failed to decode tool call arguments")
    }
}

const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
const TERMINATION_GRACE_MS: u64 = 250;
const POST_CAP_DRAIN_TIMEOUT_MS: u64 = 1_000;
const DRAIN_CHUNK_SIZE: usize = 8 * 1024;

pub(crate) fn shell_output_truncation_note(max_output_bytes: usize) -> String {
    format!(
        "{SHELL_OUTPUT_TRUNCATED_PREFIX}combined stdout/stderr capture limited to {max_output_bytes} bytes; remaining output discarded"
    )
}

pub(crate) fn shell_output_was_truncated(output: &str) -> bool {
    output
        .lines()
        .any(|line| line.starts_with(SHELL_OUTPUT_TRUNCATED_PREFIX))
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
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
        let max_output_bytes = self.max_output_bytes;
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
            let output = Self::run_with_timeout(&command, timeout_ms, max_output_bytes)
                .await
                .context("failed to run shell command")?;

            Ok(Self::format_output(&output, max_output_bytes))
        })
    }
}

impl Shell {
    fn format_output(output: &ShellOutput, max_output_bytes: usize) -> String {
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
        if output.truncated {
            result.push('\n');
            result.push_str(&shell_output_truncation_note(max_output_bytes));
        }

        result
    }

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

    fn observe_cap_hit(
        cap_hit_rx: &watch::Receiver<Option<Instant>>,
        effective_deadline: &mut Instant,
        cap_hit_observed: &mut bool,
    ) {
        if *cap_hit_observed {
            return;
        }

        if let Some(cap_hit_at) = *cap_hit_rx.borrow() {
            *effective_deadline = (*effective_deadline)
                .min(cap_hit_at + Duration::from_millis(POST_CAP_DRAIN_TIMEOUT_MS));
            *cap_hit_observed = true;
        }
    }

    fn timeout_error(
        timeout_ms: u64,
        started_at: Instant,
        effective_deadline: Instant,
    ) -> anyhow::Error {
        let effective_timeout_ms =
            u64::try_from(effective_deadline.duration_since(started_at).as_millis())
                .unwrap_or(u64::MAX);
        if effective_timeout_ms < timeout_ms {
            anyhow!(
                "tool execute timed out after {effective_timeout_ms}ms while draining bounded shell output"
            )
        } else {
            anyhow!("tool execute timed out after {timeout_ms}ms")
        }
    }

    async fn drain_stream<R>(
        mut reader: R,
        budget: Arc<Mutex<CaptureBudget>>,
        cap_hit_sent: Arc<AtomicBool>,
        cap_hit_tx: watch::Sender<Option<Instant>>,
    ) -> io::Result<DrainResult>
    where
        R: AsyncRead + Unpin,
    {
        let mut captured = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; DRAIN_CHUNK_SIZE];

        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                break;
            }

            let keep = {
                let mut state = budget.lock().unwrap_or_else(|error| error.into_inner());
                let keep = state.remaining.min(read);
                state.remaining -= keep;
                keep
            };

            if keep > 0 {
                captured.extend_from_slice(&chunk[..keep]);
            }

            if keep < read {
                truncated = true;
                if !cap_hit_sent.swap(true, Ordering::SeqCst) {
                    let _ = cap_hit_tx.send(Some(Instant::now()));
                }
            }
        }

        Ok(DrainResult {
            captured,
            truncated,
        })
    }

    async fn run_with_timeout(
        command_text: &str,
        timeout_ms: u64,
        max_output_bytes: usize,
    ) -> Result<ShellOutput> {
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
        let started_at = Instant::now();
        let original_deadline = started_at + Duration::from_millis(timeout_ms);
        let mut effective_deadline = original_deadline;
        let mut child = command.spawn().context("failed to spawn shell command")?;
        let child_id = child.id();

        let stdout = child.stdout.take().context("failed to capture stdout")?;
        let stderr = child.stderr.take().context("failed to capture stderr")?;

        let budget = Arc::new(Mutex::new(CaptureBudget {
            remaining: max_output_bytes,
        }));
        let cap_hit_sent = Arc::new(AtomicBool::new(false));
        let (cap_hit_tx, mut cap_hit_rx) = watch::channel(None::<Instant>);
        let mut stdout_task = tokio::spawn(Self::drain_stream(
            stdout,
            budget.clone(),
            cap_hit_sent.clone(),
            cap_hit_tx.clone(),
        ));
        let mut stderr_task =
            tokio::spawn(Self::drain_stream(stderr, budget, cap_hit_sent, cap_hit_tx));

        let mut status = None;
        let mut stdout_result = None;
        let mut stderr_result = None;
        let mut cap_hit_observed = false;

        loop {
            Self::observe_cap_hit(&cap_hit_rx, &mut effective_deadline, &mut cap_hit_observed);

            if status.is_some() && stdout_result.is_some() && stderr_result.is_some() {
                break;
            }

            let sleep = tokio::time::sleep_until(effective_deadline);
            tokio::pin!(sleep);

            tokio::select! {
                result = &mut stdout_task, if stdout_result.is_none() => {
                    stdout_result = Some(result.context("stdout drain task failed")??);
                }
                result = &mut stderr_task, if stderr_result.is_none() => {
                    stderr_result = Some(result.context("stderr drain task failed")??);
                }
                result = child.wait(), if status.is_none() => {
                    status = Some(result.context("failed waiting for shell command")?);
                }
                changed = cap_hit_rx.changed(), if !cap_hit_observed => {
                    if changed.is_ok() {
                        Self::observe_cap_hit(&cap_hit_rx, &mut effective_deadline, &mut cap_hit_observed);
                    } else {
                        cap_hit_observed = true;
                    }
                }
                _ = &mut sleep => {
                    Self::kill_with_fallback(&mut child, child_id).await?;
                    if stdout_result.is_none() {
                        stdout_task.abort();
                        let _ = stdout_task.await;
                    }
                    if stderr_result.is_none() {
                        stderr_task.abort();
                        let _ = stderr_task.await;
                    }
                    return Err(Self::timeout_error(timeout_ms, started_at, effective_deadline));
                }
            }
        }

        let stdout_result = stdout_result.context("stdout drain did not complete")?;
        let stderr_result = stderr_result.context("stderr drain did not complete")?;
        let status = status.context("shell command did not produce an exit status")?;

        Ok(ShellOutput {
            status,
            stdout: stdout_result.captured,
            stderr: stderr_result.captured,
            truncated: stdout_result.truncated || stderr_result.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn temp_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_tool_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

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
    async fn small_output_preserves_stdout_stderr_and_exit_code_contract() {
        let max_output_bytes = 1_024;
        let output = Shell::run_with_timeout(
            "printf 'hello'; printf 'warn' 1>&2",
            1_000,
            max_output_bytes,
        )
        .await
        .expect("small command should succeed");

        assert_eq!(
            Shell::format_output(&output, max_output_bytes),
            "stdout:\nhello\nstderr:\nwarn\nexit_code=0"
        );
        assert!(!output.truncated);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_output_is_truncated_and_marked_in_formatted_result() {
        let max_output_bytes = 64;
        let output = Shell::run_with_timeout("printf '%128s' ''", 1_000, max_output_bytes)
            .await
            .expect("large command should succeed");

        assert!(output.status.success());
        assert_eq!(output.stdout.len() + output.stderr.len(), max_output_bytes);
        assert!(output.truncated);

        let formatted = Shell::format_output(&output, max_output_bytes);
        assert!(formatted.contains(&shell_output_truncation_note(max_output_bytes)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn drain_continues_after_capture_cap_so_child_can_finish() {
        let marker = temp_path("drain_marker");
        let _ = fs::remove_file(&marker);
        let command = format!(
            "head -c 4194304 /dev/zero; printf done > {}",
            marker.display()
        );

        let output = Shell::run_with_timeout(&command, 5_000, 1_024)
            .await
            .expect("bounded drain should let the child finish");

        assert!(output.status.success());
        assert!(marker.exists(), "child should complete after large write");

        let _ = fs::remove_file(marker);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn original_timeout_still_wins_after_cap_hit() {
        let start = Instant::now();
        let error = Shell::run_with_timeout("yes x", 100, 1_024)
            .await
            .expect_err("noisy command should time out on the original deadline");

        assert!(error.to_string().contains("timed out"));
        assert!(
            start.elapsed() < Duration::from_millis(POST_CAP_DRAIN_TIMEOUT_MS + 100),
            "original timeout should win over the later post-cap deadline"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_cap_timeout_still_applies_when_earlier_than_original_timeout() {
        let start = Instant::now();
        let error = Shell::run_with_timeout("yes x", 5_000, 1_024)
            .await
            .expect_err("noisy command should time out on the post-cap deadline");

        assert!(error.to_string().contains("timed out"));
        assert!(
            start.elapsed()
                < Duration::from_millis(POST_CAP_DRAIN_TIMEOUT_MS + TERMINATION_GRACE_MS + 500),
            "post-cap deadline should cap total runtime once capture is truncated"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_and_stderr_share_one_capture_budget() {
        let max_output_bytes = 128;
        let output = Shell::run_with_timeout(
            "printf '%96s' ''; printf '%96s' '' 1>&2",
            1_000,
            max_output_bytes,
        )
        .await
        .expect("combined output command should succeed");

        assert!(output.status.success());
        assert_eq!(output.stdout.len() + output.stderr.len(), max_output_bytes);
        assert!(output.stdout.len() > 0);
        assert!(output.stderr.len() > 0);
        assert!(output.truncated);
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
        let error = Shell::run_with_timeout(&command, 100, 1_024)
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
