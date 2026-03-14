#[cfg(feature = "integration")]
use autopoiesis::{
    auth,
    config::Config,
    llm::{openai::OpenAIProvider, ChatMessage, LlmProvider, StopReason},
    turn::Turn,
    tool::Shell,
};
#[cfg(feature = "integration")]
use anyhow::Result;

#[cfg(feature = "integration")]
#[tokio::test]
async fn auth_status_when_logged_in() -> Result<()> {
    let token = auth::get_valid_token().await?;
    assert!(!token.trim().is_empty());
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn simple_prompt() -> Result<()> {
    let config = Config::load("agents.toml")?;
    let token = auth::get_valid_token().await?;
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
            &[ChatMessage::system("You are a helpful assistant."), ChatMessage::user("Say hi in 3 words")],
            &[],
            &mut on_token,
        )
        .await?;

    let response = tokens.concat();
    assert!(!response.trim().is_empty());
    assert_eq!(turn.stop_reason, StopReason::Stop);
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn invalid_model_returns_error() -> anyhow::Result<()> {
    let config = Config::load("agents.toml")?;
    let token = auth::get_valid_token().await?;
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
            &[ChatMessage::system("You are a helpful assistant."), ChatMessage::user("Hello")],
            &[],
            &mut on_token,
        )
        .await;

    assert!(result.is_err());
    Ok(())
}

#[cfg(feature = "integration")]
#[tokio::test]
async fn tool_call_roundtrip() -> Result<()> {
    let config = Config::load("agents.toml")?;
    let token = auth::get_valid_token().await?;
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
    let call = completion
        .tool_calls
        .iter()
        .find(|call| call.name == "execute")
        .expect("expected execute tool call");

    let output = shell_turn.execute_tool(&call.name, &call.arguments)?;
    assert!(output.contains("hello123"));
    Ok(())
}
