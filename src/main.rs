//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{Result, anyhow};
use autopoiesis::server;
use autopoiesis::{agent, auth, config, llm, session, store, turn};
use clap::{Parser, Subcommand};
use reqwest::Client;

use std::io::{self, BufRead, Write};

#[derive(Parser)]
#[command(name = "autopoiesis", version, about = "MVP Rust agent runtime")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(
        long,
        value_name = "name",
        help = "Persistent session name for CLI mode"
    )]
    session: Option<String>,

    #[arg(help = "Prompt for the agent", trailing_var_arg = true)]
    prompt: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    Serve {
        #[arg(short, long, default_value_t = 8423)]
        port: u16,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    Login,
    Status,
    Logout,
}

fn resolve_session_id(cli_session: Option<&str>, config_session: Option<&str>) -> String {
    cli_session
        .or(config_session)
        .unwrap_or("default")
        .to_string()
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

                let tokens = auth::read_tokens()
                    .map_err(|error| anyhow!("failed to read auth file: {error}"))?;
                println!("Logged in");
                println!("Expires at: {}", tokens.expires_at);
            }
            AuthAction::Logout => {
                let auth_path = auth::token_file_path();

                if !auth_path.exists() {
                    println!("Not logged in");
                    return Ok(());
                }

                std::fs::remove_file(&auth_path).map_err(|error| {
                    anyhow!("failed to remove {}: {error}", auth_path.display())
                })?;
                println!("Logged out from {}", auth_path.display());
            }
        },
        Some(Commands::Serve { port }) => {
            server::run(port).await?;
        }
        None => {
            let config = config::Config::load("agents.toml")
                .map_err(|error| anyhow!("failed to load configuration: {error}"))?;

            let session_id =
                resolve_session_id(cli.session.as_deref(), config.session_name.as_deref());
            let session_root = format!("sessions/{session_id}");
            let mut history = session::Session::new(&session_root)?;
            history.load_today()?;

            let mut queue = store::Store::new("sessions/queue.sqlite")?;
            queue.create_session(&session_id, Some(r#"{"source":"cli"}"#))?;

            let provider_config = config.clone();
            let turn = turn::build_default_turn(&provider_config);
            let http_client = Client::new();
            let mut provider_factory = move || {
                let provider_config = provider_config.clone();
                let client = http_client.clone();
                async move {
                    let api_key = auth::get_valid_token().await?;
                    Ok::<llm::openai::OpenAIProvider, anyhow::Error>(
                        llm::openai::OpenAIProvider::with_client(
                            client,
                            api_key,
                            provider_config.base_url,
                            provider_config.model,
                            provider_config.reasoning_effort,
                        ),
                    )
                }
            };

            let mut token_sink = agent::CliTokenSink::new();
            let mut approval_handler = agent::CliApprovalHandler::new();

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

                    queue.enqueue_message(&session_id, "user", prompt, "cli")?;
                    if let Some(agent::TurnVerdict::Denied { reason, gate_id }) =
                        agent::drain_queue(
                            &mut queue,
                            &session_id,
                            &mut history,
                            &turn,
                            &mut provider_factory,
                            &mut token_sink,
                            &mut approval_handler,
                        )
                        .await?
                    {
                        eprintln!("{}", agent::format_denial_message(&reason, &gate_id));
                    }
                }
            } else {
                let prompt = cli.prompt.join(" ");
                queue.enqueue_message(&session_id, "user", &prompt, "cli")?;
                if let Some(agent::TurnVerdict::Denied { reason, gate_id }) = agent::drain_queue(
                    &mut queue,
                    &session_id,
                    &mut history,
                    &turn,
                    &mut provider_factory,
                    &mut token_sink,
                    &mut approval_handler,
                )
                .await?
                {
                    eprintln!("{}", agent::format_denial_message(&reason, &gate_id));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_session_id;

    #[test]
    fn resolve_session_id_prefers_cli_value() {
        assert_eq!(
            resolve_session_id(Some("fix-auth"), Some("configured-default")),
            "fix-auth"
        );
    }

    #[test]
    fn resolve_session_id_uses_config_value_when_cli_missing() {
        assert_eq!(
            resolve_session_id(None, Some("configured-default")),
            "configured-default"
        );
    }

    #[test]
    fn resolve_session_id_falls_back_to_default() {
        assert_eq!(resolve_session_id(None, None), "default");
    }
}
