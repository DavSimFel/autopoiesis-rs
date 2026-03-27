use std::time::Duration;

use anyhow::Context;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::warn;

use crate::context::SessionManifest;
use crate::session_runtime::{
    build_openai_provider_factory, build_turn_builder_for_subscriptions_with_manifest,
    drain_queue_with_shared_store, load_subscriptions_for_session,
};
use crate::{agent, llm, session, turn};

use super::ServerState;
use super::session_lock::SessionLockLease;

#[tracing::instrument(level = "debug", skip(state, turn_builder, make_provider, token_sink, approval_handler), fields(session_id = %session_id))]
async fn drain_session_queue_with_turn_builder_locked<F, Fut, P, TS, AH, TB>(
    state: ServerState,
    session_id: String,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> anyhow::Result<(Option<agent::TurnVerdict>, bool)>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<P>> + Send,
    P: llm::LlmProvider + Send,
    TS: agent::TokenSink + Send,
    AH: agent::ApprovalHandler + Send,
    TB: FnMut() -> anyhow::Result<turn::Turn> + Send,
{
    let mut history = session::Session::new(state.sessions_dir.join(&session_id))
        .with_context(|| format!("failed to open session {session_id}"))?;
    history.load_today()?;
    let (verdict, processed_any, _) = drain_queue_with_shared_store(
        state.store.clone(),
        &session_id,
        &mut history,
        turn_builder,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await?;
    Ok((verdict, processed_any))
}

pub(super) async fn drain_session_queue_with_turn_builder<F, Fut, P, TS, AH, TB>(
    state: ServerState,
    session_id: String,
    turn_builder: &mut TB,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
    pause_while_websocket_active: bool,
) -> anyhow::Result<(Option<agent::TurnVerdict>, bool)>
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
    if pause_while_websocket_active && state.always_on_websocket_count(&session_id) > 0 {
        return Ok((None, false));
    }
    drain_session_queue_with_turn_builder_locked(
        state,
        session_id,
        turn_builder,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

pub(super) async fn drain_session_queue_with_subscriptions_locked<TS, AH>(
    state: ServerState,
    session_id: String,
    config: crate::config::Config,
    session_manifest: Option<SessionManifest>,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> anyhow::Result<(Option<agent::TurnVerdict>, bool)>
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
    let mut turn_builder = build_turn_builder_for_subscriptions_with_manifest(
        config.clone(),
        subscriptions,
        session_manifest,
    );
    let mut provider_factory = build_openai_provider_factory(state.http_client.clone(), config);
    drain_session_queue_with_turn_builder_locked(
        state,
        session_id,
        &mut turn_builder,
        &mut provider_factory,
        token_sink,
        approval_handler,
    )
    .await
}

pub(super) async fn drain_session_queue_with_subscriptions<TS, AH>(
    state: ServerState,
    session_id: String,
    config: crate::config::Config,
    session_manifest: Option<SessionManifest>,
    token_sink: &mut TS,
    approval_handler: &mut AH,
    pause_while_websocket_active: bool,
) -> anyhow::Result<(Option<agent::TurnVerdict>, bool)>
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
    let mut turn_builder = build_turn_builder_for_subscriptions_with_manifest(
        config.clone(),
        subscriptions,
        session_manifest,
    );
    let mut provider_factory = build_openai_provider_factory(state.http_client.clone(), config);
    drain_session_queue_with_turn_builder(
        state,
        session_id,
        &mut turn_builder,
        &mut provider_factory,
        token_sink,
        approval_handler,
        pause_while_websocket_active,
    )
    .await
}

pub(super) fn spawn_background_queue_worker(
    state: ServerState,
    session_id: String,
    config: crate::config::Config,
    session_manifest: Option<SessionManifest>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut token_sink = NoopTokenSink;
        let mut approval_handler = RejectApprovalHandler;
        loop {
            if state.always_on_websocket_count(&session_id) > 0 {
                sleep(Duration::from_millis(100)).await;
                continue;
            }
            match drain_session_queue_with_subscriptions(
                state.clone(),
                session_id.clone(),
                config.clone(),
                session_manifest.clone(),
                &mut token_sink,
                &mut approval_handler,
                true,
            )
            .await
            {
                Ok((Some(verdict), processed_any)) => {
                    match verdict {
                        agent::TurnVerdict::Denied { reason: _, gate_id } => {
                            warn!(%gate_id, "always-on turn denied");
                        }
                        _ => unreachable!("drain_queue only returns denial verdicts"),
                    }
                    if !processed_any {
                        sleep(Duration::from_millis(100)).await;
                    }
                }
                Ok((None, processed_any)) => {
                    if !processed_any {
                        sleep(Duration::from_millis(100)).await;
                    }
                }
                Err(error) => {
                    warn!(%session_id, %error, "failed to drain queued messages");
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }
    })
}

pub(super) struct NoopTokenSink;

impl agent::TokenSink for NoopTokenSink {
    fn on_token(&mut self, _token: String) {}
}

pub(super) struct RejectApprovalHandler;

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
