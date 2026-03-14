use anyhow::Result;
use clap::Parser;

mod agent;
mod config;
mod llm;
mod session;
mod tools;

use crate::agent::run_agent_loop;
use crate::config::Config;
use crate::llm::openai::OpenAIProvider;
use crate::session::Session;

#[derive(Parser)]
#[command(name = "autopoiesis", version, about = "MVP Rust agent runtime")]
struct Cli {
    #[arg(help = "Prompt for the agent")]
    prompt: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let config = Config::load("agents.toml")
        .map_err(|error| anyhow::anyhow!("failed to load configuration: {error}"))?;
    let api_key = config.openai_api_key()?;

    let provider = OpenAIProvider::new(api_key, config.base_url, config.model, config.max_tokens);
    let mut session = Session::new(config.system_prompt);

    run_agent_loop(&provider, &mut session, cli.prompt).await
}
