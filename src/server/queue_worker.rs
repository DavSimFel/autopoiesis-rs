use anyhow::Context;
use tracing::warn;

use crate::session_runtime::{
    build_openai_provider_factory, build_turn_builder_for_subscriptions,
    drain_queue_with_shared_store, load_subscriptions_for_session,
};
use crate::{agent, llm, session, turn};

use super::ServerState;
use super::session_lock::SessionLockLease;

#[tracing::instrument(level = "debug", skip(state, turn_builder, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
pub(super) async fn drain_session_queue_with_turn_builder<F, Fut, P, TS, AH, TB>(
    state: ServerState,
    session_id: String,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> anyhow::Result<Option<agent::TurnVerdict>>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<P>> + Send,
    P: llm::LlmProvider + Send,
    TS: agent::TokenSink + Send,
    AH: agent::ApprovalHandler + Send,
    TB: FnMut() -> anyhow::Result<turn::Turn> + Send,
{
    let session_lock = state.session_lock(&session_id);
    let _session_lock_lease = SessionLockLease::new(
        state.clone(),
        session_id.clone(),
        std::sync::Arc::downgrade(&session_lock),
    );
    let _session_guard = session_lock.lock().await;

    let mut history = session::Session::new(state.sessions_dir.join(&session_id))
        .with_context(|| format!("failed to open session {session_id}"))?;
    history.load_today()?;
    let (verdict, _, _) = drain_queue_with_shared_store(
        state.store.clone(),
        &session_id,
        &mut history,
        turn_builder,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await?;
    Ok(verdict)
}

pub(super) async fn drain_session_queue_with_subscriptions<TS, AH>(
    state: ServerState,
    session_id: String,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> anyhow::Result<Option<agent::TurnVerdict>>
where
    TS: agent::TokenSink + Send,
    AH: agent::ApprovalHandler + Send,
{
    let subscriptions = {
        let mut store = state.store.lock().await;
        load_subscriptions_for_session(&mut store, &session_id).with_context(|| {
            format!("failed to load subscriptions for queue turn for {session_id}")
        })?
    };
    let mut turn_builder =
        build_turn_builder_for_subscriptions(state.config.clone(), subscriptions);
    let mut provider_factory =
        build_openai_provider_factory(state.http_client.clone(), state.config.clone());
    drain_session_queue_with_turn_builder(
        state,
        session_id,
        &mut turn_builder,
        &mut provider_factory,
        token_sink,
        approval_handler,
    )
    .await
}

pub(super) fn spawn_background_queue_worker(state: ServerState, session_id: String) {
    tokio::spawn(async move {
        let mut token_sink = NoopTokenSink;
        let mut approval_handler = RejectApprovalHandler;
        match drain_session_queue_with_subscriptions(
            state.clone(),
            session_id.clone(),
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        {
            Ok(Some(verdict)) => match verdict {
                agent::TurnVerdict::Denied { reason: _, gate_id } => {
                    warn!(%gate_id, "http turn denied");
                }
                _ => unreachable!("drain_queue only returns denial verdicts"),
            },
            Ok(None) => {}
            Err(error) => {
                warn!(%session_id, %error, "failed to drain queued HTTP messages");
            }
        }
    });
}

struct NoopTokenSink;

impl agent::TokenSink for NoopTokenSink {
    fn on_token(&mut self, _token: String) {}
}

struct RejectApprovalHandler;

impl agent::ApprovalHandler for RejectApprovalHandler {
    fn request_approval(
        &mut self,
        _severity: &crate::gate::Severity,
        _reason: &str,
        _command: &str,
    ) -> bool {
        false
    }
}
