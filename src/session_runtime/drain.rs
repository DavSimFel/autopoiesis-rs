//! Shared queue-drain state machine for CLI, HTTP, and spawned sessions.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::agent::{ApprovalHandler, QueueOutcome, TokenSink, TurnVerdict};
use crate::llm::{ChatMessage, LlmProvider, MessageContent};
use crate::principal::Principal;
use crate::session::Session;
use crate::spawn;
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;

/// Queue operations needed by the shared drain state machine.
pub(crate) trait DrainBackend {
    fn dequeue_next_message<'a>(
        &'a mut self,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<QueuedMessage>>> + Send + 'a>>;
    fn mark_processed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
    fn mark_failed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
    fn enqueue_child_completion<'a>(
        &'a mut self,
        session_id: &'a str,
        session: &'a Session,
        last_assistant_response: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// Direct `Store` backend used by CLI and spawned-session code paths.
pub(crate) struct StoreDrainBackend<'a> {
    store: &'a mut Store,
}

impl<'a> StoreDrainBackend<'a> {
    pub(crate) fn new(store: &'a mut Store) -> Self {
        Self { store }
    }
}

impl DrainBackend for StoreDrainBackend<'_> {
    fn dequeue_next_message<'a>(
        &'a mut self,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<QueuedMessage>>> + Send + 'a>> {
        Box::pin(async move { self.store.dequeue_next_message(session_id) })
    }

    fn mark_processed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { self.store.mark_processed(message_id) })
    }

    fn mark_failed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { self.store.mark_failed(message_id) })
    }

    fn enqueue_child_completion<'a>(
        &'a mut self,
        session_id: &'a str,
        session: &'a Session,
        last_assistant_response: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            crate::spawn::enqueue_child_completion(
                self.store,
                session_id,
                session,
                last_assistant_response,
            )?;
            Ok(())
        })
    }
}

/// Shared `Arc<Mutex<Store>>` backend used by the HTTP server.
pub(crate) struct SharedStoreDrainBackend {
    store: Arc<Mutex<Store>>,
}

impl SharedStoreDrainBackend {
    pub(crate) fn new(store: Arc<Mutex<Store>>) -> Self {
        Self { store }
    }
}

impl DrainBackend for SharedStoreDrainBackend {
    fn dequeue_next_message<'a>(
        &'a mut self,
        session_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<QueuedMessage>>> + Send + 'a>> {
        Box::pin(async move {
            let mut guard = self.store.lock().await;
            guard.dequeue_next_message(session_id)
        })
    }

    fn mark_processed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut guard = self.store.lock().await;
            guard.mark_processed(message_id)
        })
    }

    fn mark_failed<'a>(
        &'a mut self,
        message_id: i64,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut guard = self.store.lock().await;
            guard.mark_failed(message_id)
        })
    }

    fn enqueue_child_completion<'a>(
        &'a mut self,
        session_id: &'a str,
        session: &'a Session,
        last_assistant_response: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut guard = self.store.lock().await;
            crate::spawn::enqueue_child_completion(
                &mut guard,
                session_id,
                session,
                last_assistant_response,
            )
            .map(|_| ())
        })
    }
}

trait DrainProcessor {
    fn process<'a>(
        &'a mut self,
        message: &'a QueuedMessage,
        session: &'a mut Session,
    ) -> Pin<Box<dyn Future<Output = Result<QueueOutcome>> + Send + 'a>>;
}

struct FixedTurnProcessor<'a, F, Fut, P> {
    turn: &'a Turn,
    make_provider: &'a mut F,
    token_sink: &'a mut (dyn TokenSink + Send),
    approval_handler: &'a mut (dyn ApprovalHandler + Send),
    _marker: std::marker::PhantomData<(Fut, P)>,
}

impl<'a, F, Fut, P> FixedTurnProcessor<'a, F, Fut, P> {
    fn new(
        turn: &'a Turn,
        make_provider: &'a mut F,
        token_sink: &'a mut (dyn TokenSink + Send),
        approval_handler: &'a mut (dyn ApprovalHandler + Send),
    ) -> Self {
        Self {
            turn,
            make_provider,
            token_sink,
            approval_handler,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a, F, Fut, P> DrainProcessor for FixedTurnProcessor<'a, F, Fut, P>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    fn process<'b>(
        &'b mut self,
        message: &'b QueuedMessage,
        session: &'b mut Session,
    ) -> Pin<Box<dyn Future<Output = Result<QueueOutcome>> + Send + 'b>> {
        Box::pin(process_queued_message(
            message,
            session,
            self.turn,
            self.make_provider,
            self.token_sink,
            self.approval_handler,
        ))
    }
}

struct FreshTurnProcessor<'a, F, Fut, P, TB> {
    turn_builder: &'a mut TB,
    make_provider: &'a mut F,
    token_sink: &'a mut (dyn TokenSink + Send),
    approval_handler: &'a mut (dyn ApprovalHandler + Send),
    _marker: std::marker::PhantomData<(Fut, P)>,
}

impl<'a, F, Fut, P, TB> FreshTurnProcessor<'a, F, Fut, P, TB> {
    fn new(
        turn_builder: &'a mut TB,
        make_provider: &'a mut F,
        token_sink: &'a mut (dyn TokenSink + Send),
        approval_handler: &'a mut (dyn ApprovalHandler + Send),
    ) -> Self {
        Self {
            turn_builder,
            make_provider,
            token_sink,
            approval_handler,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<'a, F, Fut, P, TB> DrainProcessor for FreshTurnProcessor<'a, F, Fut, P, TB>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TB: FnMut() -> Result<Turn> + Send,
{
    fn process<'b>(
        &'b mut self,
        message: &'b QueuedMessage,
        session: &'b mut Session,
    ) -> Pin<Box<dyn Future<Output = Result<QueueOutcome>> + Send + 'b>> {
        Box::pin(process_queued_message_with_turn_builder(
            message,
            session,
            self.turn_builder,
            self.make_provider,
            self.token_sink,
            self.approval_handler,
        ))
    }
}

async fn drain_queue_state_machine<B, P>(
    backend: &mut B,
    session_id: &str,
    session: &mut Session,
    mut process: P,
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    B: DrainBackend,
    P: DrainProcessor,
{
    info!(%session_id, "draining queue");
    let mut completed_agent_turn = false;
    let mut first_denial: Option<TurnVerdict> = None;
    let mut last_assistant_response = None;

    // Queue state machine invariant: each claimed row is always resolved to processed or failed,
    // and completion messages are only emitted after at least one non-denied agent turn.
    while let Some(message) = backend.dequeue_next_message(session_id).await? {
        let outcome = process.process(&message, session).await;
        match outcome {
            Ok(QueueOutcome::Agent(verdict)) => {
                backend.mark_processed(message.id).await?;
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
                backend.mark_processed(message.id).await?;
            }
            Ok(QueueOutcome::UnsupportedRole(role)) => {
                warn!(message_id = message.id, %role, "unsupported queued role");
                backend.mark_processed(message.id).await?;
            }
            Err(error) => {
                backend.mark_failed(message.id).await?;
                warn!(message_id = message.id, %error, "failed processing queued message");
                return Err(error);
            }
        }
    }

    if spawn::should_enqueue_child_completion(completed_agent_turn) {
        backend
            .enqueue_child_completion(session_id, session, last_assistant_response.as_deref())
            .await?;
    }

    let verdict = if completed_agent_turn {
        None
    } else {
        first_denial
    };

    Ok((verdict, completed_agent_turn, last_assistant_response))
}

#[tracing::instrument(level = "debug", skip(make_provider, session, turn, token_sink, approval_handler, backend), fields(session_id = %session_id))]
pub(crate) async fn drain_queue_with_stats<B, F, Fut, P>(
    backend: &mut B,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    B: DrainBackend,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    drain_queue_state_machine(
        backend,
        session_id,
        session,
        FixedTurnProcessor::new(turn, make_provider, token_sink, approval_handler),
    )
    .await
}

pub(crate) async fn drain_queue<B, F, Fut, P>(
    backend: &mut B,
    session_id: &str,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<Option<TurnVerdict>>
where
    B: DrainBackend,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    let (verdict, _processed_any, _last_assistant_response) = drain_queue_with_stats(
        backend,
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

#[tracing::instrument(level = "debug", skip(backend, session, turn_builder, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
pub(crate) async fn drain_queue_with_stats_fresh_turns<B, F, Fut, P, TB>(
    backend: &mut B,
    session_id: &str,
    session: &mut Session,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    B: DrainBackend,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TB: FnMut() -> Result<Turn> + Send,
{
    drain_queue_state_machine(
        backend,
        session_id,
        session,
        FreshTurnProcessor::new(turn_builder, make_provider, token_sink, approval_handler),
    )
    .await
}

pub async fn drain_queue_with_store<F, Fut, P, TB>(
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
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TB: FnMut() -> Result<Turn> + Send,
{
    let mut backend = StoreDrainBackend::new(store);
    drain_queue_with_stats_fresh_turns(
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

pub async fn drain_queue_with_shared_store<F, Fut, P, TB>(
    store: Arc<Mutex<Store>>,
    session_id: &str,
    session: &mut Session,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut (dyn TokenSink + Send),
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<(Option<TurnVerdict>, bool, Option<String>)>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TB: FnMut() -> Result<Turn> + Send,
{
    let mut backend = SharedStoreDrainBackend::new(store);
    drain_queue_with_stats_fresh_turns(
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
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + Send + ?Sized,
{
    match message.role.as_str() {
        "user" => Ok(QueueOutcome::Agent(
            crate::agent::run_agent_loop(
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
    Fut: Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + Send + ?Sized,
    TB: FnMut() -> Result<Turn> + Send,
{
    match message.role.as_str() {
        "user" => {
            let turn = turn_builder()?;
            Ok(QueueOutcome::Agent(
                crate::agent::run_agent_loop(
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
