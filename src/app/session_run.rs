use anyhow::{Result, anyhow};
use reqwest::Client;
use std::future::Future;
use std::io::{self, BufRead, Write};
use tracing::warn;

use crate::app::args::Cli;
use autopoiesis::agent;
use autopoiesis::config;
use autopoiesis::context::SessionManifest;
use autopoiesis::session;
use autopoiesis::session_registry::{SessionRegistry, SessionSpec};
use autopoiesis::session_runtime::{
    build_openai_provider_factory, build_turn_builder_for_subscriptions_with_manifest,
    drain_queue_with_store, load_subscriptions_for_session,
};
use autopoiesis::store;
use autopoiesis::terminal_ui;

fn resolve_session_id(
    cli_session: Option<&str>,
    config_session: Option<&str>,
    registry_default: Option<&str>,
    queue_owned_hint: Option<&str>,
) -> Result<String> {
    if let Some(session_id) = cli_session.or(config_session).or(registry_default) {
        return Ok(session_id.to_string());
    }

    if let Some(session_id) = queue_owned_hint {
        return Err(anyhow!(
            "no direct CLI default session configured; registry sessions like '{session_id}' are queue-owned; use `autopoiesis enqueue --session {session_id}`"
        ));
    }

    Err(anyhow!("no default session configured; pass --session"))
}

fn ensure_direct_run_target(session_id: &str, registry_spec: Option<&SessionSpec>) -> Result<()> {
    if let Some(spec) = registry_spec
        && spec.is_queue_owned()
    {
        return Err(anyhow!(
            "session '{}' is queue-owned; use `autopoiesis enqueue --session {session_id}`",
            spec.session_id
        ));
    }

    Ok(())
}

struct PromptRunner<'a, F> {
    queue: &'a mut store::Store,
    history: &'a mut session::Session,
    session_id: &'a str,
    runtime: PromptRunnerRuntime<'a>,
    provider_factory: &'a mut F,
    token_sink: &'a mut (dyn agent::TokenSink + Send),
    approval_handler: &'a mut (dyn agent::ApprovalHandler + Send),
}

struct PromptRunnerRuntime<'a> {
    provider_config: &'a config::Config,
    session_manifest: Option<&'a SessionManifest>,
}

impl<'a, F> PromptRunner<'a, F> {
    fn new(
        queue: &'a mut store::Store,
        history: &'a mut session::Session,
        session_id: &'a str,
        runtime: PromptRunnerRuntime<'a>,
        provider_factory: &'a mut F,
        token_sink: &'a mut (dyn agent::TokenSink + Send),
        approval_handler: &'a mut (dyn agent::ApprovalHandler + Send),
    ) -> Self {
        Self {
            queue,
            history,
            session_id,
            runtime,
            provider_factory,
            token_sink,
            approval_handler,
        }
    }
}

impl<'a, F, Fut, P> PromptRunner<'a, F>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: autopoiesis::llm::LlmProvider + Send,
{
    async fn process_prompt(&mut self, prompt: &str) -> Result<Option<agent::TurnVerdict>> {
        self.queue
            .enqueue_message(self.session_id, "user", prompt, "cli")?;
        let subscriptions = load_subscriptions_for_session(self.queue, self.session_id)?;
        let mut turn_builder = build_turn_builder_for_subscriptions_with_manifest(
            self.runtime.provider_config.clone(),
            subscriptions,
            self.runtime.session_manifest.cloned(),
        );
        let (verdict, _, _) = drain_queue_with_store(
            &mut *self.queue,
            self.session_id,
            self.history,
            &mut turn_builder,
            self.provider_factory,
            self.token_sink,
            self.approval_handler,
        )
        .await?;
        Ok(verdict)
    }
}

async fn run_interactive_loop<F, Fut, P>(runner: &mut PromptRunner<'_, F>) -> Result<()>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<P>> + Send,
    P: autopoiesis::llm::LlmProvider + Send,
{
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

        if let Some(agent::TurnVerdict::Denied { reason, gate_id }) =
            runner.process_prompt(prompt).await?
        {
            warn!(
                target: autopoiesis::logging::STDERR_USER_OUTPUT_TARGET,
                "{}",
                agent::format_denial_message(&reason, &gate_id)
            );
        }
    }
    Ok(())
}

pub(crate) async fn run(cli: &Cli) -> Result<()> {
    let config = config::Config::load("agents.toml")
        .map_err(|error| anyhow!("failed to load configuration: {error}"))?;
    let registry = SessionRegistry::from_config(&config)
        .map_err(|error| anyhow!("failed to build session registry: {error}"))?;
    let registry_default = registry
        .default_request_owned_session()
        .map(|spec| spec.session_id.as_str());
    let queue_owned_hint = registry
        .sessions()
        .into_iter()
        .find(|spec| spec.is_queue_owned())
        .map(|spec| spec.session_id.as_str());

    let session_id = resolve_session_id(
        cli.session.as_deref(),
        config.session_name.as_deref(),
        registry_default,
        queue_owned_hint,
    )?;
    let registry_spec = registry.get(&session_id).cloned();
    ensure_direct_run_target(&session_id, registry_spec.as_ref())?;

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
    let provider_config = registry_spec
        .as_ref()
        .map(|spec| spec.config.clone())
        .unwrap_or_else(|| config.clone());
    if registry_spec.is_some() {
        queue.ensure_session_row(&session_id)?;
    } else {
        queue.create_session(&session_id, Some(r#"{"source":"cli"}"#))?;
    }

    let http_client = Client::new();
    let mut provider_factory = build_openai_provider_factory(http_client, provider_config.clone());
    let session_manifest = registry_spec
        .as_ref()
        .map(|_| SessionManifest::from_registry(&registry));

    let mut token_sink = terminal_ui::CliTokenSink::new();
    let mut approval_handler = terminal_ui::CliApprovalHandler::new();
    let mut runner = PromptRunner::new(
        &mut queue,
        &mut history,
        &session_id,
        PromptRunnerRuntime {
            provider_config: &provider_config,
            session_manifest: session_manifest.as_ref(),
        },
        &mut provider_factory,
        &mut token_sink,
        &mut approval_handler,
    );

    if cli.prompt.is_empty() {
        run_interactive_loop(&mut runner).await?;
    } else {
        let prompt = cli.prompt.join(" ");
        if let Some(agent::TurnVerdict::Denied { reason, gate_id }) =
            runner.process_prompt(&prompt).await?
        {
            warn!(
                target: autopoiesis::logging::STDERR_USER_OUTPUT_TARGET,
                "{}",
                agent::format_denial_message(&reason, &gate_id)
            );
        }
    }

    Ok(())
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::{ensure_direct_run_target, resolve_session_id};
    use crate::app::args::Cli;
    use autopoiesis::config::{
        AgentsConfig, Config, DomainsConfig, ModelsConfig, QueueConfig, ReadToolConfig,
        ShellPolicy, SubscriptionsConfig,
    };
    use autopoiesis::session_registry::SessionSpec;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resolve_session_id_prefers_cli_value() {
        assert_eq!(
            resolve_session_id(
                Some("fix-auth"),
                Some("configured-default"),
                Some("silas-t1"),
                Some("silas-t2"),
            )
            .unwrap(),
            "fix-auth"
        );
    }

    #[test]
    fn resolve_session_id_uses_configured_default_when_cli_missing() {
        assert_eq!(
            resolve_session_id(
                None,
                Some("configured-default"),
                Some("analysis-session"),
                Some("silas-t1"),
            )
            .unwrap(),
            "configured-default"
        );
    }

    #[test]
    fn resolve_session_id_falls_back_to_registry_default() {
        assert_eq!(
            resolve_session_id(None, None, Some("analysis-session"), Some("silas-t1")).unwrap(),
            "analysis-session"
        );
    }

    #[test]
    fn resolve_session_id_errors_without_any_default() {
        assert!(resolve_session_id(None, None, None, None).is_err());
    }

    #[test]
    fn resolve_session_id_reports_queue_owned_hint_when_no_direct_default_exists() {
        let err = resolve_session_id(None, None, None, Some("silas-t1")).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("no direct CLI default session configured"));
        assert!(message.contains("queue-owned"));
        assert!(message.contains("autopoiesis enqueue --session silas-t1"));
    }

    #[test]
    fn ensure_direct_run_target_rejects_queue_owned_registry_session() {
        let err = ensure_direct_run_target("silas-t1", Some(&test_session_spec("silas-t1", true)))
            .unwrap_err();

        assert!(err.to_string().contains("queue-owned"));
        assert!(
            err.to_string()
                .contains("autopoiesis enqueue --session silas-t1")
        );
    }

    #[test]
    fn ensure_direct_run_target_allows_request_owned_registry_session() {
        ensure_direct_run_target(
            "analysis-session",
            Some(&test_session_spec("analysis-session", false)),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn run_reports_queue_owned_hint_when_only_registry_sessions_exist() {
        let _cwd_guard = crate::app::test_cwd_lock().lock().await;
        let temp_root = temp_root("session_run_queue_owned_hint");
        fs::create_dir_all(temp_root.join("sessions")).unwrap();
        fs::write(temp_root.join("agents.toml"), queue_owned_only_config()).unwrap();
        let _restore_dir = set_current_dir_guard(&temp_root);

        let err = super::run(&Cli {
            command: None,
            session: None,
            prompt: Vec::new(),
        })
        .await
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("no direct CLI default session configured"));
        assert!(message.contains("autopoiesis enqueue --session silas-t1"));
    }

    #[tokio::test]
    async fn run_rejects_explicit_queue_owned_registry_session() {
        let _cwd_guard = crate::app::test_cwd_lock().lock().await;
        let temp_root = temp_root("session_run_explicit_queue_owned");
        fs::create_dir_all(temp_root.join("sessions")).unwrap();
        fs::write(temp_root.join("agents.toml"), queue_owned_only_config()).unwrap();
        let _restore_dir = set_current_dir_guard(&temp_root);

        let err = super::run(&Cli {
            command: None,
            session: Some("silas-t1".to_string()),
            prompt: Vec::new(),
        })
        .await
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("session 'silas-t1' is queue-owned"));
        assert!(message.contains("autopoiesis enqueue --session silas-t1"));
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

    fn temp_root(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn queue_owned_only_config() -> &'static str {
        "[agents.silas]\nidentity = \"silas\"\n\n[agents.silas.t1]\nmodel = \"gpt-5.4-mini\"\n\n[agents.silas.t2]\nmodel = \"gpt-5.4-mini\"\n"
    }

    fn set_current_dir_guard(path: &std::path::Path) -> RestoreDir {
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        RestoreDir(old_dir)
    }

    struct RestoreDir(PathBuf);

    impl Drop for RestoreDir {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
}
