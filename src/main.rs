//! Binary entrypoint for the `autopoiesis` CLI.
#![cfg_attr(test, allow(clippy::all))]

mod app;

use anyhow::{Result, anyhow};
use app::tracing as app_tracing;
use app::{args, enqueue_command, plan_commands, session_run, subscription_commands};
use autopoiesis::auth;
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::server;
use clap::Parser;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = args::Cli::parse();

    // Reject --tui when a subcommand is present.
    if cli.tui && cli.command.is_some() {
        return Err(anyhow!(
            "--tui is only supported for interactive mode (no subcommand)"
        ));
    }

    match cli.command {
        Some(cmd) => {
            // Subcommands use plain tracing.
            app_tracing::init_tracing();

            match cmd {
                args::Commands::Auth { action } => match action {
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
                args::Commands::Serve { port } => {
                    server::run(port).await?;
                }
                args::Commands::Plan { action } => {
                    plan_commands::handle_plan_command(action).await?;
                }
                args::Commands::Enqueue(enqueue_args) => {
                    enqueue_command::handle_enqueue_command(enqueue_args).await?;
                }
                args::Commands::Sub { action } => {
                    subscription_commands::handle_subscription_command(action).await?;
                }
            }
        }
        None => {
            // Tracing is initialized inside run() after determining tui vs cli mode.
            session_run::run(&cli).await?;
        }
    }

    Ok(())
}
