//! Agent loop and denial/audit helpers.

use anyhow::{Context, Result};
use serde_json::{Value, from_str};

use crate::delegation::{self, DelegationAdvice};
use crate::gate::{GuardContext, Verdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall, TurnMeta};
use crate::principal::Principal;
use crate::session::Session;
use crate::turn::Turn;
use crate::util::utc_timestamp;
use tracing::{debug, info, warn};

use super::{ApprovalHandler, TokenSink};

const MAX_DENIALS_PER_TURN: usize = 2;

/// Agent verdict returned after processing a queued message or turn.
pub enum TurnVerdict {
    Executed(Vec<ToolCall>),
    Denied { reason: String, gate_id: String },
    Approved { tool_calls: Vec<ToolCall> },
}

/// Outcome returned when draining a queued message.
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

pub(super) fn make_denial_verdict(
    denial_count: &mut usize,
    gate_id: String,
    reason: String,
) -> TurnVerdict {
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

#[cfg(test)]
mod tests;
