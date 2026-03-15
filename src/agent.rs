//! Agent orchestration loop coordinating model turns and tool execution.

use std::io::{self, Write};

use anyhow::Result;
use serde_json::{from_str, Value};

use crate::guard::{Severity, Verdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall};
use crate::session::Session;
use crate::turn::Turn;
use crate::util::utc_timestamp;

/// Receiver of streaming tokens emitted by the model during completion.
pub trait TokenSink {
    fn on_token(&mut self, token: String);
    fn on_complete(&mut self) {}
}

impl<F> TokenSink for F
where
    F: FnMut(String),
{
    fn on_token(&mut self, token: String) {
        self(token)
    }
}

/// Request approval for execution paths that need user confirmation.
pub trait ApprovalHandler {
    fn request_approval(
        &mut self,
        severity: &Severity,
        reason: &str,
        command: &str,
    ) -> bool;
}

impl<F> ApprovalHandler for F
where
    F: FnMut(&Severity, &str, &str) -> bool,
{
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        self(severity, reason, command)
    }
}

/// CLI token sink implementation.
pub struct CliTokenSink;

impl CliTokenSink {
    pub fn new() -> Self {
        Self
    }
}

impl TokenSink for CliTokenSink {
    fn on_token(&mut self, token: String) {
        print!("{token}");
        if let Err(err) = io::stdout().flush() {
            eprintln!("failed to flush stdout: {err}");
        }
    }

    fn on_complete(&mut self) {
        println!();
    }
}

/// CLI approval handler implementation.
pub struct CliApprovalHandler;

impl CliApprovalHandler {
    pub fn new() -> Self {
        Self
    }
}

impl ApprovalHandler for CliApprovalHandler {
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        let prefix = match severity {
            Severity::Low => "⚠️",
            Severity::High => "🔴",
        };

        eprintln!("\n{prefix} {reason}");
        eprintln!("  Command: {command}");
        eprint!("  Approve? [y/n]: ");
        if io::stdout().flush().is_err() {
            return false;
        }

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap_or(0);
        input.trim().eq_ignore_ascii_case("y")
    }
}

pub enum TurnVerdict {
    Executed(Vec<ToolCall>),
    Denied { reason: String, gate_id: String },
    Approved { tool_calls: Vec<ToolCall> },
}

fn command_from_tool_call(call: &ToolCall) -> Option<String> {
    let value = from_str::<Value>(&call.arguments).ok()?;
    value.get("command").and_then(Value::as_str).map(ToString::to_string)
}

fn append_approval_denied(session: &mut Session, reason: &str, command: &str) -> Result<()> {
    session.append(
        ChatMessage::system(format!("Tool execution rejected by user: {reason}. Command: {command}")),
        None,
    )
}

fn append_hard_deny(session: &mut Session, by: &str, reason: &str) -> Result<()> {
    session.append(ChatMessage::system(format!("Tool execution hard-denied by {by}: {reason}")), None)
}

/// Run the agent loop until the model emits a non-tool stop reason.
pub async fn run_agent_loop<F, Fut, P, TS, AH>(
    make_provider: &mut F,
    session: &mut Session,
    user_prompt: String,
    turn: &Turn,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<TurnVerdict>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TS: TokenSink + Send,
    AH: ApprovalHandler,
{
    let user_prompt = format!("[{}] {}", utc_timestamp(), user_prompt);
    let tools = turn.tool_definitions();
    session.add_user_message(user_prompt)?;

    let mut executed: Vec<ToolCall> = Vec::new();
    let mut had_user_approval = false;

    'agent_turn: loop {
        session.ensure_context_within_limit();
        let mut messages = session.history().to_vec();
        match turn.check_inbound(&mut messages) {
            Verdict::Allow => {}
            Verdict::Modify => {}
            Verdict::Deny { reason, gate_id } => {
                session.append(
                    ChatMessage::system(format!("Message hard-denied by {gate_id}: {reason}")),
                    None,
                )?;
                continue;
            }
            Verdict::Approve {
                reason,
                gate_id: _,
                severity,
            } => {
                let command = messages
                    .iter()
                    .find_map(|message| {
                        message
                            .content
                            .iter()
                            .find_map(|block| match block {
                                MessageContent::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                    })
                    .unwrap_or("<inbound message>");
                let approved = approval_handler.request_approval(&severity, &reason, command);
                if !approved {
                    append_approval_denied(session, &reason, command)?;
                    continue;
                }
            }
        }

        if messages.is_empty() {
            continue;
        }

        let provider = make_provider().await?;
        let turn_reply = provider
            .stream_completion(&messages, &tools, &mut |token| token_sink.on_token(token))
            .await?;
        let turn_meta = turn_reply.meta;

        match turn_reply.stop_reason {
            StopReason::ToolCalls => {
                let tool_calls = turn_reply.tool_calls.clone();
                session.append(turn_reply.assistant_message, turn_meta)?;

                for call in &tool_calls {
                    match turn.check_tool_call(call) {
                        Verdict::Allow => {}
                        Verdict::Modify => {}
                        Verdict::Deny { reason, gate_id } => {
                            append_hard_deny(session, &gate_id, &reason)?;
                            continue 'agent_turn;
                        }
                        Verdict::Approve {
                            reason,
                            gate_id: _,
                            severity,
                        } => {
                            let command = command_from_tool_call(call)
                                .unwrap_or_else(|| "<command unavailable>".to_string());
                            let approved = approval_handler.request_approval(&severity, &reason, &command);
                            if !approved {
                                append_approval_denied(session, &reason, &command)?;
                                continue 'agent_turn;
                            }
                            had_user_approval = true;
                        }
                    }
                }

                match turn.check_tool_batch(&tool_calls) {
                    Verdict::Allow => {}
                    Verdict::Modify => {}
                    Verdict::Deny { reason, gate_id } => {
                        append_hard_deny(session, &gate_id, &reason)?;
                        continue 'agent_turn;
                    }
                    Verdict::Approve {
                        reason,
                        gate_id: _,
                        severity,
                    } => {
                        let command = tool_calls
                            .first()
                            .and_then(command_from_tool_call)
                            .unwrap_or_else(|| "<command unavailable>".to_string());
                        if !approval_handler.request_approval(&severity, &reason, &command) {
                            append_approval_denied(session, &reason, &command)?;
                            continue 'agent_turn;
                        }
                        had_user_approval = true;
                    }
                }

                for call in &tool_calls {
                    let result = match turn.execute_tool(&call.name, &call.arguments).await {
                        Ok(output) => output,
                        Err(err) => format!(r#"{{"error": "{err}"}}"#),
                    };

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result), None)?;
                    executed.push(call.clone());
                }
            }

            StopReason::Stop => {
                session.append(turn_reply.assistant_message, turn_meta)?;
                token_sink.on_complete();
                if had_user_approval {
                    return Ok(TurnVerdict::Approved { tool_calls: executed });
                }
                return Ok(TurnVerdict::Executed(executed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::context::History;
    use crate::guard::SecretRedactor;
    use crate::llm::{FunctionTool, StreamedTurn};
    use crate::tool::Shell;
    use crate::turn::Turn;

    #[derive(Clone)]
    struct InspectingProvider {
        observed_message_counts: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    }

    impl InspectingProvider {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
            let observed_message_counts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (
                Self {
                    observed_message_counts: observed_message_counts.clone(),
                },
                observed_message_counts,
            )
        }
    }

    impl crate::llm::LlmProvider for InspectingProvider {
        async fn stream_completion(
            &self,
            messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.observed_message_counts
                .lock()
                .expect("observed message count mutex poisoned")
                .push(messages.len());

            Ok(StreamedTurn {
                assistant_message: ChatMessage::system("ok"),
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            })
        }
    }

    fn temp_sessions_dir(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_agent_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    #[ignore]
    async fn trims_context_before_stream_completion_when_over_estimated_limit() {
        let dir = temp_sessions_dir("pre_call_trim");
        let (provider, observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();
        session.set_max_context_tokens(1);

        session.add_user_message("one").unwrap();
        session.add_user_message("two").unwrap();
        session.add_user_message("three").unwrap();

        let turn = Turn::new()
            .context(History::new(1_000))
            .tool(Shell::new())
            .guard(SecretRedactor::new(&[]));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;
        let _verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "new command".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let observed = observed_message_counts
            .lock()
            .expect("observed mutex poisoned");
        assert!(
            observed.first().cloned().is_some_and(|count| count <= 3),
            "expected pre-call trimming to run before stream completion"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
