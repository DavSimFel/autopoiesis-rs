use anyhow::{Result, anyhow};
use tracing::info;

use crate::app::args::EnqueueArgs;
use autopoiesis::config;
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::session_registry::{SessionRegistry, SessionSpec};
use autopoiesis::store;

fn render_message(args: &EnqueueArgs) -> Result<String> {
    if args.message.is_empty() {
        return Err(anyhow!("enqueue message must not be empty"));
    }

    Ok(args.message.join(" "))
}

fn validate_enqueue_target(session_id: &str, registry_spec: Option<&SessionSpec>) -> Result<()> {
    match registry_spec {
        Some(spec) if spec.is_queue_owned() => Ok(()),
        Some(spec) => Err(anyhow!(
            "session '{}' is registry-backed but request-owned; use `autopoiesis --session {session_id}`",
            spec.session_id
        )),
        None => Err(anyhow!(
            "session '{session_id}' is not registry-backed; use `autopoiesis --session {session_id}`"
        )),
    }
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

    validate_enqueue_target(&session_id, registry.get(&session_id))?;
    store.ensure_session_row(&session_id)?;

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
    use super::{render_message, validate_enqueue_target};
    use crate::app::args::EnqueueArgs;
    use autopoiesis::config::{
        AgentsConfig, Config, DomainsConfig, ModelsConfig, QueueConfig, ReadToolConfig,
        ShellPolicy, SubscriptionsConfig,
    };
    use autopoiesis::session_registry::SessionSpec;
    use std::path::PathBuf;

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

    #[test]
    fn validate_enqueue_target_allows_queue_owned_registry_session() {
        validate_enqueue_target("silas-t1", Some(&test_session_spec("silas-t1", true))).unwrap();
    }

    #[test]
    fn validate_enqueue_target_rejects_non_registry_session() {
        let err = validate_enqueue_target("ad-hoc", None).unwrap_err();

        assert!(err.to_string().contains("not registry-backed"));
        assert!(err.to_string().contains("autopoiesis --session ad-hoc"));
    }

    #[test]
    fn validate_enqueue_target_rejects_request_owned_registry_session() {
        let err = validate_enqueue_target(
            "analysis-session",
            Some(&test_session_spec("analysis-session", false)),
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("registry-backed but request-owned")
        );
        assert!(
            err.to_string()
                .contains("autopoiesis --session analysis-session")
        );
    }

    fn test_session_spec(session_id: &str, always_on: bool) -> SessionSpec {
        SessionSpec {
            session_id: session_id.to_string(),
            tier: "t1".to_string(),
            config: test_config(),
            description: "test session".to_string(),
            always_on,
        }
    }

    fn test_config() -> Config {
        Config {
            model: "gpt-test".to_string(),
            system_prompt: "system".to_string(),
            base_url: "https://example.test/api".to_string(),
            reasoning_effort: None,
            session_name: None,
            operator_key: None,
            shell_policy: ShellPolicy::default(),
            budget: None,
            read: ReadToolConfig::default(),
            subscriptions: SubscriptionsConfig::default(),
            queue: QueueConfig::default(),
            identity_files: Vec::new(),
            agents: AgentsConfig::default(),
            models: ModelsConfig::default(),
            domains: DomainsConfig::default(),
            skills_dir: PathBuf::from("skills"),
            skills_dir_resolved: PathBuf::from("skills"),
            skills: autopoiesis::skills::SkillCatalog::default(),
            active_agent: None,
        }
    }
}
