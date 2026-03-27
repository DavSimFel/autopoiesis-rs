//! Shared subscription and provider factory helpers for session drains.

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
use reqwest::Client;

use crate::auth;
use crate::config::Config;
use crate::context::SessionManifest;
use crate::llm::openai::OpenAIProvider;
use crate::store::Store;
use crate::subscription::SubscriptionRecord;
use crate::turn::{self, Turn};

pub fn load_subscriptions_for_session(
    store: &mut Store,
    session_id: &str,
) -> Result<Vec<SubscriptionRecord>> {
    store
        .list_subscriptions_for_session(session_id)?
        .into_iter()
        .map(SubscriptionRecord::from_row)
        .collect()
}

pub fn build_turn_builder_for_subscriptions(
    config: Config,
    subscriptions: Vec<SubscriptionRecord>,
) -> impl FnMut() -> Result<Turn> {
    build_turn_builder_for_subscriptions_with_manifest(config, subscriptions, None)
}

pub fn build_turn_builder_for_subscriptions_with_manifest(
    config: Config,
    subscriptions: Vec<SubscriptionRecord>,
    session_manifest: Option<SessionManifest>,
) -> impl FnMut() -> Result<Turn> {
    move || {
        turn::build_turn_for_config_with_subscriptions_and_manifest(
            &config,
            &subscriptions,
            session_manifest.as_ref(),
        )
    }
}

pub fn build_openai_provider_factory(
    client: Client,
    config: Config,
) -> impl FnMut() -> Pin<Box<dyn Future<Output = Result<OpenAIProvider>> + Send>> {
    move || {
        let client = client.clone();
        let config = config.clone();
        Box::pin(async move {
            let api_key = auth::get_valid_token().await?;
            Ok::<OpenAIProvider, anyhow::Error>(OpenAIProvider::with_client(
                client,
                api_key,
                config.base_url,
                config.model,
                config.reasoning_effort,
            ))
        })
    }
}

pub fn build_turn_builder_for_t3(
    config: Config,
    skills: Vec<crate::skills::SkillDefinition>,
) -> impl FnMut() -> Result<Turn> {
    move || crate::turn::build_spawned_t3_turn(&config, skills.clone())
}
