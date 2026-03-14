//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Write};
use autopoiesis::{agent, auth, config, gate, llm, session, tools};

use std::collections::HashMap;
use std::env;

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
            let config = config::Config::load("agents.toml")
                .map_err(|error| anyhow!("failed to load configuration: {error}"))?;

            let mut session = session::Session::new("sessions")?;
            session.load_today()?;
            let provider_config = config.clone();
            let mut pipeline = default_pipeline(&config);

            // Build a fresh provider per turn so the auth token can be refreshed mid-session.
            let mut provider_factory = move || {
                let provider_config = provider_config.clone();

                async move {
                    let api_key = auth::get_valid_token().await?;
                    Ok::<llm::openai::OpenAIProvider, anyhow::Error>(llm::openai::OpenAIProvider::new(
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
                    agent::run_agent_loop(
                        &mut provider_factory,
                        &mut session,
                        prompt.to_string(),
                        &mut pipeline,
                    )
                    .await?;
                    println!();
                }
            } else {
                agent::run_agent_loop(
                    &mut provider_factory,
                    &mut session,
                    cli.prompt.join(" "),
                    &mut pipeline,
                )
                .await?;
            }
        }
    }

    Ok(())
}

fn default_pipeline(config: &config::Config) -> gate::Pipeline {
    let cwd = env::current_dir()
        .ok()
        .and_then(|path| path.to_str().map(ToString::to_string))
        .unwrap_or_else(String::new);
    let tools = vec![tools::execute_tool_definition()];
    let tools_list = tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let mut vars = HashMap::new();
    vars.insert("model".to_string(), config.model.clone());
    vars.insert("cwd".to_string(), cwd);
    vars.insert("tools".to_string(), tools_list);

    gate::Pipeline::new()
        .assemble(gate::IdentityGate::new("identity", vars, &config.system_prompt))
        .context(gate::HistoryGate::new(100_000))
        .sanitize(gate::SecretRedactor::new(&[
            r"sk-[a-zA-Z0-9_-]{20,}",
            r"ghp_[a-zA-Z0-9]{36}",
            r"AKIA[0-9A-Z]{16}",
        ]))
        .validate(gate::ShellHeuristic::new())
        .validate(gate::ExfiltrationDetector::new())
}
