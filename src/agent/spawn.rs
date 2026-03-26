//! Child session spawn/drain helpers.

use anyhow::{Context, Result};
use std::path::Path;

use crate::llm::LlmProvider;
use crate::plan::extract_plan_action;
use crate::session::Session;
use crate::store::Store;

use super::queue::drain_queue_with_stats;
use super::{ApprovalHandler, SpawnDrainResult, SpawnRequest, SpawnResult, TokenSink, TurnVerdict};

pub(super) type SpawnedChildMetadata = crate::spawn::ChildSessionMetadata;

pub(super) fn parse_spawned_child_metadata(metadata: &str) -> Result<SpawnedChildMetadata> {
    crate::spawn::parse_child_session_metadata(metadata)
}

pub(super) async fn spawn_and_drain_with_provider<F, Fut, P, TS>(
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

pub(crate) struct SpawnDrainContext<'a> {
    pub(crate) store: &'a mut Store,
    pub(crate) config: &'a crate::config::Config,
    pub(crate) session_dir: &'a Path,
    pub(crate) spawn_result: SpawnResult,
}

pub(crate) async fn finish_spawned_child_drain<F, Fut, P, TS>(
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
    let metadata = parse_spawned_child_metadata(metadata_json)?;
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
    }?;

    let child_session_dir = context
        .session_dir
        .join(&context.spawn_result.child_session_id);
    let mut child_session =
        Session::new(&child_session_dir).context("failed to open child session")?;
    child_session
        .load_today()
        .context("failed to load child session history")?;

    let mut make_provider_for_turn = || make_provider(&child_config);
    let (drain_result, completed_agent_turn, current_assistant_response) = drain_queue_with_stats(
        context.store,
        &context.spawn_result.child_session_id,
        &mut child_session,
        &turn,
        &mut make_provider_for_turn,
        token_sink,
        approval_handler,
    )
    .await?;
    match drain_result {
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

    let last_assistant_response = current_assistant_response;
    if completed_agent_turn {
        apply_t2_plan_handoff(
            context.store,
            &metadata.parent_session_id,
            &metadata.tier,
            last_assistant_response.as_deref(),
        )?;
    }
    Ok(SpawnDrainResult {
        child_session_id: context.spawn_result.child_session_id,
        resolved_model: context.spawn_result.resolved_model,
        last_assistant_response,
    })
}

fn apply_t2_plan_handoff(
    store: &mut Store,
    owner_session_id: &str,
    tier: &str,
    last_assistant_response: Option<&str>,
) -> Result<()> {
    if tier != "t2" {
        return Ok(());
    }

    let Some(last_assistant_response) = last_assistant_response else {
        return Ok(());
    };

    let Some(action) = extract_plan_action(last_assistant_response)? else {
        return Ok(());
    };

    crate::plan::patch::apply_plan_action(store, owner_session_id, &action)?;
    Ok(())
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

#[cfg(test)]
mod tests;
