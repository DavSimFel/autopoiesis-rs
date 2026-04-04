#![cfg(not(clippy))]
#![allow(clippy::all)]

#[cfg(feature = "integration")]
use anyhow::{Result, anyhow};
#[cfg(feature = "integration")]
use autopoiesis::{
    auth,
    config::Config,
    llm::{ChatMessage, LlmProvider, StopReason, openai::OpenAIProvider},
    tool::Shell,
    turn::Turn,
};

#[cfg(feature = "integration")]
async fn get_valid_token_or_skip() -> Result<Option<String>> {
    let auth_path = auth::token_file_path();
    if !auth_path.exists() {
        return Ok(None);
    }

    Ok(Some(auth::get_valid_token().await?))
}

#[cfg(feature = "integration")]
fn skip_no_auth() {
    eprintln!("skipping integration test: no local auth; run autopoiesis auth login");
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn auth_status_when_logged_in() -> Result<()> {
    let Some(token) = get_valid_token_or_skip().await? else {
        skip_no_auth();
        return Ok(());
    };
    assert!(!token.trim().is_empty());
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn simple_prompt() -> Result<()> {
    let config = Config::load("agents.toml")?;
    let Some(token) = get_valid_token_or_skip().await? else {
        skip_no_auth();
        return Ok(());
    };
    let provider = OpenAIProvider::new(
        token,
        config.base_url,
        config.model,
        config.reasoning_effort,
    );

    let mut tokens = Vec::<String>::new();
    let mut on_token = |token: String| {
        tokens.push(token);
    };

    let turn = provider
        .stream_completion(
            &[
                ChatMessage::system("You are a helpful assistant."),
                ChatMessage::user("Reply briefly with a greeting."),
            ],
            &[],
            &mut on_token,
        )
        .await?;

    let response = tokens.concat();
    let words = response
        .split_whitespace()
        .map(|word| word.trim_matches(|ch: char| !ch.is_ascii_alphanumeric()))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    assert!(
        !words.is_empty(),
        "expected a word-like response, got {response:?}"
    );
    assert!(
        response.chars().any(|ch| ch.is_ascii_alphabetic()),
        "expected response to include actual words, got {response:?}"
    );
    assert_eq!(turn.stop_reason, StopReason::Stop);
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn invalid_model_returns_error() -> anyhow::Result<()> {
    let config = Config::load("agents.toml")?;
    let Some(token) = get_valid_token_or_skip().await? else {
        skip_no_auth();
        return Ok(());
    };
    let provider = OpenAIProvider::new(
        token,
        config.base_url,
        "nonexistent-model-xyz",
        config.reasoning_effort,
    );

    let mut on_token = |token: String| {
        // keep behavior consistent with other tests while proving the call can produce output before failing.
        let _ = token;
    };

    let result = provider
        .stream_completion(
            &[
                ChatMessage::system("You are a helpful assistant."),
                ChatMessage::user("Hello"),
            ],
            &[],
            &mut on_token,
        )
        .await;

    let err = match result {
        Ok(_) => return Err(anyhow!("invalid model should be rejected")),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("API error"), "unexpected error: {err}");
    assert!(
        err.contains("nonexistent-model-xyz") || err.contains("model_not_found"),
        "expected a model-specific rejection, got {err}"
    );
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn tool_call_roundtrip() -> Result<()> {
    let config = Config::load("agents.toml")?;
    let Some(token) = get_valid_token_or_skip().await? else {
        skip_no_auth();
        return Ok(());
    };
    let provider = OpenAIProvider::new(
        token,
        config.base_url,
        config.model,
        config.reasoning_effort,
    );

    let shell_turn = Turn::new().tool(Shell::new());
    let tool_defs = shell_turn.tool_definitions();
    let mut tokens = Vec::<String>::new();
    let mut on_token = |token: String| {
        tokens.push(token);
    };

    let completion = provider
        .stream_completion(
            &[
                ChatMessage::system("You are a helpful assistant. Use tools when asked."),
                ChatMessage::user(
                    "You MUST use the execute tool to run this shell command: echo hello123. Do not respond with text, only use the tool.",
                ),
            ],
            &tool_defs,
            &mut on_token,
        )
        .await?;

    assert_eq!(completion.stop_reason, StopReason::ToolCalls);
    let call = match completion
        .tool_calls
        .iter()
        .find(|call| call.name == "execute")
    {
        Some(call) => call,
        None => return Err(anyhow!("expected execute tool call")),
    };

    let output = shell_turn.execute_tool(&call.name, &call.arguments).await?;
    assert!(output.contains("hello123"));
    Ok(())
}
