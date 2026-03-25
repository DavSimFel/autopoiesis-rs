//! Agent orchestration facade coordinating model turns and tool execution.

use anyhow::Result;

use crate::config::Config;
use crate::gate::{BudgetSnapshot, Severity};
use crate::llm::LlmProvider;
use crate::session::Session;
use crate::store::{QueuedMessage, Store};
use crate::turn::Turn;

pub use crate::spawn::{SpawnDrainResult, SpawnRequest, SpawnResult};

mod loop_impl;
mod queue;
pub(crate) mod shell_execute;
mod spawn;

pub use loop_impl::{QueueOutcome, TurnVerdict, format_denial_message, run_agent_loop};
pub use queue::drain_queue;
pub use spawn::spawn_and_drain;

/// Receiver of streaming tokens emitted by the model during completion.
pub trait TokenSink {
    fn on_token(&mut self, token: String);
    fn on_complete(&mut self) {}
}

impl<F> TokenSink for F
where
    F: FnMut(String),
{
    fn on_token(&mut self, token: String) {
        self(token)
    }
}

/// Request approval for execution paths that need user confirmation.
pub trait ApprovalHandler {
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool;
}

impl<F> ApprovalHandler for F
where
    F: FnMut(&Severity, &str, &str) -> bool,
{
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        self(severity, reason, command)
    }
}

/// Convenience wrapper for spawning a child session through the shared spawn module.
pub fn spawn_child(
    store: &mut Store,
    config: &Config,
    parent_budget: BudgetSnapshot,
    request: SpawnRequest,
) -> Result<SpawnResult> {
    crate::spawn::spawn_child(store, config, parent_budget, request)
}

pub async fn process_message<F, Fut, P, TS, AH>(
    message: &QueuedMessage,
    session: &mut Session,
    turn: &Turn,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<QueueOutcome>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: LlmProvider,
    TS: TokenSink + Send + ?Sized,
    AH: ApprovalHandler + ?Sized,
{
    queue::process_queued_message(
        message,
        session,
        turn,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await
}

#[cfg(test)]
#[cfg(test)]
pub(crate) mod tests;
