//! Binary entrypoint for the `autopoiesis` CLI.

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use autopoiesis::{
    agent, auth, config, llm, session, store, turn,
};
use autopoiesis::server;
use reqwest::Client;

use std::io::{self, BufRead, Write};
use std::time::{SystemTime, UNIX_EPOCH};

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

                let tokens = auth::read_tokens().map_err(|error| anyhow!("failed to read auth file: {error}"))?;
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
        Some(Commands::Serve { port }) => {
            server::run(port).await?;
        }
        None => {
            let config = config::Config::load("agents.toml")
                .map_err(|error| anyhow!("failed to load configuration: {error}"))?;

            let session_id = format!(
                "cli-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros()
            );
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
                    process_queue(
                        &mut queue,
                        &session_id,
                        &mut history,
                        &turn,
                        &mut provider_factory,
                        &mut token_sink,
                        &mut approval_handler,
                    )
                    .await?;
                }
            } else {
                let prompt = cli.prompt.join(" ");
                queue.enqueue_message(&session_id, "user", &prompt, "cli")?;
                process_queue(
                    &mut queue,
                    &session_id,
                    &mut history,
                    &turn,
                    &mut provider_factory,
                    &mut token_sink,
                    &mut approval_handler,
                )
                .await?;
            }
        }
    }

    Ok(())
    }

async fn process_queue<F, Fut, P, TS, AH>(
    queue: &mut store::Store,
    session_id: &str,
    session: &mut session::Session,
    turn: &turn::Turn,
    provider_factory: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: llm::LlmProvider,
    TS: agent::TokenSink + Send,
    AH: agent::ApprovalHandler,
{
    while let Some(message) = queue.dequeue_next_message(session_id)? {
        match message.role.as_str() {
            "system" => {
                session.append(llm::ChatMessage::system(message.content), None)?;
                queue.mark_processed(message.id)?;
                continue;
            }
            "assistant" => {
                session.append(
                    llm::ChatMessage {
                        role: llm::ChatRole::Assistant,
                        content: vec![llm::MessageContent::text(message.content)],
                    },
                    None,
                )?;
                queue.mark_processed(message.id)?;
                continue;
            }
            "user" => {}
            other => {
                eprintln!("unsupported queued role '{other}' for message {}", message.id);
                queue.mark_failed(message.id)?;
                continue;
            }
        }

        let verdict = agent::run_agent_loop(
            provider_factory,
            session,
            message.content,
            turn,
            token_sink,
            approval_handler,
        )
        .await;

        match verdict {
            Ok(verdict) => {
                queue.mark_processed(message.id)?;
                match verdict {
                    agent::TurnVerdict::Executed(_) => {}
                    agent::TurnVerdict::Approved { .. } => {
                        eprintln!("Command approved by user and executed.");
                    }
                    agent::TurnVerdict::Denied { reason, gate_id } => {
                        eprintln!("Command hard-denied by {gate_id}: {reason}");
                    }
                }
            }
            Err(err) => {
                queue.mark_failed(message.id)?;
                return Err(err);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FailingProvider;

    impl llm::LlmProvider for FailingProvider {
        async fn stream_completion(
            &self,
            _messages: &[llm::ChatMessage],
            _tools: &[llm::FunctionTool],
            _on_token: &mut (dyn FnMut(String) + Send),
        ) -> Result<llm::StreamedTurn> {
            Err(anyhow!("provider failure"))
        }
    }

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_main_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn process_queue_marks_failed_when_agent_loop_errors() {
        let root = temp_dir("failed_marking");
        std::fs::create_dir_all(&root).unwrap();

        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "worker";
        let mut queue = store::Store::new(&queue_path).unwrap();
        queue.create_session(session_id, None).unwrap();
        let message_id = queue
            .enqueue_message(session_id, "user", "run something", "cli")
            .unwrap();

        let mut session = session::Session::new(&sessions_dir).unwrap();
        let turn = turn::Turn::new();
        let mut provider_factory = || async { Ok::<_, anyhow::Error>(FailingProvider) };
        let mut token_sink = |_token: String| {};
        let mut approval_handler = |_severity: &autopoiesis::guard::Severity, _reason: &str, _command: &str| true;

        let result = process_queue(
            &mut queue,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await;

        assert!(result.is_err());

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn process_queue_persists_system_messages_instead_of_discarding_them() {
        let root = temp_dir("system_queue_message");
        std::fs::create_dir_all(&root).unwrap();

        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "worker";
        let mut queue = store::Store::new(&queue_path).unwrap();
        queue.create_session(session_id, None).unwrap();
        let message_id = queue
            .enqueue_message(session_id, "system", "operational note", "cli")
            .unwrap();

        let mut session = session::Session::new(&sessions_dir).unwrap();
        let turn = turn::Turn::new();
        let mut provider_factory = || async { Ok::<_, anyhow::Error>(FailingProvider) };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &autopoiesis::guard::Severity, _reason: &str, _command: &str| true;

        process_queue(
            &mut queue,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(session.history().len(), 1);
        assert!(matches!(session.history()[0].role, llm::ChatRole::System));
        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn process_queue_marks_unknown_roles_failed() {
        let root = temp_dir("unknown_queue_message");
        std::fs::create_dir_all(&root).unwrap();

        let queue_path = root.join("queue.sqlite");
        let sessions_dir = root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let session_id = "worker";
        let mut queue = store::Store::new(&queue_path).unwrap();
        queue.create_session(session_id, None).unwrap();
        let message_id = queue
            .enqueue_message(session_id, "tool", "orphan tool result", "cli")
            .unwrap();

        let mut session = session::Session::new(&sessions_dir).unwrap();
        let turn = turn::Turn::new();
        let mut provider_factory = || async { Ok::<_, anyhow::Error>(FailingProvider) };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &autopoiesis::guard::Severity, _reason: &str, _command: &str| true;

        process_queue(
            &mut queue,
            session_id,
            &mut session,
            &turn,
            &mut provider_factory,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let conn = rusqlite::Connection::open(&queue_path).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "failed");

        let _ = std::fs::remove_dir_all(&root);
    }
}
