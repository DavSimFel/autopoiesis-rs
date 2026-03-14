#[cfg(feature = "integration")]
use autopoiesis::{
    auth,
    config::Config,
    llm::{openai::OpenAIProvider, ChatMessage, LlmProvider, StopReason},
    tools,
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
async fn tool_call_roundtrip() -> Result<()> {
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
    let tool_defs = vec![tools::execute_tool_definition()];

    let turn = provider
        .stream_completion(
            &[ChatMessage::system("You are a helpful assistant. Use tools when asked."), ChatMessage::user("Run: echo hello123")],
            &tool_defs,
            &mut on_token,
        )
        .await?;

    assert_eq!(turn.stop_reason, StopReason::ToolCalls);
    let call = turn
        .tool_calls
        .iter()
        .find(|call| call.name == "execute")
        .expect("expected execute tool call");

    let output = tools::execute_tool_call(call).await?;
    assert!(output.contains("hello123"));
    Ok(())
}
