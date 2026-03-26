//! Queue claim and delivery processing.

use anyhow::Result;

use crate::llm::{ChatMessage, LlmProvider, MessageContent};
use crate::principal::Principal;
use crate::session::Session;
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;
use tracing::{info, warn};

use super::loop_impl::run_agent_loop;
use super::{ApprovalHandler, QueueOutcome, TokenSink, TurnVerdict};
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

pub(crate) async fn process_queued_message_with_turn_builder<F, Fut, P, TS, AH, TB>(
    message: &QueuedMessage,
    session: &mut Session,
    turn_builder: &mut TB,
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
    TB: FnMut() -> Result<Turn>,
{
    match message.role.as_str() {
        "user" => {
            let turn = turn_builder()?;
            Ok(QueueOutcome::Agent(
                run_agent_loop(
                    make_provider,
                    session,
                    message.content.clone(),
                    Principal::from_source(&message.source),
                    &turn,
                    token_sink,
                    approval_handler,
                )
                .await?,
            ))
        }
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
    let (verdict, _processed_any, _last_assistant_response) = drain_queue_with_stats(
        store,
        session_id,
        session,
        turn,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await?;
    Ok(verdict)
}

pub(crate) async fn drain_queue_with_stats<F, Fut, P>(
    store: &mut Store,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
{
    info!(%session_id, "draining queue");
    let mut completed_agent_turn = false;
    let mut first_denial: Option<TurnVerdict> = None;
    let mut last_assistant_response = None;
    while let Some(message) = store.dequeue_next_message(session_id)? {
        let outcome = process_queued_message(
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
                if !matches!(verdict, TurnVerdict::Denied { .. }) {
                    last_assistant_response = crate::spawn::latest_assistant_response(session);
                    completed_agent_turn = true;
                }
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
                        if first_denial.is_none() {
                            first_denial = Some(TurnVerdict::Denied { reason, gate_id });
                        }
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

    if crate::spawn::should_enqueue_child_completion(completed_agent_turn) {
        let _ = crate::spawn::enqueue_child_completion(
            store,
            session_id,
            session,
            last_assistant_response.as_deref(),
        )?;
    }

    let verdict = if completed_agent_turn {
        None
    } else {
        first_denial
    };

    Ok((verdict, completed_agent_turn, last_assistant_response))
}

pub(crate) async fn drain_queue_with_stats_fresh_turns<F, Fut, P, TB>(
    store: &mut Store,
    session_id: &str,
    session: &mut Session,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TB: FnMut() -> Result<Turn>,
{
    info!(%session_id, "draining queue");
    let mut completed_agent_turn = false;
    let mut first_denial: Option<TurnVerdict> = None;
    let mut last_assistant_response = None;
    while let Some(message) = store.dequeue_next_message(session_id)? {
        let outcome = process_queued_message_with_turn_builder(
            &message,
            session,
            turn_builder,
            make_provider,
            token_sink,
            approval_handler,
        )
        .await;
        match outcome {
            Ok(QueueOutcome::Agent(verdict)) => {
                store.mark_processed(message.id)?;
                if !matches!(verdict, TurnVerdict::Denied { .. }) {
                    last_assistant_response = crate::spawn::latest_assistant_response(session);
                    completed_agent_turn = true;
                }
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
                        if first_denial.is_none() {
                            first_denial = Some(TurnVerdict::Denied { reason, gate_id });
                        }
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

    if crate::spawn::should_enqueue_child_completion(completed_agent_turn) {
        let _ = crate::spawn::enqueue_child_completion(
            store,
            session_id,
            session,
            last_assistant_response.as_deref(),
        )?;
    }

    let verdict = if completed_agent_turn {
        None
    } else {
        first_denial
    };

    Ok((verdict, completed_agent_turn, last_assistant_response))
}

#[cfg(test)]
mod tests;
