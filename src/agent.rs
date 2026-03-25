//! Agent orchestration loop coordinating model turns and tool execution.

use anyhow::{Context, Result};
use serde_json::{Value, from_str};
use std::path::Path;

use crate::delegation::{self, DelegationAdvice};
use crate::gate::{GuardContext, Severity, Verdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall, TurnMeta};
use crate::principal::Principal;
use crate::session::Session;
pub use crate::spawn::{SpawnDrainResult, SpawnRequest, SpawnResult};
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;
use crate::util::utc_timestamp;
use tracing::{debug, info, warn};

const MAX_DENIALS_PER_TURN: usize = 2;

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

/// Convenience wrapper for spawning a child session through the shared spawn module.
pub fn spawn_child(
    store: &mut Store,
    config: &crate::config::Config,
    parent_budget: crate::gate::BudgetSnapshot,
    request: SpawnRequest,
) -> Result<SpawnResult> {
    crate::spawn::spawn_child(store, config, parent_budget, request)
}

async fn spawn_and_drain_with_provider<F, Fut, P, TS>(
    store: &mut Store,
    config: &crate::config::Config,
    session_dir: &Path,
    request: SpawnRequest,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<SpawnDrainResult>
where
    F: FnMut(&crate::config::Config) -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TS: TokenSink + Send,
{
    let parent_session_dir = session_dir.join(&request.parent_session_id);
    let mut parent_session =
        Session::new(&parent_session_dir).context("failed to open parent session")?;
    parent_session
        .load_today()
        .context("failed to load parent session history")?;
    let parent_budget = parent_session
        .budget_snapshot()
        .context("failed to read parent budget snapshot")?;

    let spawn_result = crate::spawn::spawn_child(store, config, parent_budget, request)?;
    let metadata_json = store
        .get_session_metadata(&spawn_result.child_session_id)?
        .ok_or_else(|| anyhow::anyhow!("spawned child session metadata is missing"))?;
    let context = SpawnDrainContext {
        store,
        config,
        session_dir,
        spawn_result,
    };
    finish_spawned_child_drain(
        context,
        &metadata_json,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

struct SpawnDrainContext<'a> {
    store: &'a mut Store,
    config: &'a crate::config::Config,
    session_dir: &'a Path,
    spawn_result: SpawnResult,
}

async fn finish_spawned_child_drain<F, Fut, P, TS>(
    context: SpawnDrainContext<'_>,
    metadata_json: &str,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<SpawnDrainResult>
where
    F: FnMut(&crate::config::Config) -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TS: TokenSink + Send,
{
    let metadata = crate::spawn::parse_child_session_metadata(metadata_json)?;
    if metadata.resolved_model != context.spawn_result.resolved_model {
        return Err(anyhow::anyhow!(
            "spawned child metadata resolved_model does not match spawn result"
        ));
    }

    let child_config = context.config.with_spawned_child_runtime(
        &metadata.tier,
        &metadata.resolved_provider_model,
        metadata.reasoning_override.as_deref(),
    )?;
    let turn = match metadata.tier.as_str() {
        "t3" => crate::turn::build_spawned_t3_turn(&child_config, metadata.skills.clone()),
        _ => crate::turn::build_turn_for_config(&child_config),
    };

    let child_session_dir = context
        .session_dir
        .join(&context.spawn_result.child_session_id);
    let mut child_session =
        Session::new(&child_session_dir).context("failed to open child session")?;
    child_session
        .load_today()
        .context("failed to load child session history")?;

    let mut make_provider_for_turn = || make_provider(&child_config);
    match drain_queue(
        context.store,
        &context.spawn_result.child_session_id,
        &mut child_session,
        &turn,
        &mut make_provider_for_turn,
        token_sink,
        approval_handler,
    )
    .await?
    {
        Some(TurnVerdict::Denied { reason, gate_id }) => {
            return Err(anyhow::anyhow!(
                "child session denied by {gate_id}: {reason}"
            ));
        }
        Some(TurnVerdict::Executed(_)) | Some(TurnVerdict::Approved { .. }) => {
            return Err(anyhow::anyhow!(
                "child drain returned an unexpected terminal verdict"
            ));
        }
        None => {}
    }

    let last_assistant_response = crate::spawn::latest_assistant_response(&child_session);
    Ok(SpawnDrainResult {
        child_session_id: context.spawn_result.child_session_id,
        resolved_model: context.spawn_result.resolved_model,
        last_assistant_response,
    })
}

/// Spawn a child session and drain its queue to completion.
pub async fn spawn_and_drain(
    store: &mut Store,
    config: &crate::config::Config,
    session_dir: impl AsRef<Path>,
    request: SpawnRequest,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<SpawnDrainResult> {
    let http_client = reqwest::Client::new();
    let mut provider_factory = move |child_config: &crate::config::Config| {
        let client = http_client.clone();
        let child_config = child_config.clone();
        async move {
            let api_key = crate::auth::get_valid_token().await?;
            Ok::<crate::llm::openai::OpenAIProvider, anyhow::Error>(
                crate::llm::openai::OpenAIProvider::with_client(
                    client,
                    api_key,
                    child_config.base_url,
                    child_config.model,
                    child_config.reasoning_effort,
                ),
            )
        }
    };
    let mut token_sink = |_token: String| {};

    spawn_and_drain_with_provider(
        store,
        config,
        session_dir.as_ref(),
        request,
        &mut provider_factory,
        &mut token_sink,
        approval_handler,
    )
    .await
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

fn append_audit_note(session: &mut Session, note: String) -> Result<()> {
    let mut message = ChatMessage::with_role_with_principal(
        crate::llm::ChatRole::Assistant,
        Some(Principal::System),
    );
    message.content.push(MessageContent::text(note));
    session.append(message, None)
}

fn persist_denied_assistant_text(
    session: &mut Session,
    turn: &Turn,
    mut assistant_message: ChatMessage,
    meta: Option<TurnMeta>,
) -> Result<()> {
    crate::gate::guard_message_output(turn, &mut assistant_message);
    assistant_message
        .content
        .retain(|block| matches!(block, MessageContent::Text { .. }));

    if assistant_message.content.is_empty() {
        assistant_message.content.push(MessageContent::Text {
            text: String::new(),
        });
    }

    session.append(assistant_message, meta)
}

fn append_approval_denied(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(
        session,
        format!("Tool execution rejected after approval by {gate_id}"),
    )
}

fn append_inbound_approval_denied(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(
        session,
        format!("Message rejected after approval by {gate_id}"),
    )
}

fn append_hard_deny(session: &mut Session, by: &str) -> Result<()> {
    append_audit_note(session, format!("Tool execution hard-denied by {by}"))
}

fn append_inbound_deny(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(session, format!("Message hard-denied by {gate_id}"))
}

fn make_denial_verdict(denial_count: &mut usize, gate_id: String, reason: String) -> TurnVerdict {
    *denial_count += 1;
    if *denial_count >= MAX_DENIALS_PER_TURN {
        TurnVerdict::Denied {
            reason: format!(
                "stopped after {} denied actions this turn; last denial by {gate_id}: {reason}",
                *denial_count
            ),
            gate_id,
        }
    } else {
        TurnVerdict::Denied { reason, gate_id }
    }
}

pub fn format_denial_message(reason: &str, gate_id: &str) -> String {
    format!("Command hard-denied by {gate_id}: {reason}")
}

fn inbound_verdict_name(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Allow => "allow",
        Verdict::Modify => "modify",
        Verdict::Deny { .. } => "deny",
        Verdict::Approve { .. } => "approve",
    }
}

fn maybe_queue_delegation_hint(
    session: &Session,
    turn: &Turn,
    tool_call_count: usize,
) -> Result<()> {
    if !delegation::delegation_enabled(turn.delegation_config()) {
        return Ok(());
    }

    let advice = delegation::check_delegation(session, tool_call_count, turn.delegation_config());
    if let DelegationAdvice::SuggestDelegation { reason } = &advice {
        debug!(%reason, "delegation advised");
        session.queue_delegation_hint(delegation::DELEGATION_HINT)?;
    }

    Ok(())
}

#[tracing::instrument(level = "debug", skip(message, session, turn, make_provider, token_sink, approval_handler), fields(message_id = message.id, session_id = %message.session_id, role = %message.role))]
pub(crate) async fn process_queued_message<F, Fut, P, TS, AH>(
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
                Principal::from_source(&message.source),
                turn,
                token_sink,
                approval_handler,
            )
            .await?,
        )),
        "system" => {
            session.append(
                ChatMessage::system_with_principal(
                    message.content.clone(),
                    Some(Principal::from_source(&message.source)),
                ),
                None,
            )?;
            Ok(QueueOutcome::Stored)
        }
        "assistant" => {
            let mut assistant = ChatMessage::with_role_with_principal(
                crate::llm::ChatRole::Assistant,
                Some(Principal::from_source(&message.source)),
            );
            assistant
                .content
                .push(MessageContent::text(message.content.clone()));
            session.append(assistant, None)?;
            Ok(QueueOutcome::Stored)
        }
        other => Ok(QueueOutcome::UnsupportedRole(other.to_string())),
    }
}

/// Run the agent loop until the model emits a non-tool stop reason.
#[tracing::instrument(level = "info", skip(make_provider, session, turn, token_sink, approval_handler), fields(user_principal = ?user_principal))]
pub async fn run_agent_loop<F, Fut, P, TS, AH>(
    make_provider: &mut F,
    session: &mut Session,
    user_prompt: String,
    user_principal: Principal,
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
    let user_message = ChatMessage::user_with_principal(user_prompt, Some(user_principal));
    let mut persisted_user_message = false;

    let mut executed: Vec<ToolCall> = Vec::new();
    let mut had_user_approval = false;
    let mut denial_count = 0usize;
    let mut turn_tool_call_count = 0usize;
    info!("starting agent turn");

    let denied_turn = 'agent_turn: loop {
        session.ensure_context_within_limit();
        let mut messages = session.history().to_vec();
        if !persisted_user_message {
            messages.push(user_message.clone());
        }
        debug!(message_count = messages.len(), "assembled turn context");

        let budget_context = if turn.needs_budget_context() {
            Some(GuardContext {
                budget: session
                    .budget_snapshot()
                    .context("failed to read live budget snapshot")?,
                ..Default::default()
            })
        } else {
            None
        };
        let verdict = turn.check_inbound(&mut messages, budget_context);
        debug!(
            verdict = inbound_verdict_name(&verdict),
            "inbound guard verdict"
        );

        match verdict {
            Verdict::Allow | Verdict::Modify => {
                if !persisted_user_message {
                    let user_message = messages
                        .iter()
                        .rev()
                        .find(|message| message.role == crate::llm::ChatRole::User)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing user message after inbound checks")
                        })?;
                    session.append(user_message, None)?;
                    persisted_user_message = true;
                    info!("persisted inbound user message");
                }
            }
            Verdict::Deny { reason, gate_id } => {
                if !persisted_user_message {
                    let mut user_message = messages
                        .iter()
                        .rev()
                        .find(|message| message.role == crate::llm::ChatRole::User)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing user message after inbound checks")
                        })?;
                    crate::gate::guard_message_output(turn, &mut user_message);
                    session.append(user_message, None)?;
                }
                append_inbound_deny(session, &gate_id)?;
                warn!(%gate_id, "inbound message denied");
                break 'agent_turn Some(make_denial_verdict(&mut denial_count, gate_id, reason));
            }
            Verdict::Approve {
                reason,
                gate_id,
                severity,
            } => {
                debug!(%gate_id, ?severity, "inbound approval required");
                let command = messages
                    .iter()
                    .rev()
                    .find_map(|message| {
                        message.content.iter().find_map(|block| match block {
                            MessageContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                    })
                    .unwrap_or("<inbound message>");
                let approved = approval_handler.request_approval(&severity, &reason, command);
                if !approved {
                    if !persisted_user_message {
                        let mut user_message = messages
                            .iter()
                            .rev()
                            .find(|message| message.role == crate::llm::ChatRole::User)
                            .cloned()
                            .ok_or_else(|| {
                                anyhow::anyhow!("missing user message after inbound checks")
                            })?;
                        crate::gate::guard_message_output(turn, &mut user_message);
                        session.append(user_message, None)?;
                    }
                    append_inbound_approval_denied(session, &gate_id)?;
                    warn!(%gate_id, "inbound approval denied");
                    break 'agent_turn Some(make_denial_verdict(
                        &mut denial_count,
                        gate_id,
                        reason,
                    ));
                }
                if !persisted_user_message {
                    let user_message = messages
                        .iter()
                        .rev()
                        .find(|message| message.role == crate::llm::ChatRole::User)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing user message after inbound checks")
                        })?;
                    session.append(user_message, None)?;
                    persisted_user_message = true;
                }
            }
        }

        if messages.is_empty() {
            continue;
        }

        if delegation::delegation_enabled(turn.delegation_config()) {
            match session.delegation_hint() {
                Ok(Some(delegation_hint)) => {
                    messages.push(ChatMessage::system(delegation_hint));
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "failed to load delegation hint");
                }
            }
        } else if let Err(err) = session.clear_delegation_hint() {
            warn!(error = %err, "failed to clear stale delegation hint");
        }

        let provider = make_provider().await?;
        debug!("provider created");
        let mut streaming_output = crate::gate::StreamingTextBuffer::new();
        let mut redact_text = |segment: String| crate::gate::guard_text_output(turn, segment);
        let mut emit_token = |token: String| token_sink.on_token(token);
        let mut turn_reply = {
            let mut on_token = |token: String| {
                streaming_output.push(&mut redact_text, &mut emit_token, token);
            };
            provider
                .stream_completion(&messages, &tools, &mut on_token)
                .await?
        };
        streaming_output.finish(&mut redact_text, &mut emit_token);
        crate::gate::guard_message_output(turn, &mut turn_reply.assistant_message);
        let turn_meta = turn_reply.meta;
        debug!(stop_reason = ?turn_reply.stop_reason, tool_call_count = turn_reply.tool_calls.len(), "streamed completion received");

        match turn_reply.stop_reason {
            StopReason::ToolCalls => {
                let tool_calls = turn_reply.tool_calls.clone();
                for call in &tool_calls {
                    debug!(call_id = %call.id, tool_name = %call.name, "evaluating tool call");
                    match turn.check_tool_call(call) {
                        Verdict::Allow => {}
                        Verdict::Modify => {}
                        Verdict::Deny { reason, gate_id } => {
                            persist_denied_assistant_text(
                                session,
                                turn,
                                turn_reply.assistant_message,
                                turn_meta,
                            )?;
                            append_hard_deny(session, &gate_id)?;
                            warn!(%gate_id, "tool call hard-denied");
                            break 'agent_turn Some(make_denial_verdict(
                                &mut denial_count,
                                gate_id,
                                reason,
                            ));
                        }
                        Verdict::Approve {
                            reason,
                            gate_id,
                            severity,
                        } => {
                            let command = command_from_tool_call(call)
                                .unwrap_or_else(|| "<command unavailable>".to_string());
                            let approved =
                                approval_handler.request_approval(&severity, &reason, &command);
                            if !approved {
                                persist_denied_assistant_text(
                                    session,
                                    turn,
                                    turn_reply.assistant_message,
                                    turn_meta,
                                )?;
                                append_approval_denied(session, &gate_id)?;
                                warn!(%gate_id, "tool call approval denied");
                                break 'agent_turn Some(make_denial_verdict(
                                    &mut denial_count,
                                    gate_id,
                                    reason,
                                ));
                            }
                            had_user_approval = true;
                        }
                    }
                }

                match turn.check_tool_batch(&tool_calls) {
                    Verdict::Allow => {}
                    Verdict::Modify => {}
                    Verdict::Deny { reason, gate_id } => {
                        persist_denied_assistant_text(
                            session,
                            turn,
                            turn_reply.assistant_message,
                            turn_meta,
                        )?;
                        append_hard_deny(session, &gate_id)?;
                        warn!(%gate_id, "tool batch hard-denied");
                        break 'agent_turn Some(make_denial_verdict(
                            &mut denial_count,
                            gate_id,
                            reason,
                        ));
                    }
                    Verdict::Approve {
                        reason,
                        gate_id,
                        severity,
                    } => {
                        let command = tool_calls
                            .first()
                            .and_then(command_from_tool_call)
                            .unwrap_or_else(|| "<command unavailable>".to_string());
                        if !approval_handler.request_approval(&severity, &reason, &command) {
                            persist_denied_assistant_text(
                                session,
                                turn,
                                turn_reply.assistant_message,
                                turn_meta,
                            )?;
                            append_approval_denied(session, &gate_id)?;
                            warn!(%gate_id, "tool batch approval denied");
                            break 'agent_turn Some(make_denial_verdict(
                                &mut denial_count,
                                gate_id,
                                reason,
                            ));
                        }
                        had_user_approval = true;
                    }
                }

                crate::gate::guard_message_output(turn, &mut turn_reply.assistant_message);
                session.append(turn_reply.assistant_message, turn_meta.clone())?;

                for call in &tool_calls {
                    debug!(call_id = %call.id, tool_name = %call.name, "executing tool");
                    let result = match turn.execute_tool(&call.name, &call.arguments).await {
                        Ok(output) => output,
                        Err(err) => format!(r#"{{"error": "{err}"}}"#),
                    };
                    let result = crate::gate::guard_text_output(turn, result);
                    let result = crate::gate::cap_tool_output(
                        session.sessions_dir(),
                        &call.id,
                        result,
                        crate::gate::DEFAULT_OUTPUT_CAP_BYTES,
                    )?;

                    session.append(
                        ChatMessage::tool_result_with_principal(
                            &call.id,
                            &call.name,
                            result,
                            Some(Principal::System),
                        ),
                        None,
                    )?;
                    executed.push(call.clone());
                }
                turn_tool_call_count += tool_calls.len();
            }

            StopReason::Stop => {
                session.append(turn_reply.assistant_message, turn_meta.clone())?;
                if let Err(err) = session.clear_delegation_hint() {
                    warn!(error = %err, "failed to clear delegation hint");
                }
                if let Err(err) = maybe_queue_delegation_hint(session, turn, turn_tool_call_count) {
                    warn!(error = %err, "failed to persist delegation hint");
                }
                token_sink.on_complete();
                if had_user_approval {
                    info!(
                        tool_call_count = executed.len(),
                        "agent turn completed after approval"
                    );
                    return Ok(TurnVerdict::Approved {
                        tool_calls: executed,
                    });
                }
                info!(tool_call_count = executed.len(), "agent turn completed");
                return Ok(TurnVerdict::Executed(executed));
            }
        }
    };

    denied_turn.context("agent loop exited without a terminal denial")
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
    process_queued_message(
        message,
        session,
        turn,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

pub async fn drain_queue<F, Fut, P>(
    store: &mut Store,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<Option<TurnVerdict>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    info!(%session_id, "draining queue");
    let mut processed_any = false;
    while let Some(message) = store.dequeue_next_message(session_id)? {
        processed_any = true;
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
                        info!(
                            message_id = message.id,
                            "command approved by user and executed"
                        );
                    }
                    TurnVerdict::Denied { reason, gate_id } => {
                        warn!(message_id = message.id, %gate_id, "turn denied");
                        return Ok(Some(TurnVerdict::Denied { reason, gate_id }));
                    }
                }
            }
            Ok(QueueOutcome::Stored) => {
                store.mark_processed(message.id)?;
            }
            Ok(QueueOutcome::UnsupportedRole(role)) => {
                warn!(message_id = message.id, %role, "unsupported queued role");
                store.mark_processed(message.id)?;
            }
            Err(error) => {
                store.mark_failed(message.id)?;
                warn!(message_id = message.id, %error, "failed processing queued message");
                return Err(error);
            }
        }
    }

    if crate::spawn::should_enqueue_child_completion(processed_any) {
        let _ = crate::spawn::enqueue_child_completion(store, session_id, session)?;
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::context::History;
    use crate::gate::{Guard, GuardEvent, SecretRedactor, ShellSafety};
    use crate::llm::{FunctionTool, StreamedTurn};
    use crate::principal::Principal;
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

    struct InboundDenyGuard;

    impl Guard for InboundDenyGuard {
        fn name(&self) -> &str {
            "inbound-deny"
        }

        fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
            match event {
                GuardEvent::Inbound(_) => Verdict::Deny {
                    reason: "blocked by test".to_string(),
                    gate_id: "inbound-deny".to_string(),
                },
                _ => Verdict::Allow,
            }
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

    struct RecordingTool {
        executions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        output: String,
    }

    impl RecordingTool {
        fn new(
            output: impl Into<String>,
        ) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
            let executions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            (
                Self {
                    executions: executions.clone(),
                    output: output.into(),
                },
                executions,
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
                description: "Record execution attempts".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                        },
                    },
                    "required": ["command"],
                    "additionalProperties": false,
                }),
            }
        }

        fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
            self.executions
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    #[derive(Clone)]
    struct RecordingProvider {
        assistant_text: String,
        observed_tools: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }

    impl crate::llm::LlmProvider for RecordingProvider {
        async fn stream_completion(
            &self,
            _messages: &[ChatMessage],
            tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.observed_tools
                .lock()
                .expect("tools mutex poisoned")
                .push(tools.iter().map(|tool| tool.name.clone()).collect());

            Ok(StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text(self.assistant_text.clone())],
                },
                tool_calls: vec![],
                meta: Some(crate::llm::TurnMeta {
                    model: Some("gpt-child".to_string()),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    reasoning_tokens: None,
                    reasoning_trace: None,
                }),
                stop_reason: StopReason::Stop,
            })
        }
    }

    #[derive(Clone)]
    struct MessageRecordingProvider {
        assistant_text: String,
        observed_system_texts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl crate::llm::LlmProvider for MessageRecordingProvider {
        async fn stream_completion(
            &self,
            messages: &[ChatMessage],
            tools: &[FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<StreamedTurn> {
            self.observed_system_texts
                .lock()
                .expect("system text mutex poisoned")
                .push(
                    messages
                        .iter()
                        .find(|message| message.role == crate::llm::ChatRole::System)
                        .map(|message| {
                            message
                                .content
                                .iter()
                                .filter_map(|block| match block {
                                    MessageContent::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                );

            let _ = tools;
            Ok(StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text(self.assistant_text.clone())],
                },
                tool_calls: vec![],
                meta: Some(crate::llm::TurnMeta {
                    model: Some("gpt-child".to_string()),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    reasoning_tokens: None,
                    reasoning_trace: None,
                }),
                stop_reason: StopReason::Stop,
            })
        }
    }

    fn spawned_t3_test_config(
        skills_dir: std::path::PathBuf,
        skills: crate::skills::SkillCatalog,
    ) -> crate::config::Config {
        crate::config::Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: skills_dir.clone(),
            skills_dir_resolved: skills_dir,
            skills,
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig::default(),
                        t2: crate::config::AgentTierConfig::default(),
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
                models.catalog.insert(
                    "gpt-child".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-child".to_string(),
                        caps: vec!["code_review".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("medium".to_string()),
                        cost_unit: Some(2),
                        enabled: Some(true),
                    },
                );
                models.routes.insert(
                    "code_review".to_string(),
                    crate::config::ModelRoute {
                        requires: vec!["code_review".to_string()],
                        prefer: vec!["gpt-child".to_string()],
                    },
                );
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        }
    }

    fn message_text(message: &ChatMessage) -> Option<&str> {
        message.content.iter().find_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    fn shell_policy(
        default: &str,
        allow_patterns: &[&str],
        deny_patterns: &[&str],
        standing_approvals: &[&str],
        default_severity: &str,
    ) -> crate::config::ShellPolicy {
        crate::config::ShellPolicy {
            default: default.to_string(),
            allow_patterns: allow_patterns
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            deny_patterns: deny_patterns
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            standing_approvals: standing_approvals
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            default_severity: default_severity.to_string(),
            max_output_bytes: crate::config::DEFAULT_SHELL_MAX_OUTPUT_BYTES,
            max_timeout_ms: crate::config::DEFAULT_SHELL_MAX_TIMEOUT_MS,
        }
    }

    fn streamed_turn_with_tool_call(
        text: Option<&str>,
        command: &str,
        call_id: &str,
    ) -> StreamedTurn {
        let mut content = Vec::new();
        if let Some(text) = text {
            content.push(MessageContent::text(text));
        }

        let call = ToolCall {
            id: call_id.to_string(),
            name: "execute".to_string(),
            arguments: serde_json::json!({ "command": command }).to_string(),
        };
        content.push(MessageContent::ToolCall { call: call.clone() });

        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content,
            },
            tool_calls: vec![call],
            meta: None,
            stop_reason: StopReason::ToolCalls,
        }
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
                            principal: Principal::Agent,
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

        assert!(
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
            .unwrap()
            .is_none()
        );

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
    async fn drain_queue_does_not_enqueue_completion_when_no_messages_were_processed() {
        let root = temp_queue_root("empty_queue");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();
        store
            .create_child_session("parent", "child", Some(r#"{"task":"noop"}"#))
            .unwrap();

        let mut session = Session::new(&sessions_dir).unwrap();
        let turn = Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: crate::llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![MessageContent::text("unused")],
                    },
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                },
            })
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        assert!(
            drain_queue(
                &mut store,
                "child",
                &mut session,
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let conn = Connection::open(&queue_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = 'parent'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn spawn_child_wrapper_enqueues_parent_completion_after_child_drain() {
        let root = temp_queue_root("child_completion");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let config = crate::config::Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: Vec::new(),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: crate::config::AgentsConfig::default(),
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-default".to_string());
                models.catalog.insert(
                    "gpt-default".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-default".to_string(),
                        caps: vec!["reasoning".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("cheap".to_string()),
                        cost_unit: Some(1),
                        enabled: Some(true),
                    },
                );
                models.catalog.insert(
                    "gpt-child".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-child".to_string(),
                        caps: vec!["code_review".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("medium".to_string()),
                        cost_unit: Some(2),
                        enabled: Some(true),
                    },
                );
                models.routes.insert(
                    "code_review".to_string(),
                    crate::config::ModelRoute {
                        requires: vec!["code_review".to_string()],
                        prefer: vec!["gpt-child".to_string()],
                    },
                );
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        };

        let parent_session = Session::new(sessions_dir.join("parent")).expect("parent session");
        let parent_budget = parent_session
            .budget_snapshot()
            .expect("parent budget snapshot");

        let spawn_result = spawn_child(
            &mut store,
            &config,
            parent_budget,
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t2".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("low".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(spawn_result.resolved_model, "gpt-child");

        let mut session = Session::new(sessions_dir.join(&spawn_result.child_session_id)).unwrap();
        let turn = Turn::new();
        let mut provider_factory = || async {
            Ok::<_, anyhow::Error>(StaticProvider {
                turn: StreamedTurn {
                    assistant_message: ChatMessage {
                        role: crate::llm::ChatRole::Assistant,
                        principal: Principal::Agent,
                        content: vec![MessageContent::text("child finished")],
                    },
                    tool_calls: vec![],
                    meta: Some(crate::llm::TurnMeta {
                        model: None,
                        input_tokens: None,
                        output_tokens: Some(1),
                        reasoning_tokens: None,
                        reasoning_trace: None,
                    }),
                    stop_reason: StopReason::Stop,
                },
            })
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        assert!(
            drain_queue(
                &mut store,
                &spawn_result.child_session_id,
                &mut session,
                &turn,
                &mut provider_factory,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap()
            .is_none()
        );

        let completion = store.dequeue_next_message("parent").unwrap().unwrap();
        assert_eq!(completion.role, "user");
        assert_eq!(
            completion.source,
            format!("agent-{}", spawn_result.child_session_id)
        );
        assert!(completion.content.contains("Child session"));
        assert!(completion.content.contains("child finished"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn spawn_and_drain_uses_child_runtime_config_and_returns_last_assistant_response() {
        use std::sync::{Arc, Mutex};

        let root = temp_queue_root("spawn_and_drain");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let config = crate::config::Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig {
                            delegation_token_threshold: Some(12_000),
                            delegation_tool_depth: Some(3),
                            ..Default::default()
                        },
                        t2: crate::config::AgentTierConfig {
                            model: Some("o3".to_string()),
                            reasoning: Some("high".to_string()),
                            ..Default::default()
                        },
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
                models.catalog.insert(
                    "gpt-child".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-child".to_string(),
                        caps: vec!["code_review".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("medium".to_string()),
                        cost_unit: Some(2),
                        enabled: Some(true),
                    },
                );
                models.routes.insert(
                    "code_review".to_string(),
                    crate::config::ModelRoute {
                        requires: vec!["code_review".to_string()],
                        prefer: vec!["gpt-child".to_string()],
                    },
                );
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        };

        let observed_models = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
        let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));

        let mut provider_factory = {
            let observed_models = observed_models.clone();
            let observed_tools = observed_tools.clone();
            move |child_config: &crate::config::Config| {
                observed_models
                    .lock()
                    .expect("models mutex poisoned")
                    .push((
                        child_config.model.clone(),
                        child_config.reasoning_effort.clone(),
                    ));
                let provider = RecordingProvider {
                    assistant_text: "child finished".to_string(),
                    observed_tools: observed_tools.clone(),
                };
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let result = spawn_and_drain_with_provider(
            &mut store,
            &config,
            &sessions_dir,
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t2".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                ..Default::default()
            },
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(result.resolved_model, "gpt-child");
        assert_eq!(
            result.last_assistant_response,
            Some("child finished".to_string())
        );
        assert_eq!(
            observed_models
                .lock()
                .expect("models mutex poisoned")
                .as_slice(),
            &[("gpt-child".to_string(), Some("high".to_string()))]
        );
        assert_eq!(
            observed_tools
                .lock()
                .expect("tools mutex poisoned")
                .as_slice(),
            &[vec!["read_file".to_string()]]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn spawn_and_drain_uses_t3_runtime_config_and_returns_last_assistant_response() {
        use std::sync::{Arc, Mutex};

        let root = temp_queue_root("spawn_and_drain_t3");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let config = crate::config::Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: None,
            operator_key: None,
            shell_policy: crate::config::ShellPolicy::default(),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig::default(),
                        t2: crate::config::AgentTierConfig::default(),
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
                models.catalog.insert(
                    "gpt-child".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-child".to_string(),
                        caps: vec!["code_review".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("medium".to_string()),
                        cost_unit: Some(2),
                        enabled: Some(true),
                    },
                );
                models.routes.insert(
                    "code_review".to_string(),
                    crate::config::ModelRoute {
                        requires: vec!["code_review".to_string()],
                        prefer: vec!["gpt-child".to_string()],
                    },
                );
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        };

        let observed_models = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
        let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));

        let mut provider_factory = {
            let observed_models = observed_models.clone();
            let observed_tools = observed_tools.clone();
            move |child_config: &crate::config::Config| {
                observed_models
                    .lock()
                    .expect("models mutex poisoned")
                    .push((
                        child_config.model.clone(),
                        child_config.reasoning_effort.clone(),
                    ));
                let provider = RecordingProvider {
                    assistant_text: "child finished".to_string(),
                    observed_tools: observed_tools.clone(),
                };
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let result = spawn_and_drain_with_provider(
            &mut store,
            &config,
            &sessions_dir,
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                ..Default::default()
            },
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(result.resolved_model, "gpt-child");
        assert_eq!(
            result.last_assistant_response,
            Some("child finished".to_string())
        );
        assert_eq!(
            observed_models
                .lock()
                .expect("models mutex poisoned")
                .as_slice(),
            &[("gpt-child".to_string(), Some("high".to_string()))]
        );
        assert_eq!(
            observed_tools
                .lock()
                .expect("tools mutex poisoned")
                .as_slice(),
            &[vec!["execute".to_string()]]
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn drain_spawned_t3_uses_persisted_skill_snapshot_not_catalog_lookup() {
        use std::sync::{Arc, Mutex};

        let root = temp_queue_root("spawned_t3_skill_snapshot");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        let skills_dir = root.join("skills");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        )
        .unwrap();

        let mut config = spawned_t3_test_config(
            skills_dir.clone(),
            crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap(),
        );

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let spawn_result = spawn_child(
            &mut store,
            &config,
            crate::gate::BudgetSnapshot::default(),
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                skills: vec!["code-review".to_string()],
                skill_token_budget: Some(2_000),
            },
        )
        .unwrap();

        std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Mutated instructions.'\n",
        )
        .unwrap();
        config.skills = crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap_or_default();

        let observed_system_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut provider_factory = {
            let observed_system_texts = observed_system_texts.clone();
            move |_child_config: &crate::config::Config| {
                let provider = MessageRecordingProvider {
                    assistant_text: "child finished".to_string(),
                    observed_system_texts: observed_system_texts.clone(),
                };
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let metadata_json = store
            .get_session_metadata(&spawn_result.child_session_id)
            .unwrap()
            .expect("child metadata should exist");
        let context = SpawnDrainContext {
            store: &mut store,
            config: &config,
            session_dir: &sessions_dir,
            spawn_result,
        };

        let result = finish_spawned_child_drain(
            context,
            &metadata_json,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(result.resolved_model, "gpt-child");
        let system_texts = observed_system_texts
            .lock()
            .expect("system text mutex poisoned");
        assert_eq!(system_texts.len(), 1);
        assert!(system_texts[0].contains("Skill: code-review"));
        assert!(system_texts[0].contains("Original instructions."));
        assert!(!system_texts[0].contains("Mutated instructions."));
        assert!(!system_texts[0].contains("Available skills:"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn drain_old_spawned_child_without_skills_metadata_still_runs() {
        use std::sync::{Arc, Mutex};

        let root = temp_queue_root("spawned_t3_old_metadata");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        let skills_dir = root.join("skills");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("code-review.toml"),
            "[skill]\nname='code-review'\ndescription='Reviews code changes'\nrequired_caps=['code_review']\ntoken_estimate=500\ninstructions='Original instructions.'\n",
        )
        .unwrap();

        let config = spawned_t3_test_config(
            skills_dir.clone(),
            crate::skills::SkillCatalog::load_from_dir(&skills_dir).unwrap(),
        );

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let spawn_result = spawn_child(
            &mut store,
            &config,
            crate::gate::BudgetSnapshot::default(),
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                skills: vec!["code-review".to_string()],
                skill_token_budget: Some(2_000),
            },
        )
        .unwrap();

        let mut metadata_value: Value = serde_json::from_str(
            &store
                .get_session_metadata(&spawn_result.child_session_id)
                .unwrap()
                .expect("child metadata should exist"),
        )
        .unwrap();
        metadata_value
            .as_object_mut()
            .expect("metadata should be an object")
            .remove("skills");
        let old_metadata_json = metadata_value.to_string();

        let observed_system_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut provider_factory = {
            let observed_system_texts = observed_system_texts.clone();
            move |_child_config: &crate::config::Config| {
                let provider = MessageRecordingProvider {
                    assistant_text: "child finished".to_string(),
                    observed_system_texts: observed_system_texts.clone(),
                };
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let context = SpawnDrainContext {
            store: &mut store,
            config: &config,
            session_dir: &sessions_dir,
            spawn_result,
        };

        let result = finish_spawned_child_drain(
            context,
            &old_metadata_json,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(result.resolved_model, "gpt-child");
        let system_texts = observed_system_texts
            .lock()
            .expect("system text mutex poisoned");
        assert_eq!(system_texts.len(), 1);
        assert!(!system_texts[0].contains("Skill: code-review"));
        assert!(!system_texts[0].contains("Available skills:"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn spawn_and_drain_invokes_approval_handler_for_t3_shell_calls() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct ApprovalGateProvider {
            call_index: Arc<AtomicUsize>,
        }

        impl crate::llm::LlmProvider for ApprovalGateProvider {
            async fn stream_completion(
                &self,
                _messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                match self.call_index.fetch_add(1, Ordering::SeqCst) {
                    0 => Ok(streamed_turn_with_tool_call(
                        Some("requesting approval"),
                        "true",
                        "call-1",
                    )),
                    _ => Ok(StreamedTurn {
                        assistant_message: ChatMessage {
                            role: crate::llm::ChatRole::Assistant,
                            principal: Principal::Agent,
                            content: vec![MessageContent::text("approval handled")],
                        },
                        tool_calls: vec![],
                        meta: Some(crate::llm::TurnMeta {
                            model: Some("gpt-child".to_string()),
                            input_tokens: Some(1),
                            output_tokens: Some(1),
                            reasoning_tokens: None,
                            reasoning_trace: None,
                        }),
                        stop_reason: StopReason::Stop,
                    }),
                }
            }
        }

        let root = temp_queue_root("spawn_and_drain_approval");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();

        let mut config = crate::config::Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: Some("medium".to_string()),
            session_name: None,
            operator_key: None,
            shell_policy: shell_policy("approve", &[], &[], &[], "medium"),
            budget: None,
            read: crate::config::ReadToolConfig::default(),
            queue: crate::config::QueueConfig::default(),
            identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
            skills_dir: std::path::PathBuf::from("skills"),
            skills_dir_resolved: std::path::PathBuf::from("skills"),
            skills: crate::skills::SkillCatalog::default(),
            agents: {
                let mut agents = crate::config::AgentsConfig::default();
                agents.entries.insert(
                    "silas".to_string(),
                    crate::config::AgentDefinition {
                        identity: Some("silas".to_string()),
                        tier: None,
                        model: None,
                        base_url: None,
                        system_prompt: None,
                        session_name: None,
                        reasoning_effort: None,
                        t1: crate::config::AgentTierConfig::default(),
                        t2: crate::config::AgentTierConfig::default(),
                    },
                );
                agents
            },
            models: {
                let mut models = crate::config::ModelsConfig::default();
                models.default = Some("gpt-child".to_string());
                models.catalog.insert(
                    "gpt-child".to_string(),
                    crate::config::ModelDefinition {
                        provider: "openai".to_string(),
                        model: "gpt-child".to_string(),
                        caps: vec!["code_review".to_string()],
                        context_window: Some(128_000),
                        cost_tier: Some("medium".to_string()),
                        cost_unit: Some(2),
                        enabled: Some(true),
                    },
                );
                models.routes.insert(
                    "code_review".to_string(),
                    crate::config::ModelRoute {
                        requires: vec!["code_review".to_string()],
                        prefer: vec!["gpt-child".to_string()],
                    },
                );
                models
            },
            domains: Default::default(),
            active_agent: Some("silas".to_string()),
        };
        config.agents.entries.get_mut("silas").unwrap().tier = Some("t3".to_string());

        let approval_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let mut provider_factory = {
            let call_index = Arc::new(AtomicUsize::new(0));
            let observed_calls = observed_calls.clone();
            move |_child_config: &crate::config::Config| {
                let provider = ApprovalGateProvider {
                    call_index: call_index.clone(),
                };
                let observed_calls = observed_calls.clone();
                async move {
                    observed_calls
                        .lock()
                        .expect("calls mutex poisoned")
                        .push(vec!["execute".to_string()]);
                    Ok::<_, anyhow::Error>(provider)
                }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| {
            approval_calls.fetch_add(1, Ordering::SeqCst);
            true
        };

        let result = spawn_and_drain_with_provider(
            &mut store,
            &config,
            &sessions_dir,
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t3".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                ..Default::default()
            },
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(
            result.last_assistant_response,
            Some("approval handled".to_string())
        );
        assert!(approval_calls.load(Ordering::SeqCst) > 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn spawn_and_drain_rejects_invalid_persisted_tier() {
        use std::sync::{Arc, Mutex};

        let root = temp_queue_root("spawn_and_drain_bad_tier");
        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let mut store = Store::new(&queue_path).unwrap();
        store.create_session("parent", None).unwrap();
        let spawn_result = spawn_child(
            &mut store,
            &crate::config::Config {
                model: "gpt-test".to_string(),
                system_prompt: "system".to_string(),
                base_url: "https://example.test/api".to_string(),
                reasoning_effort: None,
                session_name: None,
                operator_key: None,
                shell_policy: crate::config::ShellPolicy::default(),
                budget: None,
                read: crate::config::ReadToolConfig::default(),
                queue: crate::config::QueueConfig::default(),
                identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
                skills_dir: std::path::PathBuf::from("skills"),
                skills_dir_resolved: std::path::PathBuf::from("skills"),
                skills: crate::skills::SkillCatalog::default(),
                agents: {
                    let mut agents = crate::config::AgentsConfig::default();
                    agents.entries.insert(
                        "silas".to_string(),
                        crate::config::AgentDefinition {
                            identity: Some("silas".to_string()),
                            tier: None,
                            model: None,
                            base_url: None,
                            system_prompt: None,
                            session_name: None,
                            reasoning_effort: None,
                            t1: crate::config::AgentTierConfig::default(),
                            t2: crate::config::AgentTierConfig::default(),
                        },
                    );
                    agents
                },
                models: {
                    let mut models = crate::config::ModelsConfig::default();
                    models.default = Some("gpt-child".to_string());
                    models.catalog.insert(
                        "gpt-child".to_string(),
                        crate::config::ModelDefinition {
                            provider: "openai".to_string(),
                            model: "gpt-child".to_string(),
                            caps: vec!["code_review".to_string()],
                            context_window: Some(128_000),
                            cost_tier: Some("medium".to_string()),
                            cost_unit: Some(2),
                            enabled: Some(true),
                        },
                    );
                    models.routes.insert(
                        "code_review".to_string(),
                        crate::config::ModelRoute {
                            requires: vec!["code_review".to_string()],
                            prefer: vec!["gpt-child".to_string()],
                        },
                    );
                    models
                },
                domains: Default::default(),
                active_agent: Some("silas".to_string()),
            },
            crate::gate::BudgetSnapshot::default(),
            SpawnRequest {
                parent_session_id: "parent".to_string(),
                task: "child task".to_string(),
                task_kind: Some("code_review".to_string()),
                tier: Some("t2".to_string()),
                model_override: Some("gpt-child".to_string()),
                reasoning_override: Some("high".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let bad_metadata = serde_json::json!({
            "parent_session_id": "parent",
            "task": "child task",
            "task_kind": "code_review",
            "tier": "bogus",
            "model_override": "gpt-child",
            "reasoning_override": "high",
            "resolved_model": spawn_result.resolved_model,
            "resolved_provider_model": "gpt-child",
        })
        .to_string();

        let observed_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let mut provider_factory = {
            let observed_tools = observed_tools.clone();
            move |_child_config: &crate::config::Config| {
                let provider = RecordingProvider {
                    assistant_text: "child finished".to_string(),
                    observed_tools: observed_tools.clone(),
                };
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let context = SpawnDrainContext {
            store: &mut store,
            config: &crate::config::Config {
                model: "gpt-test".to_string(),
                system_prompt: "system".to_string(),
                base_url: "https://example.test/api".to_string(),
                reasoning_effort: None,
                session_name: None,
                operator_key: None,
                shell_policy: crate::config::ShellPolicy::default(),
                budget: None,
                read: crate::config::ReadToolConfig::default(),
                queue: crate::config::QueueConfig::default(),
                identity_files: crate::identity::t1_identity_files("identity-templates", "silas"),
                skills_dir: std::path::PathBuf::from("skills"),
                skills_dir_resolved: std::path::PathBuf::from("skills"),
                skills: crate::skills::SkillCatalog::default(),
                agents: {
                    let mut agents = crate::config::AgentsConfig::default();
                    agents.entries.insert(
                        "silas".to_string(),
                        crate::config::AgentDefinition {
                            identity: Some("silas".to_string()),
                            tier: None,
                            model: None,
                            base_url: None,
                            system_prompt: None,
                            session_name: None,
                            reasoning_effort: None,
                            t1: crate::config::AgentTierConfig::default(),
                            t2: crate::config::AgentTierConfig::default(),
                        },
                    );
                    agents
                },
                models: {
                    let mut models = crate::config::ModelsConfig::default();
                    models.default = Some("gpt-child".to_string());
                    models.catalog.insert(
                        "gpt-child".to_string(),
                        crate::config::ModelDefinition {
                            provider: "openai".to_string(),
                            model: "gpt-child".to_string(),
                            caps: vec!["code_review".to_string()],
                            context_window: Some(128_000),
                            cost_tier: Some("medium".to_string()),
                            cost_unit: Some(2),
                            enabled: Some(true),
                        },
                    );
                    models.routes.insert(
                        "code_review".to_string(),
                        crate::config::ModelRoute {
                            requires: vec!["code_review".to_string()],
                            prefer: vec!["gpt-child".to_string()],
                        },
                    );
                    models
                },
                domains: Default::default(),
                active_agent: Some("silas".to_string()),
            },
            session_dir: &sessions_dir,
            spawn_result,
        };

        let error = finish_spawned_child_drain(
            context,
            &bad_metadata,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .expect_err("invalid persisted tier should fail");

        assert!(error.to_string().contains("invalid child tier"));
        assert!(
            observed_tools
                .lock()
                .expect("tools mutex poisoned")
                .is_empty(),
            "provider should never be created for invalid metadata"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
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
            Principal::Operator,
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
    async fn delegation_hint_is_appended_after_successful_turn() {
        let dir = temp_sessions_dir("delegation_hint");
        let (provider, _observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .append(
                ChatMessage::user("seed delegation context"),
                Some(crate::llm::TurnMeta {
                    input_tokens: Some(8),
                    ..Default::default()
                }),
            )
            .unwrap();

        let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
            token_threshold: Some(0),
            tool_depth_threshold: None,
        });
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "please keep this short".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(verdict, TurnVerdict::Executed(_)));
        assert_eq!(
            session.delegation_hint().unwrap().as_deref(),
            Some(crate::delegation::DELEGATION_HINT)
        );
        let reloaded_session = crate::session::Session::new(&dir).unwrap();
        assert_eq!(
            reloaded_session.delegation_hint().unwrap().as_deref(),
            Some(crate::delegation::DELEGATION_HINT)
        );
        assert!(!session.history().iter().any(|message| {
            matches!(message.role, crate::llm::ChatRole::System)
                && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
        }));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn delegation_hint_is_retained_when_provider_fails() {
        use std::sync::{Arc, Mutex};

        let dir = temp_sessions_dir("delegation_hint_failed_provider");
        let observed_hint = Arc::new(Mutex::new(false));

        #[derive(Clone)]
        struct FailingProvider {
            observed_hint: Arc<Mutex<bool>>,
        }

        impl crate::llm::LlmProvider for FailingProvider {
            async fn stream_completion(
                &self,
                messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Err(anyhow::anyhow!("provider failure"))
            }
        }

        let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
            token_threshold: Some(u64::MAX),
            tool_depth_threshold: None,
        });
        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
            .unwrap();
        let mut make_provider = {
            let provider = FailingProvider {
                observed_hint: observed_hint.clone(),
            };
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let error = match run_agent_loop(
            &mut make_provider,
            &mut session,
            "keep going".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            Ok(_) => panic!("provider failure should bubble up"),
            Err(err) => err,
        };

        assert!(
            *observed_hint
                .lock()
                .expect("hint mutex should not be poisoned")
        );
        assert_eq!(
            session.delegation_hint().unwrap().as_deref(),
            Some(crate::delegation::DELEGATION_HINT)
        );
        assert!(error.to_string().contains("provider failure"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn delegation_hint_is_ignored_when_delegation_is_disabled() {
        use std::sync::{Arc, Mutex};

        let dir = temp_sessions_dir("delegation_hint_disabled");
        let observed_hint = Arc::new(Mutex::new(false));

        #[derive(Clone)]
        struct HintObservingProvider {
            observed_hint: Arc<Mutex<bool>>,
        }

        impl crate::llm::LlmProvider for HintObservingProvider {
            async fn stream_completion(
                &self,
                messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Ok(StreamedTurn {
                    assistant_message: ChatMessage::system("ok"),
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                })
            }
        }

        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
            .unwrap();
        let turn = Turn::new();
        let mut make_provider = {
            let provider = HintObservingProvider {
                observed_hint: observed_hint.clone(),
            };
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
            "keep going".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(
            !*observed_hint
                .lock()
                .expect("hint mutex should not be poisoned")
        );
        assert!(session.delegation_hint().unwrap().is_none());
        let reloaded_session = crate::session::Session::new(&dir).unwrap();
        assert!(reloaded_session.delegation_hint().unwrap().is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn delegation_hint_is_cleared_after_successful_turn_without_new_advice() {
        use std::sync::{Arc, Mutex};

        let dir = temp_sessions_dir("delegation_hint_cleared");
        let observed_hint = Arc::new(Mutex::new(false));

        #[derive(Clone)]
        struct HintObservingProvider {
            observed_hint: Arc<Mutex<bool>>,
        }

        impl crate::llm::LlmProvider for HintObservingProvider {
            async fn stream_completion(
                &self,
                messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                *self
                    .observed_hint
                    .lock()
                    .expect("hint mutex should not be poisoned") = saw_hint;

                Ok(StreamedTurn {
                    assistant_message: ChatMessage::system("ok"),
                    tool_calls: vec![],
                    meta: None,
                    stop_reason: StopReason::Stop,
                })
            }
        }

        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
            .unwrap();
        let turn = Turn::new().delegation(crate::delegation::DelegationConfig {
            token_threshold: Some(u64::MAX),
            tool_depth_threshold: None,
        });
        let mut make_provider = {
            let provider = HintObservingProvider {
                observed_hint: observed_hint.clone(),
            };
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
            "keep going".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(
            *observed_hint
                .lock()
                .expect("hint mutex should not be poisoned")
        );
        assert!(session.delegation_hint().unwrap().is_none());
        let reloaded_session = crate::session::Session::new(&dir).unwrap();
        assert!(reloaded_session.delegation_hint().unwrap().is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn delegation_hint_accumulates_tool_calls_across_batches() {
        use std::sync::{Arc, Mutex};

        let dir = temp_sessions_dir("delegation_tool_batches");
        let observed_hints = Arc::new(Mutex::new(Vec::new()));

        #[derive(Clone)]
        struct HintObservingSequenceProvider {
            observed_hints: Arc<Mutex<Vec<bool>>>,
            call_index: Arc<Mutex<usize>>,
        }

        impl crate::llm::LlmProvider for HintObservingSequenceProvider {
            async fn stream_completion(
                &self,
                messages: &[ChatMessage],
                _tools: &[FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> Result<StreamedTurn> {
                let saw_hint = messages.iter().any(|message| {
                    matches!(message.role, crate::llm::ChatRole::System)
                        && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
                });
                self.observed_hints
                    .lock()
                    .expect("hint mutex should not be poisoned")
                    .push(saw_hint);

                let mut call_index = self
                    .call_index
                    .lock()
                    .expect("call index mutex should not be poisoned");
                let turn = match *call_index {
                    0 => StreamedTurn {
                        assistant_message: ChatMessage::system("batch one"),
                        tool_calls: vec![
                            ToolCall {
                                id: "call-1".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                            ToolCall {
                                id: "call-2".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                        ],
                        meta: None,
                        stop_reason: StopReason::ToolCalls,
                    },
                    1 => StreamedTurn {
                        assistant_message: ChatMessage::system("batch two"),
                        tool_calls: vec![
                            ToolCall {
                                id: "call-3".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                            ToolCall {
                                id: "call-4".to_string(),
                                name: "execute".to_string(),
                                arguments: r#"{"command":"true"}"#.to_string(),
                            },
                        ],
                        meta: None,
                        stop_reason: StopReason::ToolCalls,
                    },
                    _ => StreamedTurn {
                        assistant_message: ChatMessage::system("done"),
                        tool_calls: vec![],
                        meta: None,
                        stop_reason: StopReason::Stop,
                    },
                };
                *call_index += 1;
                Ok(turn)
            }
        }

        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .queue_delegation_hint(crate::delegation::DELEGATION_HINT)
            .unwrap();

        let turn = Turn::new()
            .tool(Shell::new())
            .delegation(crate::delegation::DelegationConfig {
                token_threshold: None,
                tool_depth_threshold: Some(3),
            });
        let mut make_provider = {
            let provider = HintObservingSequenceProvider {
                observed_hints: observed_hints.clone(),
                call_index: Arc::new(Mutex::new(0)),
            };
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "run the tools".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(verdict, TurnVerdict::Executed(_)));
        assert_eq!(
            observed_hints
                .lock()
                .expect("hint mutex should not be poisoned")
                .as_slice(),
            &[true, true, true]
        );
        assert_eq!(
            session.delegation_hint().unwrap().as_deref(),
            Some(crate::delegation::DELEGATION_HINT)
        );
        let reloaded_session = crate::session::Session::new(&dir).unwrap();
        assert_eq!(
            reloaded_session.delegation_hint().unwrap().as_deref(),
            Some(crate::delegation::DELEGATION_HINT)
        );
        assert!(!session.history().iter().any(|message| {
            matches!(message.role, crate::llm::ChatRole::System)
                && message.content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == crate::delegation::DELEGATION_HINT))
        }));

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
            Principal::Operator,
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
    async fn inbound_denial_returns_denied_without_looping() {
        let dir = temp_sessions_dir("inbound_denial");
        let (provider, observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();

        let turn = Turn::new().guard(InboundDenyGuard);
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "blocked prompt".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "blocked by test" && gate_id == "inbound-deny"
        ));
        assert!(
            observed_message_counts
                .lock()
                .expect("observed message count mutex poisoned")
                .is_empty()
        );

        let stored = session.history();
        assert_eq!(stored.len(), 2);
        assert!(matches!(stored[1].role, crate::llm::ChatRole::Assistant));
        assert_eq!(stored[1].principal, Principal::System);
        let note = match &stored[1].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert_eq!(note, "Message hard-denied by inbound-deny");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn approval_denial_audit_is_not_system_role_or_raw_command() {
        let dir = temp_sessions_dir("approval_denial_audit");
        let provider = SequenceProvider::new(vec![StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::ToolCall {
                    call: ToolCall {
                        id: "call-1".to_string(),
                        name: "leak".to_string(),
                        arguments: serde_json::json!({"command":"reject marker command"})
                            .to_string(),
                    },
                }],
            },
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "leak".to_string(),
                arguments: serde_json::json!({"command":"reject marker command"}).to_string(),
            }],
            meta: None,
            stop_reason: StopReason::ToolCalls,
        }]);
        let mut session = crate::session::Session::new(&dir).unwrap();

        struct RedactingApproval;

        impl Guard for RedactingApproval {
            fn name(&self) -> &str {
                "redacting-approval"
            }

            fn check(
                &self,
                event: &mut GuardEvent,
                _context: &crate::gate::GuardContext,
            ) -> Verdict {
                match event {
                    GuardEvent::ToolCall(_) => Verdict::Approve {
                        reason: "danger".to_string(),
                        gate_id: "needs-approval".to_string(),
                        severity: Severity::High,
                    },
                    _ => Verdict::Allow,
                }
            }
        }

        let turn = Turn::new().guard(RedactingApproval);
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "reject marker command".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "danger" && gate_id == "needs-approval"
        ));

        let stored = session.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        let text = match &stored[1].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert!(text.is_empty());
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        let note = match &stored[2].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert_eq!(
            note,
            "Tool execution rejected after approval by needs-approval"
        );
        assert!(!note.contains("reject marker command"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn context_insertion_does_not_replace_persisted_user_message() {
        let dir = temp_sessions_dir("persist_user_with_context");
        let identity_dir = dir.join("identity");
        std::fs::create_dir_all(identity_dir.join("agents/silas")).unwrap();
        std::fs::write(identity_dir.join("constitution.md"), "constitution").unwrap();
        std::fs::write(identity_dir.join("agents/silas/agent.md"), "You are Silas.").unwrap();
        std::fs::write(identity_dir.join("context.md"), "context").unwrap();

        let (provider, _observed_message_counts) = InspectingProvider::new();
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().context(crate::context::Identity::new(
            crate::identity::t1_identity_files(&identity_dir, "silas"),
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
            Principal::Operator,
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

    #[test]
    fn max_denial_counter_returns_summary_after_threshold() {
        let mut denial_count = 0usize;

        let first = make_denial_verdict(
            &mut denial_count,
            "guard-1".to_string(),
            "first denial".to_string(),
        );
        assert!(matches!(
            first,
            TurnVerdict::Denied { ref reason, ref gate_id }
                if reason == "first denial" && gate_id == "guard-1"
        ));

        let second = make_denial_verdict(
            &mut denial_count,
            "guard-2".to_string(),
            "second denial".to_string(),
        );
        match second {
            TurnVerdict::Denied { reason, gate_id } => {
                assert_eq!(gate_id, "guard-2");
                assert!(reason.contains("stopped after 2 denied actions this turn"));
                assert!(reason.contains("last denial by guard-2: second denial"));
            }
            _ => panic!("expected denied verdict"),
        }

        assert_eq!(denial_count, 2);
    }

    #[tokio::test]
    async fn inbound_approval_denial_audit_is_not_system_role_or_raw_command() {
        let dir = temp_sessions_dir("inbound_approval_denial_audit");
        let provider = SequenceProvider::new(vec![StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::text("unused")],
            },
            tool_calls: vec![],
            meta: None,
            stop_reason: StopReason::Stop,
        }]);
        let mut session = crate::session::Session::new(&dir).unwrap();

        struct NeedsApproval;

        impl Guard for NeedsApproval {
            fn name(&self) -> &str {
                "needs-approval"
            }

            fn check(
                &self,
                event: &mut GuardEvent,
                _context: &crate::gate::GuardContext,
            ) -> Verdict {
                match event {
                    GuardEvent::Inbound(_) => Verdict::Approve {
                        reason: "danger".to_string(),
                        gate_id: "needs-approval".to_string(),
                        severity: Severity::High,
                    },
                    _ => Verdict::Allow,
                }
            }
        }

        let turn = Turn::new()
            .guard(NeedsApproval)
            .guard(crate::gate::SecretRedactor::default_catalog());
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "sk-1234567890abcdef1234567890abcdef1234567890abcdef".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "danger" && gate_id == "needs-approval"
        ));

        let stored = session.history();
        assert_eq!(stored.len(), 2);
        let persisted_user = &stored[0];
        assert_eq!(persisted_user.role, crate::llm::ChatRole::User);
        let redacted_text = persisted_user
            .content
            .iter()
            .find_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .expect("expected redacted user text");
        assert!(redacted_text.contains("[REDACTED]"));
        assert!(!redacted_text.contains("sk-"));
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::System);
        let note = match &stored[1].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert_eq!(note, "Message rejected after approval by needs-approval");
        assert!(!note.contains("sk-"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn hard_deny_audit_is_not_system_role_or_raw_command() {
        let dir = temp_sessions_dir("hard_deny_audit");
        let provider = SequenceProvider::new(vec![StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::ToolCall {
                    call: ToolCall {
                        id: "call-1".to_string(),
                        name: "leak".to_string(),
                        arguments: serde_json::json!({"command":"hard deny marker"}).to_string(),
                    },
                }],
            },
            tool_calls: vec![ToolCall {
                id: "call-1".to_string(),
                name: "leak".to_string(),
                arguments: serde_json::json!({"command":"hard deny marker"}).to_string(),
            }],
            meta: None,
            stop_reason: StopReason::ToolCalls,
        }]);
        let mut session = crate::session::Session::new(&dir).unwrap();

        struct HardDeny;

        impl Guard for HardDeny {
            fn name(&self) -> &str {
                "hard-deny"
            }

            fn check(
                &self,
                event: &mut GuardEvent,
                _context: &crate::gate::GuardContext,
            ) -> Verdict {
                match event {
                    GuardEvent::ToolCall(_) => Verdict::Deny {
                        reason: "blocked".to_string(),
                        gate_id: "hard-deny".to_string(),
                    },
                    _ => Verdict::Allow,
                }
            }
        }

        let turn = Turn::new().guard(HardDeny);
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "hard deny marker".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "blocked" && gate_id == "hard-deny"
        ));

        let stored = session.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        let text = match &stored[1].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert!(text.is_empty());
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        let note = match &stored[2].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert_eq!(note, "Tool execution hard-denied by hard-deny");
        assert!(!note.contains("hard deny marker"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn denied_tool_calls_are_not_persisted_without_tool_results() {
        let dir = temp_sessions_dir("denied_tool_calls");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            None, "ls /tmp", "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let (tool, executions) = RecordingTool::new("marker-output");
        let turn = Turn::new().tool(tool).guard(ShellSafety::new());
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "deny tool call".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "shell command `ls /tmp` did not match any allowlist pattern"
                    && gate_id == "shell-policy"
        ));
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);

        let stored = session.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[0].role, crate::llm::ChatRole::User);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        let text = match &stored[1].content[0] {
            MessageContent::Text { text } => text,
            _ => panic!("expected text audit note"),
        };
        assert!(text.is_empty());
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        assert!(
            stored[2]
                .content
                .iter()
                .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
        );
        assert!(
            !std::fs::read_to_string(session.today_path())
                .unwrap()
                .contains("marker-output")
        );
        assert!(!session.sessions_dir().join("results").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn denied_tool_calls_reload_with_placeholder_and_audit_note() {
        let dir = temp_sessions_dir("denied_tool_calls_reload");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            None, "ls /tmp", "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let (tool, _executions) = RecordingTool::new("marker-output");
        let turn = Turn::new().tool(tool).guard(ShellSafety::new());
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "deny tool call".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(verdict, TurnVerdict::Denied { .. }));

        let mut reloaded = crate::session::Session::new(&dir).unwrap();
        reloaded.load_today().unwrap();
        let stored = reloaded.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        assert!(
            stored[2]
                .content
                .iter()
                .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn denied_mixed_content_assistant_message_keeps_text_but_drops_tool_calls() {
        let dir = temp_sessions_dir("denied_mixed_content");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            Some("safe assistant text"),
            "ls /tmp",
            "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().guard(ShellSafety::new());
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| false;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "deny mixed content".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "shell command `ls /tmp` did not match any allowlist pattern"
                    && gate_id == "shell-policy"
        ));

        let stored = session.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        let text = message_text(&stored[1]).expect("expected persisted assistant text");
        assert_eq!(text, "safe assistant text");
        assert!(
            stored[1]
                .content
                .iter()
                .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
        );
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        let note = message_text(&stored[2]).expect("expected audit note");
        assert_eq!(
            note,
            "Tool execution rejected after approval by shell-policy"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn protected_path_denial_writes_no_raw_tool_output_to_jsonl_or_results_dir() {
        let dir = temp_sessions_dir("protected_path_no_output");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            None,
            "cat ~/.autopoiesis/auth.json",
            "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let (tool, executions) = RecordingTool::new("marker-output-raw");
        let turn = Turn::new()
            .tool(tool)
            .guard(ShellSafety::with_policy(shell_policy(
                "approve",
                &["cat *"],
                &[],
                &[],
                "medium",
            )));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "read protected path".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason.contains("reads protected credential path")
                    && gate_id == "shell-policy"
        ));
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);

        let session_file = std::fs::read_to_string(session.today_path()).unwrap();
        assert!(!session_file.contains("marker-output-raw"));
        assert!(!session.sessions_dir().join("results").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn protected_path_denial_persists_only_safe_audit_material() {
        let dir = temp_sessions_dir("protected_path_audit");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            Some("safe protected-path assistant text"),
            "cat ~/.autopoiesis/auth.json",
            "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new().guard(ShellSafety::with_policy(shell_policy(
            "approve",
            &["cat *"],
            &[],
            &[],
            "medium",
        )));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "read protected material".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason.contains("reads protected credential path")
                    && gate_id == "shell-policy"
        ));

        let stored = session.history();
        assert_eq!(stored.len(), 3);
        assert_eq!(stored[1].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[1].principal, Principal::Agent);
        let text = message_text(&stored[1]).expect("expected persisted assistant text");
        assert_eq!(text, "safe protected-path assistant text");
        assert!(
            stored[1]
                .content
                .iter()
                .all(|block| !matches!(block, MessageContent::ToolCall { .. }))
        );
        assert_eq!(stored[2].role, crate::llm::ChatRole::Assistant);
        assert_eq!(stored[2].principal, Principal::System);
        let note = message_text(&stored[2]).expect("expected audit note");
        assert_eq!(note, "Tool execution hard-denied by shell-policy");
        assert!(!note.contains("auth.json"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn metacharacter_smuggling_under_allowlisted_prefix_requires_approval() {
        let dir = temp_sessions_dir("metacharacter_smuggling");
        let provider = SequenceProvider::new(vec![streamed_turn_with_tool_call(
            None,
            "cat /tmp/input.txt; echo smuggled",
            "call-1",
        )]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let (tool, executions) = RecordingTool::new("marker-output-smuggled");
        let turn = Turn::new()
            .tool(tool)
            .guard(ShellSafety::with_policy(shell_policy(
                "approve",
                &["cat *"],
                &[],
                &[],
                "medium",
            )));
        let mut make_provider = {
            let provider = provider.clone();
            move || {
                let provider = provider.clone();
                async move { Ok::<_, anyhow::Error>(provider) }
            }
        };
        let approval_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let approval_count_seen = approval_count.clone();
        let mut token_sink = |_token: String| {};
        let mut approval_handler = move |_severity: &Severity, _reason: &str, _command: &str| {
            approval_count_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        };

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "smuggle metacharacter".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(
            verdict,
            TurnVerdict::Denied { reason, gate_id }
                if reason == "compound shell command requires explicit approval"
                    && gate_id == "shell-policy"
        ));
        assert_eq!(approval_count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(session.history().len(), 3);
        assert!(!session.sessions_dir().join("results").exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn truncated_shell_output_remains_explicit_in_session_pointer_and_result_file() {
        let dir = temp_sessions_dir("truncated_shell_output");
        let call_id = "call-truncated";
        let max_output_bytes = 5_000;
        let command = "printf '%9000s' ''";
        let provider = SequenceProvider::new(vec![
            streamed_turn_with_tool_call(None, command, call_id),
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text("done")],
                },
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            },
        ]);
        let mut session = crate::session::Session::new(&dir).unwrap();
        let turn = Turn::new()
            .tool(Shell::with_max_output_bytes(max_output_bytes))
            .guard(ShellSafety::with_policy(shell_policy(
                "allow",
                &[],
                &[],
                &[],
                "medium",
            )));
        let mut make_provider = move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

        let verdict = run_agent_loop(
            &mut make_provider,
            &mut session,
            "run bounded output test".to_string(),
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(verdict, TurnVerdict::Executed(ref calls) if calls.len() == 1));

        let tool_message = session
            .history()
            .iter()
            .find(|message| message.role == crate::llm::ChatRole::Tool)
            .expect("tool result should be persisted");
        let pointer = tool_message
            .content
            .iter()
            .find_map(|block| match block {
                MessageContent::ToolResult { result } => Some(result.content.as_str()),
                _ => None,
            })
            .expect("tool result should have pointer text");
        let result_path = session.sessions_dir().join("results").join(format!(
            "{}.txt",
            crate::gate::output_cap::safe_call_id_for_filename(call_id)
        ));
        let result_path_str = result_path.display().to_string();

        assert!(pointer.contains("bounded capture"));
        assert!(pointer.contains(&result_path_str));
        assert!(pointer.contains("output exceeded inline limit"));

        let persisted = std::fs::read_to_string(&result_path).unwrap();
        assert!(persisted.contains(&crate::tool::shell_output_truncation_note(max_output_bytes)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn tool_output_is_redacted_before_persist() {
        let dir = temp_sessions_dir("tool_redaction");
        let provider = SequenceProvider::new(vec![
            StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
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
                    principal: Principal::Agent,
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
            Principal::Operator,
            &turn,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let session_file = std::fs::read_to_string(session.today_path()).unwrap();
        assert!(!session_file.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"));
        assert!(session_file.contains("[REDACTED]"));

        let tool_message = session
            .history()
            .iter()
            .find(|message| message.role == crate::llm::ChatRole::Tool)
            .expect("tool message should be persisted");
        assert_eq!(tool_message.principal, Principal::System);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn replayed_tool_result_marks_followup_turn_tainted() {
        let dir = temp_sessions_dir("replayed_tool_taint");
        let mut session = crate::session::Session::new(&dir).unwrap();
        session
            .append(
                ChatMessage::tool_result_with_principal(
                    "call-1",
                    "execute",
                    "stdout:\nok",
                    Some(Principal::System),
                ),
                None,
            )
            .unwrap();

        let turn = Turn::new();
        let mut messages = session.history().to_vec();

        let verdict = turn.check_inbound(&mut messages, None);
        assert!(matches!(verdict, Verdict::Allow | Verdict::Modify));
        assert!(turn.is_tainted());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    mod regression_tests {
        use super::*;
        use crate::llm::{ChatRole, StreamedTurn};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        };

        #[derive(Clone)]
        struct ApprovalProbeGuard;

        impl Guard for ApprovalProbeGuard {
            fn name(&self) -> &str {
                "approval-probe"
            }

            fn check(
                &self,
                event: &mut GuardEvent,
                _context: &crate::gate::GuardContext,
            ) -> Verdict {
                match event {
                    GuardEvent::Inbound(_) => Verdict::Approve {
                        reason: "probe".to_string(),
                        gate_id: "approval-probe".to_string(),
                        severity: crate::gate::Severity::High,
                    },
                    _ => Verdict::Allow,
                }
            }
        }

        #[derive(Clone)]
        struct PanicProvider;

        impl crate::llm::LlmProvider for PanicProvider {
            async fn stream_completion(
                &self,
                _messages: &[crate::llm::ChatMessage],
                _tools: &[crate::llm::FunctionTool],
                _on_token: &mut (dyn FnMut(String) + Send),
            ) -> anyhow::Result<StreamedTurn> {
                panic!("provider should not be called when inbound approval is denied");
            }
        }

        #[tokio::test]
        async fn inbound_approval_prompt_forwards_user_message_text() {
            let dir = temp_sessions_dir("inbound_approval_prompt");
            let mut session = crate::session::Session::new(&dir).unwrap();
            session
                .append(
                    ChatMessage {
                        role: ChatRole::System,
                        principal: Principal::System,
                        content: vec![MessageContent::text("system prompt sentinel")],
                    },
                    None,
                )
                .unwrap();

            let turn = Turn::new().guard(ApprovalProbeGuard);
            let seen_command = Arc::new(Mutex::new(None));
            let mut make_provider = || async { Ok::<_, anyhow::Error>(PanicProvider) };
            let mut token_sink = |_token: String| {};
            let seen_command_for_handler = seen_command.clone();
            let mut approval_handler = move |_severity: &Severity, _reason: &str, command: &str| {
                *seen_command_for_handler.lock().unwrap() = Some(command.to_string());
                false
            };

            let verdict = run_agent_loop(
                &mut make_provider,
                &mut session,
                "actual user message".to_string(),
                Principal::Operator,
                &turn,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap();

            assert!(matches!(verdict, TurnVerdict::Denied { .. }));
            let command = seen_command.lock().unwrap().clone().unwrap();
            assert!(command.ends_with("actual user message"));
            assert!(!command.contains("system prompt sentinel"));

            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[tokio::test]
        async fn budget_ceiling_is_enforced_on_the_next_turn() {
            let dir = temp_sessions_dir("budget_next_turn");
            let config_path = dir.join("agents.toml");
            std::fs::write(
                &config_path,
                r#"
[agents.silas]
identity = "silas"

[agents.silas.t1]
model = "gpt-budget"

[budget]
max_tokens_per_turn = 10
max_tokens_per_session = 10
max_tokens_per_day = 10
"#,
            )
            .unwrap();

            let config = crate::config::Config::load(&config_path).unwrap();
            let turn = crate::turn::build_turn_for_config(&config);
            let mut session = crate::session::Session::new(&dir).unwrap();
            let provider_calls = Arc::new(AtomicUsize::new(0));
            let provider_turn = StreamedTurn {
                assistant_message: crate::llm::ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![crate::llm::MessageContent::text("ok")],
                },
                tool_calls: Vec::new(),
                meta: Some(crate::llm::TurnMeta {
                    model: Some("gpt-budget".to_string()),
                    input_tokens: Some(1),
                    output_tokens: Some(20),
                    reasoning_tokens: None,
                    reasoning_trace: None,
                }),
                stop_reason: crate::llm::StopReason::Stop,
            };

            #[derive(Clone)]
            struct CountingProvider {
                calls: Arc<AtomicUsize>,
                turn: StreamedTurn,
            }

            impl crate::llm::LlmProvider for CountingProvider {
                async fn stream_completion(
                    &self,
                    _messages: &[crate::llm::ChatMessage],
                    _tools: &[crate::llm::FunctionTool],
                    _on_token: &mut (dyn FnMut(String) + Send),
                ) -> anyhow::Result<StreamedTurn> {
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    Ok(self.turn.clone())
                }
            }

            let provider = CountingProvider {
                calls: provider_calls.clone(),
                turn: provider_turn,
            };
            let mut make_provider = {
                let provider = provider.clone();
                move || {
                    let provider = provider.clone();
                    async move { Ok::<_, anyhow::Error>(provider) }
                }
            };
            let mut token_sink = |_token: String| {};
            let mut approval_handler = |_severity: &Severity, _reason: &str, _command: &str| true;

            let first = run_agent_loop(
                &mut make_provider,
                &mut session,
                "first turn".to_string(),
                Principal::Operator,
                &turn,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap();
            assert!(matches!(first, TurnVerdict::Executed(_)));

            let live_snapshot = session.budget_snapshot().unwrap();
            assert!(live_snapshot.turn_tokens > 10);

            let second = run_agent_loop(
                &mut make_provider,
                &mut session,
                "second turn".to_string(),
                Principal::Operator,
                &turn,
                &mut token_sink,
                &mut approval_handler,
            )
            .await
            .unwrap();

            assert!(matches!(
                second,
                TurnVerdict::Denied {
                    gate_id,
                    ..
                } if gate_id == "budget"
            ));
            assert_eq!(provider_calls.load(Ordering::SeqCst), 1);

            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
