use anyhow::{Result, ensure};
use tracing::debug;

use crate::agent::ApprovalHandler;
use crate::gate::{DEFAULT_OUTPUT_CAP_BYTES, Verdict, cap_tool_output, guard_text_output};
use crate::llm::ToolCall;
use crate::session::Session;
use crate::turn::Turn;

/// Result of a guarded shell execution path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedShellResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub was_approved: bool,
    pub was_denied: bool,
    pub denial_reason: Option<String>,
}

/// Run the guarded shell path for a simple command string.
#[cfg(test)]
pub async fn guarded_shell_execute<AH>(
    turn: &Turn,
    command: &str,
    call_id: &str,
    session: &Session,
    approval_handler: &mut AH,
) -> Result<GuardedShellResult>
where
    AH: ApprovalHandler + ?Sized,
{
    let call = ToolCall {
        id: call_id.to_string(),
        name: "execute".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };

    guarded_shell_execute_call(turn, &call, session, approval_handler).await
}

/// Run the guarded shell path for a prepared `execute` tool call.
pub async fn guarded_shell_execute_call<AH>(
    turn: &Turn,
    call: &ToolCall,
    session: &Session,
    approval_handler: &mut AH,
) -> Result<GuardedShellResult>
where
    AH: ApprovalHandler + ?Sized,
{
    ensure!(
        call.name == "execute",
        "guarded shell execution requires execute tool calls"
    );

    let verdict = turn.check_tool_call(call);
    let was_approved = match verdict {
        Verdict::Deny { reason, .. } => {
            return Ok(GuardedShellResult {
                output: String::new(),
                exit_code: None,
                was_approved: false,
                was_denied: true,
                denial_reason: Some(reason),
            });
        }
        Verdict::Approve {
            reason,
            gate_id: _gate_id,
            severity,
        } => {
            let command = command_from_call(call);
            if !approval_handler.request_approval(&severity, &reason, &command) {
                return Ok(GuardedShellResult {
                    output: String::new(),
                    exit_code: None,
                    was_approved: false,
                    was_denied: true,
                    denial_reason: Some(reason),
                });
            }
            true
        }
        Verdict::Allow | Verdict::Modify => false,
    };

    execute_shell_call(turn, call, session, was_approved).await
}

/// Execute a shell call after guard policy has already been applied by the caller.
pub(crate) async fn guarded_shell_execute_prechecked(
    turn: &Turn,
    call: &ToolCall,
    session: &Session,
) -> Result<GuardedShellResult> {
    ensure!(
        call.name == "execute",
        "guarded shell execution requires execute tool calls"
    );

    execute_shell_call(turn, call, session, false).await
}

fn command_from_call(call: &ToolCall) -> String {
    match serde_json::from_str::<serde_json::Value>(&call.arguments) {
        Ok(value) => value
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| "<command unavailable>".to_string()),
        Err(_) => "<command unavailable>".to_string(),
    }
}

async fn execute_shell_call(
    turn: &Turn,
    call: &ToolCall,
    session: &Session,
    was_approved: bool,
) -> Result<GuardedShellResult> {
    debug!(call_id = %call.id, tool_name = %call.name, "executing guarded shell call");
    let raw_output = match turn.execute_tool(&call.name, &call.arguments).await {
        Ok(output) => output,
        Err(err) => serde_json::json!({ "error": err.to_string() }).to_string(),
    };
    let exit_code = parse_exit_code(&raw_output);
    let output = guard_text_output(turn, raw_output);
    let output = cap_tool_output(
        session.sessions_dir(),
        &call.id,
        output,
        DEFAULT_OUTPUT_CAP_BYTES,
    )?;

    Ok(GuardedShellResult {
        output,
        exit_code,
        was_approved,
        was_denied: false,
        denial_reason: None,
    })
}

fn parse_exit_code(output: &str) -> Option<i32> {
    output.lines().rev().find_map(|line| {
        line.strip_prefix("exit_code=")
            .and_then(|value| value.parse::<i32>().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::common::shell_policy;
    use crate::gate::{SecretRedactor, ShellSafety};
    use crate::llm::FunctionTool;
    use crate::tool::{Tool, ToolFuture};
    use anyhow::anyhow;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sessions_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_shell_execute_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn test_turn<T: Tool + 'static>(tool: T) -> Turn {
        Turn::new()
            .tool(tool)
            .guard(ShellSafety::with_policy(shell_policy(
                "allow",
                &[],
                &[],
                &[],
                "low",
            )))
            .guard(
                SecretRedactor::new(&[r"shh-[a-zA-Z0-9_-]{8,}"])
                    .expect("test secret redaction regex should be valid"),
            )
    }

    #[derive(Clone)]
    struct RecordingTool {
        executions: Arc<AtomicUsize>,
        output: Arc<Mutex<String>>,
        args: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingTool {
        fn new(output: impl Into<String>) -> (Self, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
            let executions = Arc::new(AtomicUsize::new(0));
            let output = Arc::new(Mutex::new(output.into()));
            let args = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    executions: executions.clone(),
                    output,
                    args: args.clone(),
                },
                executions,
                args,
            )
        }
    }

    impl Tool for RecordingTool {
        fn name(&self) -> &str {
            "execute"
        }

        fn definition(&self) -> FunctionTool {
            FunctionTool {
                name: "execute".to_string(),
                description: "records shell calls".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"},
                        "timeout_ms": {"type": "number"},
                    },
                    "required": ["command"],
                    "additionalProperties": false,
                }),
            }
        }

        fn execute(&self, arguments: &str) -> ToolFuture<'_> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.args
                .lock()
                .expect("arguments mutex poisoned")
                .push(arguments.to_string());
            let output = self.output.lock().expect("output mutex poisoned").clone();
            Box::pin(async move { Ok(output) })
        }
    }

    struct DenyGuard;

    impl crate::gate::Guard for DenyGuard {
        fn name(&self) -> &str {
            "deny"
        }

        fn check(
            &self,
            _event: &mut crate::gate::GuardEvent,
            _context: &crate::gate::GuardContext,
        ) -> Verdict {
            Verdict::Deny {
                reason: "blocked".to_string(),
                gate_id: "deny".to_string(),
            }
        }
    }

    struct ApproveGuard;

    impl crate::gate::Guard for ApproveGuard {
        fn name(&self) -> &str {
            "approve"
        }

        fn check(
            &self,
            _event: &mut crate::gate::GuardEvent,
            _context: &crate::gate::GuardContext,
        ) -> Verdict {
            Verdict::Approve {
                reason: "needs review".to_string(),
                gate_id: "approve".to_string(),
                severity: crate::gate::Severity::Medium,
            }
        }
    }

    fn tool_call(command: &str) -> ToolCall {
        ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: json!({ "command": command }).to_string(),
        }
    }

    #[tokio::test]
    async fn allowed_command_executes_and_returns_exit_code() {
        let (tool, executions, _args) =
            RecordingTool::new("stdout:\nhello\nstderr:\n\nexit_code=0");
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("allowed");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute(
            &turn,
            "echo hello",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.was_denied);
        assert!(!result.was_approved);
        assert_eq!(result.denial_reason, None);
        assert!(result.output.contains("stdout:\nhello"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn denied_command_returns_denied_without_execution_or_results_file() {
        let (tool, executions, _args) =
            RecordingTool::new("stdout:\nsecret\nstderr:\n\nexit_code=0");
        let turn = Turn::new().tool(tool).guard(DenyGuard);
        let dir = temp_sessions_dir("denied");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute_call(
            &turn,
            &tool_call("echo nope"),
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert!(result.was_denied);
        assert!(!result.was_approved);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "");
        assert!(result.denial_reason.as_deref().is_some());
        assert!(!dir.join("results").exists());
    }

    #[tokio::test]
    async fn approved_command_requests_approval_once_and_executes_on_yes() {
        let (tool, executions, _args) = RecordingTool::new("stdout:\nok\nstderr:\n\nexit_code=0");
        let turn = Turn::new().tool(tool).guard(ApproveGuard);
        let dir = temp_sessions_dir("approved");
        let session = Session::new(&dir).unwrap();
        let approvals = Arc::new(AtomicUsize::new(0));
        let approvals_seen = approvals.clone();
        let mut approval_handler =
            move |_severity: &crate::gate::Severity, reason: &str, command: &str| {
                assert_eq!(reason, "needs review");
                assert_eq!(command, "echo review");
                approvals_seen.fetch_add(1, Ordering::SeqCst);
                true
            };

        let result = guarded_shell_execute(
            &turn,
            "echo review",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(approvals.load(Ordering::SeqCst), 1);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(result.was_approved);
        assert!(!result.was_denied);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn approval_rejection_returns_denied_without_execution() {
        let (tool, executions, _args) = RecordingTool::new("stdout:\nok\nstderr:\n\nexit_code=0");
        let turn = Turn::new().tool(tool).guard(ApproveGuard);
        let dir = temp_sessions_dir("approval_reject");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| false;

        let result = guarded_shell_execute(
            &turn,
            "echo review",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert!(result.was_denied);
        assert!(!result.was_approved);
        assert_eq!(result.exit_code, None);
        assert!(!dir.join("results").exists());
    }

    #[tokio::test]
    async fn redacted_output_is_returned_redacted() {
        let (tool, executions, _args) =
            RecordingTool::new("stdout:\nshh-secret-123456\nstderr:\n\nexit_code=0");
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("redacted");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute(
            &turn,
            "echo secret",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(!result.output.contains("shh-secret-123456"));
        assert!(result.output.contains("[REDACTED]"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn capped_output_returns_pointer_and_writes_full_result_file() {
        let payload = "line\n".repeat(2048);
        let raw_output = format!("stdout:\n{payload}\nstderr:\n\nexit_code=0");
        let (tool, executions, _args) = RecordingTool::new(raw_output.clone());
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("capped");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result =
            guarded_shell_execute(&turn, "echo big", "call-1", &session, &mut approval_handler)
                .await
                .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(result.output.contains("output exceeded inline limit"));
        let result_path = dir.join("results").join("call_call-1.txt");
        assert!(result_path.exists());
        assert_eq!(std::fs::read_to_string(result_path).unwrap(), raw_output);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn nonzero_exit_code_is_parsed_from_shell_output() {
        let (tool, _executions, _args) =
            RecordingTool::new("stdout:\nwarn\nstderr:\n\nexit_code=101");
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("exit_code");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute(
            &turn,
            "echo warn",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(result.exit_code, Some(101));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn tool_execution_error_returns_no_exit_code() {
        #[derive(Clone)]
        struct FailingTool;

        impl Tool for FailingTool {
            fn name(&self) -> &str {
                "execute"
            }

            fn definition(&self) -> FunctionTool {
                FunctionTool {
                    name: "execute".to_string(),
                    description: "fails".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"command": {"type": "string"}},
                        "required": ["command"],
                        "additionalProperties": false,
                    }),
                }
            }

            fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
                Box::pin(async { Err(anyhow!("boom")) })
            }
        }

        let turn = test_turn(FailingTool);
        let dir = temp_sessions_dir("err");
        let session = Session::new(&dir).unwrap();
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute(
            &turn,
            "echo boom",
            "call-1",
            &session,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["error"], "boom");
        assert_eq!(result.exit_code, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn prepared_call_entrypoint_preserves_timeout_ms() {
        let (tool, executions, args) = RecordingTool::new("stdout:\nhello\nstderr:\n\nexit_code=0");
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("timeout");
        let session = Session::new(&dir).unwrap();
        let call = ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: json!({ "command": "echo hello", "timeout_ms": 1234 }).to_string(),
        };
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute_call(&turn, &call, &session, &mut approval_handler)
            .await
            .unwrap();

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(
            args.lock()
                .expect("args mutex poisoned")
                .iter()
                .any(|arg| arg.contains(r#""timeout_ms":1234"#))
        );
        assert_eq!(result.exit_code, Some(0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn prepared_call_entrypoint_rejects_non_execute_tool_name() {
        let (tool, executions, _args) =
            RecordingTool::new("stdout:\nhello\nstderr:\n\nexit_code=0");
        let turn = test_turn(tool);
        let dir = temp_sessions_dir("wrong_tool");
        let session = Session::new(&dir).unwrap();
        let call = ToolCall {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
            arguments: json!({ "command": "echo hello" }).to_string(),
        };
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let err = guarded_shell_execute_call(&turn, &call, &session, &mut approval_handler)
            .await
            .expect_err("non-execute tool names should be rejected");

        assert!(err.to_string().contains("execute tool calls"));
        assert_eq!(executions.load(Ordering::SeqCst), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
