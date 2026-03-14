use anyhow::{anyhow, Result};
use clap::{CommandFactory, Parser, Subcommand};

mod agent;
mod auth;
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
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(help = "Prompt for the agent", trailing_var_arg = true)]
    prompt: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    Login,
    Status,
    Logout,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Auth { action }) => match action {
            AuthAction::Login => {
                let tokens = auth::device_code_login().await?;
                println!("Logged in. Token expiry: {}", tokens.expires_at);
            }
            AuthAction::Status => {
                let auth_path = auth::token_file_path();

                if !auth_path.exists() {
                    println!("Not logged in");
                    println!("Run: autopoiesis auth login");
                    return Ok(());
                }

                let tokens =
                    auth::read_tokens().map_err(|error| anyhow!("failed to read auth file: {error}"))?;
                println!("Logged in");
                println!("Expires at: {}", tokens.expires_at);
            }
            AuthAction::Logout => {
                let auth_path = auth::token_file_path();

                if !auth_path.exists() {
                    println!("Not logged in");
                    return Ok(());
                }

                std::fs::remove_file(&auth_path)
                    .map_err(|error| anyhow!("failed to remove {}: {error}", auth_path.display()))?;
                println!("Logged out from {}", auth_path.display());
            }
        },
        None => {
            let config = Config::load("agents.toml")
                .map_err(|error| anyhow!("failed to load configuration: {error}"))?;

            if cli.prompt.is_empty() {
                let mut cmd = Cli::command();
                cmd.print_help()?;
                println!();
                println!("Run: autopoiesis auth login");
                return Ok(());
            }

            let mut session = Session::new(config.system_prompt);
            let prompt = cli.prompt.join(" ");
            let base_url = config.base_url.clone();
            let model = config.model.clone();
            let max_output_tokens = config.max_output_tokens;
            let reasoning_effort = config.reasoning_effort.clone();

            let provider_factory = move || {
                let base_url = base_url.clone();
                let model = model.clone();
                let reasoning_effort = reasoning_effort.clone();

                async move {
                    let api_key = auth::get_valid_token().await?;
                    Ok::<OpenAIProvider, anyhow::Error>(OpenAIProvider::new(
                        api_key,
                        base_url,
                        model,
                        max_output_tokens,
                        reasoning_effort,
                    ))
                }
            };

            run_agent_loop(provider_factory, &mut session, prompt).await?;
        }
    }

    Ok(())
}
