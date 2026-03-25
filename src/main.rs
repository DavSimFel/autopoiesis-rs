//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{Result, anyhow};
use autopoiesis::server;
use autopoiesis::subscription::{self, SubscriptionFilter, SubscriptionRecord};
use autopoiesis::util::{
    PlainMessageFormatter, STDERR_USER_OUTPUT_TARGET, STDOUT_USER_OUTPUT_TARGET,
};
use autopoiesis::{agent, auth, cli, config, llm, session, store, turn};
use clap::{Args, Parser, Subcommand};
use reqwest::Client;
use tracing::{info, warn};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::prelude::*;

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

fn build_tracing_subscriber_with_filters<DW, SW, EW>(
    diagnostic_writer: DW,
    stdout_writer: SW,
    stderr_writer: EW,
    diagnostic_filter: EnvFilter,
    stdout_filter: EnvFilter,
    stderr_filter: EnvFilter,
) -> impl tracing::Subscriber + Send + Sync
where
    DW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    SW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    EW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let diagnostic_filter = diagnostic_filter
        .add_directive(
            format!("{STDOUT_USER_OUTPUT_TARGET}=off")
                .parse()
                .expect("stdout user-output target directive should parse"),
        )
        .add_directive(
            format!("{STDERR_USER_OUTPUT_TARGET}=off")
                .parse()
                .expect("stderr user-output target directive should parse"),
        );

    let diagnostic_layer = tracing_subscriber::fmt::layer()
        .with_writer(diagnostic_writer)
        .with_filter(diagnostic_filter);
    let stdout_layer = tracing_subscriber::fmt::layer()
        .event_format(PlainMessageFormatter)
        .with_writer(stdout_writer)
        .with_ansi(false)
        .with_filter(stdout_filter);
    let stderr_layer = tracing_subscriber::fmt::layer()
        .event_format(PlainMessageFormatter)
        .with_writer(stderr_writer)
        .with_ansi(false)
        .with_filter(stderr_filter);

    tracing_subscriber::registry()
        .with(diagnostic_layer)
        .with(stdout_layer)
        .with(stderr_layer)
}

fn build_tracing_subscriber<DW, SW, EW>(
    diagnostic_writer: DW,
    stdout_writer: SW,
    stderr_writer: EW,
) -> impl tracing::Subscriber + Send + Sync
where
    DW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    SW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
    EW: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let diagnostic_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let stdout_filter = EnvFilter::new(format!("{STDOUT_USER_OUTPUT_TARGET}=trace"));
    let stderr_filter = EnvFilter::new(format!("{STDERR_USER_OUTPUT_TARGET}=trace"));

    build_tracing_subscriber_with_filters(
        diagnostic_writer,
        stdout_writer,
        stderr_writer,
        diagnostic_filter,
        stdout_filter,
        stderr_filter,
    )
}

fn init_tracing() {
    let subscriber = build_tracing_subscriber(std::io::stderr, std::io::stdout, std::io::stderr);
    let _ = subscriber.try_init();
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
                warn!(
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
        info!(target: STDOUT_USER_OUTPUT_TARGET, "{}", record.format_listing());
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
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
            }
        }
        SubscriptionCommand::Remove(args) => {
            let topic = default_subscription_topic(args.topic.clone());
            let normalized_path = subscription::normalize_path(&args.path)?;
            let deleted =
                store.delete_subscription(&topic, &normalized_path.display().to_string())?;
            info!(
                target: STDOUT_USER_OUTPUT_TARGET,
                "removed {deleted} subscription(s)"
            );
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(None)?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            match render_subscription_summary(&records) {
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
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
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Auth { action }) => match action {
            AuthAction::Login => {
                let tokens = auth::device_code_login().await?;
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "Logged in. Token expiry: {}",
                    tokens.expires_at
                );
            }
            AuthAction::Status => {
                let auth_path = auth::token_file_path();

                if !auth_path.exists() {
                    info!(target: STDOUT_USER_OUTPUT_TARGET, "Not logged in");
                    info!(
                        target: STDOUT_USER_OUTPUT_TARGET,
                        "Run: autopoiesis auth login"
                    );
                    return Ok(());
                }

                let tokens = auth::read_tokens()
                    .map_err(|error| anyhow!("failed to read auth file: {error}"))?;
                info!(target: STDOUT_USER_OUTPUT_TARGET, "Logged in");
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "Expires at: {}",
                    tokens.expires_at
                );
            }
            AuthAction::Logout => {
                let auth_path = auth::token_file_path();

                if !auth_path.exists() {
                    info!(target: STDOUT_USER_OUTPUT_TARGET, "Not logged in");
                    return Ok(());
                }

                std::fs::remove_file(&auth_path).map_err(|error| {
                    anyhow!("failed to remove {}: {error}", auth_path.display())
                })?;
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "Logged out from {}",
                    auth_path.display()
                );
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
                    warn!("recovered {recovered} stale messages from previous crash");
                }
                Ok(_) => {}
                Err(error) => {
                    warn!("warning: failed to recover stale messages: {error}");
                }
            }
            queue.create_session(&session_id, Some(r#"{"source":"cli"}"#))?;

            let provider_config = config.clone();
            let turn = turn::build_turn_for_config(&provider_config);
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
                        warn!(
                            target: STDERR_USER_OUTPUT_TARGET,
                            "{}",
                            cli::format_denial_message(&reason, &gate_id)
                        );
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
                    warn!(
                        target: STDERR_USER_OUTPUT_TARGET,
                        "{}",
                        cli::format_denial_message(&reason, &gate_id)
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_session_id;
    use super::{
        STDERR_USER_OUTPUT_TARGET, STDOUT_USER_OUTPUT_TARGET, build_tracing_subscriber_with_filters,
    };
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::filter::EnvFilter;

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

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("writer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn tracing_layers_route_user_output_without_duplication() {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let diagnostic = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let subscriber = build_tracing_subscriber_with_filters(
            {
                let diagnostic = diagnostic.clone();
                move || SharedWriter(diagnostic.clone())
            },
            {
                let stdout = stdout.clone();
                move || SharedWriter(stdout.clone())
            },
            {
                let stderr = stderr.clone();
                move || SharedWriter(stderr.clone())
            },
            EnvFilter::new("info"),
            EnvFilter::new(format!("{STDOUT_USER_OUTPUT_TARGET}=trace")),
            EnvFilter::new(format!("{STDERR_USER_OUTPUT_TARGET}=trace")),
        );

        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!(target: STDOUT_USER_OUTPUT_TARGET, "hello");
        tracing::warn!("diagnostic");
        tracing::info!(target: STDERR_USER_OUTPUT_TARGET, "denial");

        let diagnostic_text =
            String::from_utf8(diagnostic.lock().expect("diagnostic lock").clone())
                .expect("diagnostic utf8");
        let stdout_text =
            String::from_utf8(stdout.lock().expect("stdout lock").clone()).expect("stdout utf8");
        let stderr_text =
            String::from_utf8(stderr.lock().expect("stderr lock").clone()).expect("stderr utf8");

        assert_eq!(stdout_text, "hello\n");
        assert_eq!(diagnostic_text.matches("diagnostic").count(), 1);
        assert_eq!(stderr_text.matches("denial\n").count(), 1);
        assert!(!diagnostic_text.contains("hello\n"));
        assert!(!stderr_text.contains("hello\n"));
        assert!(!diagnostic_text.contains("denial\n"));
        assert!(!diagnostic_text.contains("hello\n"));
    }
}
