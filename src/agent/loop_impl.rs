//! Agent loop and denial/audit helpers.

use anyhow::{Context, Result};
use serde_json::{Value, from_str};

use crate::delegation::{self, DelegationAdvice};
use crate::gate::{GuardContext, Verdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent, StopReason, ToolCall};
use crate::principal::Principal;
use crate::session::Session;
use crate::turn::Turn;
use crate::util::utc_timestamp;
use tracing::{debug, info, warn};

use super::TurnVerdict;
use super::audit::{
    append_denial_note_for_inbound, append_denial_note_for_inbound_approval,
    append_denial_note_for_tool_approval, append_denial_note_for_tool_deny, make_denial_verdict,
    persist_denied_assistant_text,
};
use super::usage::{charged_turn_meta, flush_buffered_tokens, post_turn_budget_denial};
use super::{ApprovalHandler, TokenSink};

fn command_from_tool_call(call: &ToolCall) -> Option<String> {
    let value = from_str::<Value>(&call.arguments).ok()?;
    value
        .get("command")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn inbound_verdict_name(verdict: &crate::gate::Verdict) -> &'static str {
    match verdict {
        crate::gate::Verdict::Allow => "allow",
        crate::gate::Verdict::Modify => "modify",
        crate::gate::Verdict::Deny { .. } => "deny",
        crate::gate::Verdict::Approve { .. } => "approve",
    }
}

fn persisted_inbound_user_message(
    messages: &[ChatMessage],
    persisted_user_message_index: Option<usize>,
) -> Option<ChatMessage> {
    persisted_user_message_index.and_then(|index| {
        messages
            .iter()
            .skip(index)
            .find(|message| message.role == crate::llm::ChatRole::User)
            .cloned()
    })
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
    let user_message = ChatMessage::user_with_principal(user_prompt.clone(), Some(user_principal));
    let mut persisted_user_message = false;
    let mut persisted_user_message_index = None;

    let mut executed: Vec<ToolCall> = Vec::new();
    let mut had_user_approval = false;
    let mut denial_count = 0usize;
    let mut turn_tool_call_count = 0usize;
    let mut buffered_tokens: Vec<String> = Vec::new();
    info!("starting agent turn");

    let denied_turn = 'agent_turn: loop {
        session.ensure_context_within_limit();
        let mut messages = session.history().to_vec();
        if !persisted_user_message {
            persisted_user_message_index = Some(messages.len());
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
                    let user_message =
                        persisted_inbound_user_message(&messages, persisted_user_message_index)
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
                    let mut user_message =
                        persisted_inbound_user_message(&messages, persisted_user_message_index)
                            .ok_or_else(|| {
                                anyhow::anyhow!("missing user message after inbound checks")
                            })?;
                    crate::gate::guard_message_output(turn, &mut user_message);
                    session.append(user_message, None)?;
                }
                append_denial_note_for_inbound(session, &gate_id)?;
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
                        let mut user_message =
                            persisted_inbound_user_message(&messages, persisted_user_message_index)
                                .ok_or_else(|| {
                                    anyhow::anyhow!("missing user message after inbound checks")
                                })?;
                        crate::gate::guard_message_output(turn, &mut user_message);
                        session.append(user_message, None)?;
                    }
                    append_denial_note_for_inbound_approval(session, &gate_id)?;
                    warn!(%gate_id, "inbound approval denied");
                    break 'agent_turn Some(make_denial_verdict(
                        &mut denial_count,
                        gate_id,
                        reason,
                    ));
                }
                if !persisted_user_message {
                    let user_message =
                        persisted_inbound_user_message(&messages, persisted_user_message_index)
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
        let should_buffer_tokens = turn.needs_budget_context();
        let mut emit_token = |token: String| {
            if should_buffer_tokens {
                buffered_tokens.push(token);
            } else {
                token_sink.on_token(token);
            }
        };
        let mut turn_reply = {
            let mut on_token = |token: String| {
                streaming_output.push(&mut redact_text, &mut emit_token, token);
            };
            match provider
                .stream_completion(&messages, &tools, &mut on_token)
                .await
            {
                Ok(turn_reply) => turn_reply,
                Err(err) => {
                    streaming_output.finish(&mut redact_text, &mut emit_token);
                    if should_buffer_tokens {
                        flush_buffered_tokens(token_sink, &mut buffered_tokens);
                    }
                    return Err(err);
                }
            }
        };
        streaming_output.finish(&mut redact_text, &mut emit_token);
        crate::gate::guard_message_output(turn, &mut turn_reply.assistant_message);
        let turn_meta = turn_reply.meta;
        let charged_meta = charged_turn_meta(turn_meta.clone(), &turn_reply.assistant_message);
        if let Some((gate_id, reason)) = post_turn_budget_denial(
            turn,
            session,
            &turn_reply.assistant_message,
            turn_meta.as_ref(),
        )? {
            let mut denied_message = turn_reply.assistant_message.clone();
            denied_message.content.clear();
            session.append(denied_message, Some(charged_meta.clone()))?;
            token_sink.on_complete();
            return Ok(make_denial_verdict(&mut denial_count, gate_id, reason));
        }
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
                                Some(charged_meta.clone()),
                            )?;
                            append_denial_note_for_tool_deny(session, &gate_id)?;
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
                                    Some(charged_meta.clone()),
                                )?;
                                append_denial_note_for_tool_approval(session, &gate_id)?;
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
                            Some(charged_meta.clone()),
                        )?;
                        append_denial_note_for_tool_deny(session, &gate_id)?;
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
                                Some(charged_meta.clone()),
                            )?;
                            append_denial_note_for_tool_approval(session, &gate_id)?;
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
                session.append(turn_reply.assistant_message, Some(charged_meta.clone()))?;

                for call in &tool_calls {
                    debug!(call_id = %call.id, tool_name = %call.name, "executing tool");
                    let result = if call.name == "execute" {
                        crate::agent::shell_execute::guarded_shell_execute_prechecked(
                            turn, call, session,
                        )
                        .await?
                        .output
                    } else {
                        let result = match turn.execute_tool(&call.name, &call.arguments).await {
                            Ok(output) => output,
                            Err(err) => serde_json::json!({ "error": err.to_string() }).to_string(),
                        };
                        let result = crate::gate::guard_text_output(turn, result);
                        crate::gate::cap_tool_output(
                            session.sessions_dir(),
                            &call.id,
                            result,
                            crate::gate::DEFAULT_OUTPUT_CAP_BYTES,
                        )?
                    };

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
                session.append(turn_reply.assistant_message, Some(charged_meta))?;
                if let Err(err) = session.clear_delegation_hint() {
                    warn!(error = %err, "failed to clear delegation hint");
                }
                if let Err(err) = maybe_queue_delegation_hint(session, turn, turn_tool_call_count) {
                    warn!(error = %err, "failed to persist delegation hint");
                }
                if should_buffer_tokens {
                    flush_buffered_tokens(token_sink, &mut buffered_tokens);
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
