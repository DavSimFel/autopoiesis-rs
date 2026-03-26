use anyhow::Result;

use crate::agent::ApprovalHandler;
use crate::agent::shell_execute::GuardedShellResult;
use crate::llm::ToolCall;
use crate::session::Session;
use crate::turn::Turn;

pub async fn guarded_shell_execute_call<AH>(
    turn: &Turn,
    call: &ToolCall,
    session: &Session,
    approval_handler: &mut AH,
) -> Result<GuardedShellResult>
where
    AH: ApprovalHandler + ?Sized,
{
    crate::agent::shell_execute::guarded_shell_execute_call(turn, call, session, approval_handler)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{Guard, GuardContext, GuardEvent, Severity, Verdict};
    use crate::tool::Shell;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sessions_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_plan_executor_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    struct ApproveGuard;

    impl Guard for ApproveGuard {
        fn name(&self) -> &str {
            "approve"
        }

        fn check(&self, _event: &mut GuardEvent, _context: &GuardContext) -> Verdict {
            Verdict::Approve {
                reason: "needs review".to_string(),
                gate_id: "approve".to_string(),
                severity: Severity::Medium,
            }
        }
    }

    #[tokio::test]
    async fn guarded_shell_execute_call_runs_through_full_path() {
        let dir = temp_sessions_dir("guarded");
        let session = Session::new(&dir).unwrap();
        let turn = Turn::new().tool(Shell::new()).guard(ApproveGuard);
        let call = ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: json!({ "command": "echo plan-executor", "timeout_ms": 1234 }).to_string(),
        };
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let result = guarded_shell_execute_call(&turn, &call, &session, &mut approval_handler)
            .await
            .unwrap();

        assert!(result.was_approved);
        assert!(!result.was_denied);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.output.contains("plan-executor"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
