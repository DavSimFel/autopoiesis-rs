use anyhow::{Result, anyhow};
use tracing::info;

use crate::app::args::EnqueueArgs;
use autopoiesis::config;
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::session_registry::SessionRegistry;
use autopoiesis::store;

fn render_message(args: &EnqueueArgs) -> Result<String> {
    if args.message.is_empty() {
        return Err(anyhow!("enqueue message must not be empty"));
    }

    Ok(args.message.join(" "))
}

pub(crate) async fn handle_enqueue_command(args: EnqueueArgs) -> Result<()> {
    let config = config::Config::load("agents.toml")?;
    let registry = SessionRegistry::from_config(&config)?;
    let mut store = store::Store::new("sessions/queue.sqlite")?;
    let EnqueueArgs {
        session: session_id,
        message,
    } = args;
    let content = render_message(&EnqueueArgs {
        session: session_id.clone(),
        message,
    })?;

    if registry.get(&session_id).is_some() {
        store.ensure_session_row(&session_id)?;
    } else {
        return Err(anyhow!(
            "session '{session_id}' is not registry-backed; use `autopoiesis run --session {session_id}`"
        ));
    }

    let message_id = store.enqueue_message(&session_id, "user", &content, "cli")?;
    info!(
        target: STDOUT_USER_OUTPUT_TARGET,
        "enqueued message {} for {}",
        message_id,
        session_id
    );
    Ok(())
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::render_message;
    use crate::app::args::EnqueueArgs;

    #[test]
    fn render_message_joins_words_with_spaces() {
        let rendered = render_message(&EnqueueArgs {
            session: "silas-t1".to_string(),
            message: vec!["hello".to_string(), "world".to_string()],
        })
        .unwrap();

        assert_eq!(rendered, "hello world");
    }

    #[test]
    fn render_message_rejects_empty_payload() {
        let err = render_message(&EnqueueArgs {
            session: "silas-t1".to_string(),
            message: Vec::new(),
        })
        .unwrap_err();

        assert!(err.to_string().contains("must not be empty"));
    }
}
