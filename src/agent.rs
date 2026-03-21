//! Agent orchestration loop coordinating model turns and tool execution.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Value, from_str};

use crate::guard::{Severity, Verdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall};
use crate::session::Session;
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;
use crate::util::utc_timestamp;

const DEFAULT_OUTPUT_CAP_BYTES: usize = 4096;

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
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool;
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

impl Default for CliTokenSink {
    fn default() -> Self {
        Self::new()
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

impl Default for CliApprovalHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalHandler for CliApprovalHandler {
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        let prefix = match severity {
            Severity::Low => "⚠️",
            Severity::Medium => "🟡",
            Severity::High => "🔴",
        };

        eprintln!("\n{prefix} {reason}");
        eprintln!("  Command: {command}");
        eprint!("  Approve? [y/n]: ");
        if io::stdout().flush().is_err() {
            return false;
        }

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => input.trim().eq_ignore_ascii_case("y"),
            Err(error) => {
                eprintln!("failed to read approval input: {error}");
                false
            }
        }
    }
}

pub enum TurnVerdict {
    Executed(Vec<ToolCall>),
    Denied { reason: String, gate_id: String },
    Approved { tool_calls: Vec<ToolCall> },
}

pub enum QueueOutcome {
    Agent(TurnVerdict),
    Stored,
    UnsupportedRole(String),
}

fn command_from_tool_call(call: &ToolCall) -> Option<String> {
    let value = from_str::<Value>(&call.arguments).ok()?;
    value
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn append_approval_denied(session: &mut Session, reason: &str, command: &str) -> Result<()> {
    session.append(
        ChatMessage::system(format!(
            "Tool execution rejected by user: {reason}. Command: {command}"
        )),
        None,
    )
}

fn append_hard_deny(session: &mut Session, by: &str, reason: &str) -> Result<()> {
    session.append(
        ChatMessage::system(format!("Tool execution hard-denied by {by}: {reason}")),
        None,
    )
}

fn guard_text_output(turn: &Turn, text: String) -> String {
    let mut text = text;
    match turn.check_text_delta(&mut text) {
        Verdict::Deny { .. } => String::new(),
        Verdict::Allow | Verdict::Modify | Verdict::Approve { .. } => text,
    }
}

fn guard_message_output(turn: &Turn, message: &mut ChatMessage) {
    for block in &mut message.content {
        if let MessageContent::Text { text } = block {
            *text = guard_text_output(turn, std::mem::take(text));
        }
    }
    message
        .content
        .retain(|block| !matches!(block, MessageContent::Text { text } if text.is_empty()));
}

fn cap_tool_output(
    sessions_dir: &Path,
    call_id: &str,
    output: String,
    threshold: usize,
) -> Result<String> {
    let results_dir = sessions_dir.join("results");
    fs::create_dir_all(&results_dir).with_context(|| {
        format!(
            "failed to create results directory {}",
            results_dir.display()
        )
    })?;

    let result_path = results_dir.join(format!("{call_id}.txt"));
    fs::write(&result_path, &output)
        .with_context(|| format!("failed to write tool output to {}", result_path.display()))?;

    if output.len() <= threshold {
        return Ok(output);
    }

    let line_count = output.lines().count();
    let size_kb = output.len().div_ceil(1024);
    let path_display = result_path.display();
    Ok(format!(
        "[output exceeded inline limit ({line_count} lines, {size_kb} KB) -> {path_display}]\nTo read: cat {path_display}\nTo read specific lines: sed -n '10,20p' {path_display}"
    ))
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
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + ?Sized,
{
    let user_prompt = format!("[{}] {}", utc_timestamp(), user_prompt);
    let tools = turn.tool_definitions();
    let user_message = ChatMessage::user(user_prompt);
    let mut persisted_user_message = false;

    let mut executed: Vec<ToolCall> = Vec::new();
    let mut had_user_approval = false;

    'agent_turn: loop {
        session.ensure_context_within_limit();
        let mut messages = session.history().to_vec();
        if !persisted_user_message {
            messages.push(user_message.clone());
        }

        let verdict = turn.check_inbound(&mut messages);
        if !persisted_user_message {
            let user_message = messages
                .iter()
                .rev()
                .find(|message| message.role == crate::llm::ChatRole::User)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing user message after inbound checks"))?;
            session.append(user_message, None)?;
            persisted_user_message = true;
        }

        match verdict {
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
                        message.content.iter().find_map(|block| match block {
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
        let mut streamed_output = String::new();
        let mut turn_reply = provider
            .stream_completion(&messages, &tools, &mut |token| {
                streamed_output.push_str(&token)
            })
            .await?;
        if !streamed_output.is_empty() {
            let redacted_output = guard_text_output(turn, streamed_output);
            if !redacted_output.is_empty() {
                token_sink.on_token(redacted_output);
            }
        }
        guard_message_output(turn, &mut turn_reply.assistant_message);
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
                            let approved =
                                approval_handler.request_approval(&severity, &reason, &command);
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
                    let result = guard_text_output(turn, result);
                    let result = cap_tool_output(
                        session.sessions_dir(),
                        &call.id,
                        result,
                        DEFAULT_OUTPUT_CAP_BYTES,
                    )?;

                    session.append(ChatMessage::tool_result(&call.id, &call.name, result), None)?;
                    executed.push(call.clone());
                }
            }

            StopReason::Stop => {
                session.append(turn_reply.assistant_message, turn_meta)?;
                token_sink.on_complete();
                if had_user_approval {
                    return Ok(TurnVerdict::Approved {
                        tool_calls: executed,
                    });
                }
                return Ok(TurnVerdict::Executed(executed));
            }
        }
    }
}

pub async fn process_message<F, Fut, P, TS, AH>(
    message: &QueuedMessage,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<QueueOutcome>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + ?Sized,
{
    match message.role.as_str() {
        "user" => Ok(QueueOutcome::Agent(
            run_agent_loop(
                make_provider,
                session,
                message.content.clone(),
                turn,
                token_sink,
                approval_handler,
            )
            .await?,
        )),
        "system" => {
            session.append(ChatMessage::system(message.content.clone()), None)?;
            Ok(QueueOutcome::Stored)
        }
        "assistant" => {
            session.append(
                ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::text(message.content.clone())],
                },
                None,
            )?;
            Ok(QueueOutcome::Stored)
        }
        other => Ok(QueueOutcome::UnsupportedRole(other.to_string())),
    }
}

pub async fn drain_queue<F, Fut, P>(
    store: &mut Store,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    while let Some(message) = store.dequeue_next_message(session_id)? {
        let outcome = process_message(
            &message,
            session,
            turn,
            make_provider,
            token_sink,
            approval_handler,
        )
        .await;

        match outcome {
            Ok(QueueOutcome::Agent(verdict)) => {
                store.mark_processed(message.id)?;
                match verdict {
                    TurnVerdict::Executed(_) => {}
                    TurnVerdict::Approved { .. } => {
                        eprintln!("Command approved by user and executed.");
                    }
                    TurnVerdict::Denied { reason, gate_id } => {
                        eprintln!("Command hard-denied by {gate_id}: {reason}");
                    }
                }
            }
            Ok(QueueOutcome::Stored) => {
                store.mark_processed(message.id)?;
            }
            Ok(QueueOutcome::UnsupportedRole(role)) => {
                eprintln!(
                    "unsupported queued role '{role}' for message {}",
                    message.id
                );
                store.mark_processed(message.id)?;
            }
            Err(error) => {
                store.mark_failed(message.id)?;
                return Err(error);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::context::History;
    use crate::guard::SecretRedactor;
    use crate::llm::{FunctionTool, StreamedTurn};
    use crate::store::Store;
    use crate::tool::{Shell, Tool, ToolFuture};
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

    #[derive(Clone)]
    struct StreamingProvider {
        streamed_tokens: Vec<String>,
        turn: StreamedTurn,
    }

    impl crate::llm::LlmProvider for StreamingProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            for token in &self.streamed_tokens {
                on_token(token.clone());
            }
            Ok(self.turn.clone())
        }
    }

    #[derive(Clone)]
    struct SequenceProvider {
        turns: std::sync::Arc<std::sync::Mutex<Vec<StreamedTurn>>>,
    }

    impl SequenceProvider {
        fn new(turns: Vec<StreamedTurn>) -> Self {
            Self {
                turns: std::sync::Arc::new(std::sync::Mutex::new(
                    turns.into_iter().rev().collect(),
                )),
            }
        }
    }

    impl crate::llm::LlmProvider for SequenceProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.turns
                .lock()
                .expect("sequence provider mutex poisoned")
                .pop()
                .ok_or_else(|| anyhow::anyhow!("no more turns"))
        }
    }

    #[derive(Clone)]
    struct StaticProvider {
        turn: StreamedTurn,
    }

    impl crate::llm::LlmProvider for StaticProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            Ok(self.turn.clone())
        }
    }

    #[derive(Clone)]
    struct FailingProvider;

    impl crate::llm::LlmProvider for FailingProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            _tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            Err(anyhow::anyhow!("provider failure"))
        }
    }

    struct LeakyTool;

    impl Tool for LeakyTool {
        fn name(&self) -> &str {
            "leak"
        }

        fn definition(&self) -> FunctionTool {
            FunctionTool {
                name: "leak".to_string(),
                description: "Return a secret".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            }
        }

        fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
            Box::pin(async { Ok("stdout:\nsk-proj-abcdefghijklmnopqrstuvwxyz012345".to_string()) })
        }
    }

    struct StaticOutputTool {
        name: &'static str,
        output: String,
    }

    impl Tool for StaticOutputTool {
        fn name(&self) -> &str {
            self.name
        }

        fn definition(&self) -> FunctionTool {
            FunctionTool {
                name: self.name.to_string(),
                description: "Return static output".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
            }
        }

        fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
            let output = self.output.clone();
            Box::pin(async move { Ok(output) })
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

    fn temp_queue_root(prefix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aprs_agent_queue_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn message_text(message: &ChatMessage) -> Option<&str> {
        message.content.iter().find_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    fn tool_result_text(message: &ChatMessage) -> Option<&str> {
        message.content.iter().find_map(|block| match block {
            MessageContent::ToolResult { result } => Some(result.content.as_str()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn drain_queue_processes_user_system_and_unknown_roles() {
        let root = temp_queue_root("mixed_roles");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "worker";
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session(session_id, None).unwrap();
        let user_id = store
            .enqueue_message(session_id, "user", "hello", "cli")
            .unwrap();
        let system_id = store
            .enqueue_message(session_id, "system", "operational note", "cli")
            .unwrap();
        let unknown_id = store
            .enqueue_message(session_id, "tool", "orphan tool result", "cli")
            .unwrap();

        let mut session = Session::new(&sessions_dir).unwrap();
        let turn = Turn::new();
        let provider_calls = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let provider_calls_seen = provider_calls.clone();
        let mut provider_factory = move || {
            let provider_calls_seen = provider_calls_seen.clone();
            async move {
                *provider_calls_seen
                    .lock()
                    .expect("provider call counter mutex poisoned") += 1;
                Ok::<_, anyhow::Error>(StaticProvider {
                    turn: StreamedTurn {
                        assistant_message: ChatMessage {
                            role: crate::llm::ChatRole::Assistant,
                            content: vec![MessageContent::text("ok")],
                        },
                        tool_calls: vec![],
                        meta: None,
                        stop_reason: StopReason::Stop,
                    },
                })
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        drain_queue(
            &mut store,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(
            *provider_calls
                .lock()
                .expect("provider call counter mutex poisoned"),
            1
        );
        assert!(session.history().iter().any(|message| {
            matches!(message.role, crate::llm::ChatRole::System)
                && message_text(message) == Some("operational note")
        }));
        assert!(
            !session
                .history()
                .iter()
                .any(|message| { message_text(message) == Some("orphan tool result") })
        );

        let conn = Connection::open(&queue_path).unwrap();
        for message_id in [user_id, system_id, unknown_id] {
            let status: String = conn
                .query_row(
                    "SELECT status FROM messages WHERE id = ?1",
                    [message_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "processed");
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn drain_queue_marks_failed_when_agent_loop_errors() {
        let root = temp_queue_root("failed_marking");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "worker";
        let mut store = Store::new(&queue_path).unwrap();
        store.create_session(session_id, None).unwrap();
        let message_id = store
            .enqueue_message(session_id, "user", "run something", "cli")
            .unwrap();

        let mut session = Session::new(&sessions_dir).unwrap();
        let turn = Turn::new();
        let mut provider_factory = || async { Ok::<_, anyhow::Error>(FailingProvider) };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let result = drain_queue(
            &mut store,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await;

        assert!(result.is_err());

        let conn = Connection::open(&queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");

        std::fs::remove_dir_all(&root).unwrap();
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

    #[tokio::test]
    async fn inbound_redaction_is_persisted_before_session_write() {
        let dir = temp_sessions_dir("redaction_persisted");
        let (provider, _observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();

        let turn = Turn::new().guard(SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"]));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "please store sk-proj-abcdefghijklmnopqrstuvwxyz012345".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let session_file = std::fs::read_to_string(session.today_path()).unwrap();
        assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
        assert!(session_file.contains("[REDACTED]"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn context_insertion_does_not_replace_persisted_user_message() {
        let dir = temp_sessions_dir("persist_user_with_context");
        let identity_dir = dir.join("identity");
        std::fs::create_dir_all(&identity_dir).unwrap();
        std::fs::write(identity_dir.join("constitution.md"), "constitution").unwrap();
        std::fs::write(identity_dir.join("identity.md"), "identity").unwrap();
        std::fs::write(identity_dir.join("context.md"), "context").unwrap();

        let (provider, _observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().context(crate::context::Identity::new(
            identity_dir.to_str().unwrap(),
            std::collections::HashMap::new(),
            "fallback",
        ));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "store this user prompt".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let first = session
            .history()
            .first()
            .expect("user message should be persisted");
        assert!(matches!(first.role, crate::llm::ChatRole::User));
        let content = match &first.content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text content"),
        };
        assert!(content.contains("store this user prompt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn outbound_redaction_is_streamed_and_persisted_before_session_write() {
        let dir = temp_sessions_dir("outbound_redaction");
        let provider = StreamingProvider {
            streamed_tokens: vec!["sk-proj-abcdefghijklmnopqrstuvwxyz012345".to_string()],
            turn: StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::text(
                        "sk-proj-abcdefghijklmnopqrstuvwxyz012345",
                    )],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        };
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().guard(SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"]));
        let mut make_provider = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let streamed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let streamed_tokens = streamed.clone();
        let mut token_sink = move |token: String| {
            streamed_tokens
                .lock()
                .expect("streamed token mutex poisoned")
                .push_str(&token);
        };
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "reply with the secret".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let streamed = streamed.lock().expect("streamed token mutex poisoned");
        assert_eq!(streamed.as_str(), "[REDACTED]");
        let session_file = std::fs::read_to_string(session.today_path()).unwrap();
        assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
        assert!(session_file.contains("[REDACTED]"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn tool_output_is_redacted_before_persist() {
        let dir = temp_sessions_dir("tool_redaction");
        let provider = SequenceProvider::new(vec![
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::ToolCall {
                        call: ToolCall {
                            id: "call-1".to_string(),
                            name: "leak".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }],
                },
                tool_calls: vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "leak".to_string(),
                    arguments: "{}".to_string(),
                }],
                meta: None,
                stop_reason: StopReason::ToolCalls,
            },
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::text("done")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        ]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new()
            .tool(LeakyTool)
            .guard(SecretRedactor::new(&[r"sk-[a-zA-Z0-9_-]{20,}"]));
        let mut make_provider = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "use the tool".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let session_file = std::fs::read_to_string(session.today_path()).unwrap();
        assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
        assert!(session_file.contains("[REDACTED]"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn tool_output_below_threshold_is_inline_and_saved_to_file() {
        let dir = temp_sessions_dir("tool_output_inline");
        let call_id = "call-inline";
        let output = "stdout:\nsmall output\nstderr:\n\nexit_code=0".to_string();
        let provider = SequenceProvider::new(vec![
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::ToolCall {
                        call: ToolCall {
                            id: call_id.to_string(),
                            name: "static".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }],
                },
                tool_calls: vec![ToolCall {
                    id: call_id.to_string(),
                    name: "static".to_string(),
                    arguments: "{}".to_string(),
                }],
                meta: None,
                stop_reason: StopReason::ToolCalls,
            },
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::text("done")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        ]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().tool(StaticOutputTool {
            name: "static",
            output: output.clone(),
        });
        let mut make_provider = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "use the tool".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let tool_message = session
            .history()
            .iter()
            .find(|message| matches!(message.role, crate::llm::ChatRole::Tool))
            .expect("tool result should be persisted");
        assert_eq!(tool_result_text(tool_message), Some(output.as_str()));

        let result_path = dir.join("results").join(format!("{call_id}.txt"));
        assert!(result_path.exists(), "result file should be created");
        assert_eq!(std::fs::read_to_string(result_path).unwrap(), output);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cap_tool_output_creates_results_directory() {
        let dir = temp_sessions_dir("cap_tool_output_dir");
        let output = "stdout:\nhello\nstderr:\n\nexit_code=0".to_string();

        let capped =
            cap_tool_output(&dir, "call-dir", output.clone(), DEFAULT_OUTPUT_CAP_BYTES).unwrap();

        assert_eq!(capped, output);
        let result_path = dir.join("results").join("call-dir.txt");
        assert!(result_path.exists(), "result file should be created");
        assert_eq!(std::fs::read_to_string(result_path).unwrap(), output);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn tool_output_above_threshold_is_capped_with_metadata_pointer() {
        let dir = temp_sessions_dir("tool_output_capped");
        let call_id = "call-capped";
        let output = "line\n".repeat(2048);
        let provider = SequenceProvider::new(vec![
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::ToolCall {
                        call: ToolCall {
                            id: call_id.to_string(),
                            name: "static".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }],
                },
                tool_calls: vec![ToolCall {
                    id: call_id.to_string(),
                    name: "static".to_string(),
                    arguments: "{}".to_string(),
                }],
                meta: None,
                stop_reason: StopReason::ToolCalls,
            },
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    content: vec![MessageContent::text("done")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        ]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().tool(StaticOutputTool {
            name: "static",
            output: output.clone(),
        });
        let mut make_provider = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        run_agent_loop(
            &mut make_provider,
            &mut session,
            "use the tool".to_string(),
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let tool_message = session
            .history()
            .iter()
            .find(|message| matches!(message.role, crate::llm::ChatRole::Tool))
            .expect("tool result should be persisted");
        let tool_result = tool_result_text(tool_message).expect("tool result text should exist");
        assert!(!tool_result.contains(&output));
        let expected_path = dir.join("results").join(format!("{call_id}.txt"));
        let expected_path_str = expected_path.display().to_string();
        assert!(tool_result.contains(&format!(
            "[output exceeded inline limit (2048 lines, 10 KB) -> {expected_path_str}]"
        )));
        assert!(tool_result.contains(&format!("To read: cat {expected_path_str}")));
        assert!(tool_result.contains(&format!(
            "To read specific lines: sed -n '10,20p' {expected_path_str}"
        )));

        let result_path = dir.join("results").join(format!("{call_id}.txt"));
        assert!(result_path.exists(), "result file should be created");
        assert_eq!(std::fs::read_to_string(result_path).unwrap(), output);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
