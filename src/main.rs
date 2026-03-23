//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{Result, anyhow};
use autopoiesis::server;
use autopoiesis::subscription::{self, SubscriptionFilter, SubscriptionRecord};
use autopoiesis::{agent, auth, cli, config, llm, session, store, turn};
use clap::{Args, Parser, Subcommand};
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

#[cfg(test)]
mod subscription_cli_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_subscription_add_command() {
        let cli = Cli::parse_from([
            "autopoiesis",
            "sub",
            "add",
            "notes.txt",
            "--topic",
            "alpha",
            "--jq",
            ".items[0] | .name",
        ]);

        match cli.command {
            Some(Commands::Sub {
                action: SubscriptionCommand::Add(args),
            }) => {
                assert_eq!(args.path, "notes.txt");
                assert_eq!(args.topic.as_deref(), Some("alpha"));
                assert_eq!(args.jq.as_deref(), Some(".items[0] | .name"));
            }
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_subscription_list_command() {
        let cli = Cli::parse_from(["autopoiesis", "sub", "list", "--topic", "beta"]);

        match cli.command {
            Some(Commands::Sub {
                action: SubscriptionCommand::List(args),
            }) => {
                assert_eq!(args.topic.as_deref(), Some("beta"));
            }
            _ => panic!("unexpected command variant"),
        }
    }
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
    Sub {
        #[command(subcommand)]
        action: SubscriptionCommand,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    Login,
    Status,
    Logout,
}

#[derive(Subcommand)]
enum SubscriptionCommand {
    Add(SubscriptionAddArgs),
    Remove(SubscriptionRemoveArgs),
    List(SubscriptionListArgs),
}

#[derive(Args)]
struct SubscriptionAddArgs {
    path: String,

    #[arg(long)]
    topic: Option<String>,

    #[arg(long)]
    lines: Option<String>,

    #[arg(long)]
    regex: Option<String>,

    #[arg(long)]
    head: Option<usize>,

    #[arg(long)]
    tail: Option<usize>,

    #[arg(
        long,
        help = "Limited jq subset: .field, .field[index], .field[], and pipelines with |"
    )]
    jq: Option<String>,
}

#[derive(Args)]
struct SubscriptionRemoveArgs {
    path: String,

    #[arg(long)]
    topic: Option<String>,
}

#[derive(Args)]
struct SubscriptionListArgs {
    #[arg(long)]
    topic: Option<String>,
}

fn resolve_session_id(cli_session: Option<&str>, config_session: Option<&str>) -> String {
    cli_session
        .or(config_session)
        .unwrap_or("default")
        .to_string()
}

fn default_subscription_topic(topic: Option<String>) -> String {
    topic.unwrap_or_else(|| "_default".to_string())
}

fn subscription_filter(args: &SubscriptionAddArgs) -> Result<SubscriptionFilter> {
    SubscriptionFilter::from_flags(
        args.lines.as_deref(),
        args.regex.as_deref(),
        args.head,
        args.tail,
        args.jq.as_deref(),
    )
}

fn render_subscription_summary(records: &[SubscriptionRecord]) -> Option<usize> {
    let mut total = 0usize;
    for record in records {
        match record.utilization_tokens() {
            Ok(count) => total += count,
            Err(error) => {
                eprintln!(
                    "warning: failed to estimate subscription utilization for {}: {error}",
                    record.path.display()
                );
                return None;
            }
        }
    }

    Some(total)
}

fn print_subscription_rows(records: &[SubscriptionRecord]) {
    for record in records {
        println!("{}", record.format_listing());
    }
}

async fn handle_subscription_command(command: SubscriptionCommand) -> Result<()> {
    let mut store = store::Store::new("sessions/queue.sqlite")?;

    match command {
        SubscriptionCommand::Add(args) => {
            let topic = default_subscription_topic(args.topic.clone());
            let normalized_path = subscription::normalize_path(&args.path)?;
            subscription::ensure_readable_subscription_path(&normalized_path)?;
            let filter = subscription_filter(&args)?;
            store.create_subscription(
                &topic,
                &normalized_path.display().to_string(),
                filter.to_storage().as_deref(),
            )?;
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(None)?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            match render_subscription_summary(&records) {
                Some(total) => println!("subscription utilization: {total} tokens"),
                None => println!("subscription utilization: unavailable"),
            }
        }
        SubscriptionCommand::Remove(args) => {
            let topic = default_subscription_topic(args.topic.clone());
            let normalized_path = subscription::normalize_path(&args.path)?;
            let deleted =
                store.delete_subscription(&topic, &normalized_path.display().to_string())?;
            println!("removed {deleted} subscription(s)");
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(None)?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            match render_subscription_summary(&records) {
                Some(total) => println!("subscription utilization: {total} tokens"),
                None => println!("subscription utilization: unavailable"),
            }
        }
        SubscriptionCommand::List(args) => {
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(args.topic.as_deref())?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            print_subscription_rows(&records);
            match render_subscription_summary(&records) {
                Some(total) => println!("subscription utilization: {total} tokens"),
                None => println!("subscription utilization: unavailable"),
            }
        }
    }

    Ok(())
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
        Some(Commands::Sub { action }) => {
            handle_subscription_command(action).await?;
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
            match queue.recover_stale_messages(config.queue.stale_processing_timeout_secs) {
                Ok(recovered) if recovered > 0 => {
                    eprintln!("recovered {recovered} stale messages from previous crash");
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("warning: failed to recover stale messages: {error}");
                }
            }
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

            let mut token_sink = cli::CliTokenSink::new();
            let mut approval_handler = cli::CliApprovalHandler::new();

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
                        eprintln!("{}", cli::format_denial_message(&reason, &gate_id));
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
                    eprintln!("{}", cli::format_denial_message(&reason, &gate_id));
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
