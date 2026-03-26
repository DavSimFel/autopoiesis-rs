use anyhow::{Result, anyhow};
use reqwest::Client;
use std::future::Future;
use std::io::{self, BufRead, Write};
use tracing::warn;

use crate::app::args::Cli;
use autopoiesis::agent;
use autopoiesis::config;
use autopoiesis::session;
use autopoiesis::session_runtime::{
    build_openai_provider_factory, build_turn_builder_for_subscriptions, drain_queue_with_store,
    load_subscriptions_for_session,
};
use autopoiesis::store;
use autopoiesis::terminal_ui;

fn resolve_session_id(cli_session: Option<&str>, config_session: Option<&str>) -> String {
    cli_session
        .or(config_session)
        .unwrap_or("default")
        .to_string()
}

struct PromptRunner<'a, F> {
    queue: &'a mut store::Store,
    history: &'a mut session::Session,
    session_id: &'a str,
    provider_config: &'a config::Config,
    provider_factory: &'a mut F,
    token_sink: &'a mut (dyn agent::TokenSink + Send),
    approval_handler: &'a mut (dyn agent::ApprovalHandler + Send),
}

impl<'a, F> PromptRunner<'a, F> {
    fn new(
        queue: &'a mut store::Store,
        history: &'a mut session::Session,
        session_id: &'a str,
        provider_config: &'a config::Config,
        provider_factory: &'a mut F,
        token_sink: &'a mut (dyn agent::TokenSink + Send),
        approval_handler: &'a mut (dyn agent::ApprovalHandler + Send),
    ) -> Self {
        Self {
            queue,
            history,
            session_id,
            provider_config,
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
        let mut turn_builder =
            build_turn_builder_for_subscriptions(self.provider_config.clone(), subscriptions);
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

    let session_id = resolve_session_id(cli.session.as_deref(), config.session_name.as_deref());
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
    let http_client = Client::new();
    let mut provider_factory = build_openai_provider_factory(http_client, provider_config.clone());

    let mut token_sink = terminal_ui::CliTokenSink::new();
    let mut approval_handler = terminal_ui::CliApprovalHandler::new();
    let mut runner = PromptRunner::new(
        &mut queue,
        &mut history,
        &session_id,
        &provider_config,
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
