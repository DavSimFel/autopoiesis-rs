//! Queue claim and delivery processing.

use anyhow::Result;

use crate::llm::LlmProvider;
use crate::session::Session;
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;

use super::QueueOutcome;
use super::{ApprovalHandler, TokenSink, TurnVerdict};
use crate::session_runtime::drain::{self, StoreDrainBackend};

#[tracing::instrument(
    level = "debug",
    skip(message, session, turn, make_provider, token_sink, approval_handler),
    fields(message_id = message.id, session_id = %message.session_id, role = %message.role)
)]
pub(crate) async fn process_queued_message<F, Fut, P, TS, AH>(
    message: &QueuedMessage,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<QueueOutcome>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + Send + ?Sized,
{
    let observer = crate::observe::runtime_observer(session.sessions_dir());
    drain::process_queued_message(
        message,
        session,
        turn,
        observer,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

#[tracing::instrument(
    level = "debug",
    skip(message, session, turn_builder, make_provider, token_sink, approval_handler),
    fields(message_id = message.id, session_id = %message.session_id, role = %message.role)
)]
pub(crate) async fn process_queued_message_with_turn_builder<F, Fut, P, TS, AH, TB>(
    message: &QueuedMessage,
    session: &mut Session,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<QueueOutcome>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + Send + ?Sized,
    TB: FnMut() -> Result<Turn> + Send,
{
    let observer = crate::observe::runtime_observer(session.sessions_dir());
    drain::process_queued_message_with_turn_builder(
        message,
        session,
        turn_builder,
        observer,
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
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    let mut backend = StoreDrainBackend::new(store);
    drain::drain_queue(
        &mut backend,
        session_id,
        session,
        turn,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
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
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TB: FnMut() -> Result<Turn> + Send,
{
    let mut backend = StoreDrainBackend::new(store);
    drain::drain_queue_with_stats_fresh_turns(
        &mut backend,
        session_id,
        session,
        turn_builder,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

#[cfg(test)]
mod tests;
