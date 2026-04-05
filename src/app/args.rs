use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "autopoiesis", version, about = "MVP Rust agent runtime")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,

    #[arg(
        long,
        value_name = "name",
        help = "Persistent session name for CLI mode"
    )]
    pub(crate) session: Option<String>,

    #[arg(long, help = "Launch interactive TUI mode (requires --features tui)")]
    pub(crate) tui: bool,

    #[arg(help = "Prompt for the agent", trailing_var_arg = true)]
    pub(crate) prompt: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    Serve {
        #[arg(short, long, default_value_t = 8423)]
        port: u16,
    },
    Plan {
        #[command(subcommand)]
        action: PlanCommand,
    },
    Enqueue(EnqueueArgs),
    Sub {
        #[command(subcommand)]
        action: SubscriptionCommand,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuthAction {
    Login,
    Status,
    Logout,
}

#[derive(Subcommand)]
pub(crate) enum SubscriptionCommand {
    Add(SubscriptionAddArgs),
    Remove(SubscriptionRemoveArgs),
    List(SubscriptionListArgs),
}

#[derive(Subcommand)]
pub(crate) enum PlanCommand {
    Status(PlanStatusArgs),
    List(PlanListArgs),
    Resume(PlanRunIdArgs),
    Cancel(PlanRunIdArgs),
}

#[derive(Args)]
pub(crate) struct SubscriptionAddArgs {
    pub(crate) path: String,

    #[arg(long)]
    pub(crate) topic: Option<String>,

    #[arg(long)]
    pub(crate) lines: Option<String>,

    #[arg(long)]
    pub(crate) regex: Option<String>,

    #[arg(long)]
    pub(crate) head: Option<usize>,

    #[arg(long)]
    pub(crate) tail: Option<usize>,

    #[arg(
        long,
        help = "Limited jq subset: .field, .field[index], .field[], and pipelines with |"
    )]
    pub(crate) jq: Option<String>,
}

#[derive(Args)]
pub(crate) struct SubscriptionRemoveArgs {
    pub(crate) path: String,

    #[arg(long)]
    pub(crate) topic: Option<String>,
}

#[derive(Args)]
pub(crate) struct SubscriptionListArgs {
    #[arg(long)]
    pub(crate) topic: Option<String>,
}

#[derive(Args)]
pub(crate) struct PlanStatusArgs {
    pub(crate) plan_run_id: Option<String>,
}

#[derive(Args)]
pub(crate) struct PlanRunIdArgs {
    pub(crate) plan_run_id: String,
}

#[derive(Args)]
pub(crate) struct PlanListArgs {
    #[arg(long, default_value_t = 10)]
    pub(crate) limit: usize,
}

#[derive(Args)]
pub(crate) struct EnqueueArgs {
    #[arg(long)]
    pub(crate) session: String,

    #[arg(help = "Message to enqueue", trailing_var_arg = true)]
    pub(crate) message: Vec<String>,
}

#[cfg(all(test, not(clippy)))]
mod tests {
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

    #[test]
    fn parses_plan_status_command() {
        let cli = Cli::parse_from(["autopoiesis", "plan", "status", "plan-1"]);
        match cli.command {
            Some(Commands::Plan {
                action: PlanCommand::Status(args),
            }) => assert_eq!(args.plan_run_id.as_deref(), Some("plan-1")),
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_plan_status_command_without_id() {
        let cli = Cli::parse_from(["autopoiesis", "plan", "status"]);
        match cli.command {
            Some(Commands::Plan {
                action: PlanCommand::Status(args),
            }) => assert_eq!(args.plan_run_id, None),
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_plan_list_command() {
        let cli = Cli::parse_from(["autopoiesis", "plan", "list", "--limit", "3"]);
        match cli.command {
            Some(Commands::Plan {
                action: PlanCommand::List(args),
            }) => assert_eq!(args.limit, 3),
            _ => panic!("unexpected command variant"),
        }
    }

    #[test]
    fn parses_enqueue_command() {
        let cli = Cli::parse_from(["autopoiesis", "enqueue", "--session", "silas-t1", "hello"]);
        match cli.command {
            Some(Commands::Enqueue(args)) => {
                assert_eq!(args.session, "silas-t1");
                assert_eq!(args.message, vec!["hello".to_string()]);
            }
            _ => panic!("unexpected command variant"),
        }
    }
}
