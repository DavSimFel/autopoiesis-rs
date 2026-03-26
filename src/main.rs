//! Binary entrypoint for the `autopoiesis` CLI.

mod app;

use anyhow::{Result, anyhow};
use app::tracing as app_tracing;
use app::{args, plan_commands, session_run, subscription_commands};
use autopoiesis::auth;
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::server;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    app_tracing::init_tracing();
    let cli = args::Cli::parse();

    match cli.command {
        Some(args::Commands::Auth { action }) => match action {
            args::AuthAction::Login => {
                let tokens = auth::device_code_login().await?;
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "Logged in. Token expiry: {}",
                    tokens.expires_at
                );
            }
            args::AuthAction::Status => {
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
            args::AuthAction::Logout => {
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
        Some(args::Commands::Serve { port }) => {
            server::run(port).await?;
        }
        Some(args::Commands::Plan { action }) => {
            plan_commands::handle_plan_command(action).await?;
        }
        Some(args::Commands::Sub { action }) => {
            subscription_commands::handle_subscription_command(action).await?;
        }
        None => {
            session_run::run(&cli).await?;
        }
    }

    Ok(())
}
