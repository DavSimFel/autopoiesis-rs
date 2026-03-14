//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Write};

mod agent;
mod auth;
mod config;
mod llm;
mod identity;
mod template;
mod session;
mod tools;

use std::collections::HashMap;
use std::env;

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

            let cwd = env::current_dir()
                .ok()
                .and_then(|path| path.to_str().map(ToString::to_string))
                .unwrap_or_else(String::new);
            let tools = vec![tools::execute_tool_definition()];
            let tool_names = tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            let mut vars = HashMap::new();
            vars.insert("model".to_string(), config.model.clone());
            vars.insert("cwd".to_string(), cwd);
            vars.insert("tools".to_string(), tool_names);

            let system_prompt = identity::load_system_prompt("identity", &vars)
                .unwrap_or(config.system_prompt.clone());

            let mut session = Session::new(system_prompt);
            let provider_config = config.clone();

            // Build a fresh provider per turn so the auth token can be refreshed mid-session.
            let mut provider_factory = move || {
                let provider_config = provider_config.clone();

                async move {
                    let api_key = auth::get_valid_token().await?;
                    Ok::<OpenAIProvider, anyhow::Error>(OpenAIProvider::new(
                        api_key,
                        provider_config.base_url,
                        provider_config.model,
                        provider_config.reasoning_effort,
                    ))
                }
            };

            if cli.prompt.is_empty() {
                let stdin = io::stdin();
                let mut line = String::new();
                let mut handle = stdin.lock();
                loop {
                    print!("> ");
                    io::stdout().flush()?;
                    line.clear();
                    if handle.read_line(&mut line)? == 0 {
                        break;
                    }
                    let prompt = line.trim();
                    if prompt.is_empty() {
                        continue;
                    }
                    if prompt == "exit" || prompt == "quit" {
                        break;
                    }
                    run_agent_loop(&mut provider_factory, &mut session, prompt.to_string()).await?;
                    println!();
                }
            } else {
                run_agent_loop(&mut provider_factory, &mut session, cli.prompt.join(" ")).await?;
            }
        }
    }

    Ok(())
}
