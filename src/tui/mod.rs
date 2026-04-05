//! Optional ratatui-based TUI for direct interactive CLI sessions.
//!
//! Architecture: a dedicated OS thread owns the terminal render loop, while
//! an async worker loop on a tokio task owns all mutable session state and
//! processes prompts serially.

pub mod bridge;
pub mod event;
pub mod input;
pub mod render;
pub mod state;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event as ct_event;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config;
use crate::context::SessionManifest;
use crate::llm::{ChatMessage, LlmProvider, MessageContent};
use crate::observe::{MultiObserver, Observer, runtime_observer};
use crate::session::Session;
use crate::session_runtime::{
    build_turn_builder_for_subscriptions_with_manifest, drain_queue_with_store_observed,
    load_subscriptions_for_session,
};
use crate::store::Store;

use self::bridge::{TuiApprovalHandler, TuiObserver, TuiTokenSink};
use self::event::{HistoryEntry, HistoryRole, TuiCommand, TuiEvent};
use self::input::InputResult;
use self::state::{InputMode, OutputLine, PendingApproval, TuiState};

/// RAII guard that restores the terminal on drop.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Entry point for the TUI mode.
///
/// Called from `session_run::run()` when `--tui` is active.  This function:
/// 1. Creates channels for TUI ↔ worker communication.
/// 2. Spawns a dedicated OS thread for the ratatui render loop.
/// 3. Runs the async worker loop on the current tokio task.
/// 4. Joins the render thread and restores the terminal on exit.
#[allow(clippy::too_many_arguments)]
pub async fn run_tui<F, Fut, P>(
    session_id: String,
    model_label: String,
    mut store: Store,
    mut session: Session,
    provider_config: config::Config,
    session_manifest: Option<SessionManifest>,
    mut provider_factory: F,
    event_tx: mpsc::UnboundedSender<TuiEvent>,
    event_rx: mpsc::UnboundedReceiver<TuiEvent>,
) -> Result<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<TuiCommand>();

    // Build the observer stack: runtime observer + TUI observer.
    let tui_observer = Arc::new(TuiObserver::new(event_tx.clone()));
    let runtime_obs = runtime_observer(session.sessions_dir());
    let observer: Arc<dyn Observer> = Arc::new(MultiObserver::new(vec![runtime_obs, tui_observer]));

    // Hydrate initial state from session history.
    let history_entries = hydrate_history(session.history());
    let _ = event_tx.send(TuiEvent::HistoryLoaded(history_entries));

    // Spawn the render thread.
    let display_name = session_id.clone();
    let render_handle = std::thread::spawn(move || {
        run_render_loop(event_rx, cmd_tx, display_name, model_label);
    });

    loop {
        let Some(cmd) = cmd_rx.recv().await else {
            break; // render thread exited, channel closed
        };
        match cmd {
            TuiCommand::UserPrompt(prompt) => {
                let _ = event_tx.send(TuiEvent::PromptStarted);

                let result = process_prompt(
                    &mut store,
                    &mut session,
                    &session_id,
                    &provider_config,
                    session_manifest.as_ref(),
                    &observer,
                    &mut provider_factory,
                    &event_tx,
                    &prompt,
                )
                .await;

                if let Err(err) = result {
                    let _ = event_tx.send(TuiEvent::AgentError(err.to_string()));
                }

                let _ = event_tx.send(TuiEvent::PromptFinished);
            }
            TuiCommand::Quit => {
                break;
            }
        }
    }

    let _ = event_tx.send(TuiEvent::AgentFinished);
    drop(event_tx);

    // Wait for the render thread to finish.
    let _ = render_handle.join();

    Ok(())
}

// ---------------------------------------------------------------------------
// Worker helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn process_prompt<F, Fut, P>(
    store: &mut Store,
    session: &mut Session,
    session_id: &str,
    provider_config: &config::Config,
    session_manifest: Option<&SessionManifest>,
    observer: &Arc<dyn Observer>,
    provider_factory: &mut F,
    event_tx: &mpsc::UnboundedSender<TuiEvent>,
    prompt: &str,
) -> Result<()>
where
    F: FnMut() -> Fut + Send,
    Fut: std::future::Future<Output = Result<P>> + Send,
    P: LlmProvider + Send,
{
    store.enqueue_message(session_id, "user", prompt, "tui")?;
    let subscriptions = load_subscriptions_for_session(store, session_id)?;
    let mut turn_builder = build_turn_builder_for_subscriptions_with_manifest(
        provider_config.clone(),
        subscriptions,
        session_manifest.cloned(),
    );

    let mut token_sink = TuiTokenSink::new(event_tx.clone());
    let mut approval_handler = TuiApprovalHandler::new(event_tx.clone());

    let (verdict, _, _) = drain_queue_with_store_observed(
        store,
        observer.clone(),
        session_id,
        session,
        &mut turn_builder,
        provider_factory,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?;

    if let Some(crate::agent::TurnVerdict::Denied { reason, gate_id }) = verdict {
        let msg = crate::agent::format_denial_message(&reason, &gate_id);
        let _ = event_tx.send(TuiEvent::SystemMessage(msg));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// History hydration
// ---------------------------------------------------------------------------

fn hydrate_history(messages: &[ChatMessage]) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    for msg in messages {
        match msg.role {
            crate::llm::ChatRole::User => {
                let text = extract_text_content(&msg.content);
                if !text.is_empty() {
                    entries.push(HistoryEntry {
                        role: HistoryRole::User,
                        text,
                    });
                }
            }
            crate::llm::ChatRole::Assistant => {
                // Text blocks become transcript rows.
                let text = extract_text_content(&msg.content);
                if !text.is_empty() {
                    entries.push(HistoryEntry {
                        role: HistoryRole::Assistant,
                        text,
                    });
                }
                // Tool-call blocks become completed tool-activity rows.
                for content in &msg.content {
                    if let MessageContent::ToolCall { call } = content {
                        let command = crate::llm::ToolCall::parse_command(&call.arguments);
                        entries.push(HistoryEntry {
                            role: HistoryRole::ToolActivity {
                                tool_name: call.name.clone(),
                                command,
                            },
                            text: String::new(),
                        });
                    }
                }
            }
            crate::llm::ChatRole::System => {
                let text = extract_text_content(&msg.content);
                if !text.is_empty() {
                    entries.push(HistoryEntry {
                        role: HistoryRole::System,
                        text,
                    });
                }
            }
            // Raw tool-result messages are not rendered as separate startup rows.
            crate::llm::ChatRole::Tool => {}
        }
    }
    entries
}

fn extract_text_content(content: &[MessageContent]) -> String {
    content
        .iter()
        .filter_map(|c| {
            if let MessageContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Render loop (runs on a dedicated OS thread)
// ---------------------------------------------------------------------------

fn run_render_loop(
    mut event_rx: mpsc::UnboundedReceiver<TuiEvent>,
    cmd_tx: mpsc::UnboundedSender<TuiCommand>,
    session_name: String,
    model_label: String,
) {
    let _guard = TerminalGuard;

    if enable_raw_mode().is_err() || execute!(io::stdout(), EnterAlternateScreen).is_err() {
        return;
    }

    let backend = CrosstermBackend::new(io::stdout());
    let Ok(mut terminal) = Terminal::new(backend) else {
        return;
    };

    let mut state = TuiState::new(session_name, model_label);

    loop {
        // Draw.
        if terminal.draw(|f| render::draw(f, &state)).is_err() {
            break;
        }

        // Poll crossterm events (50ms timeout).
        if ct_event::poll(Duration::from_millis(50)).unwrap_or(false)
            && let Ok(event) = ct_event::read()
        {
            match input::handle_event(event, &mut state) {
                InputResult::Command(cmd) => {
                    let is_quit = matches!(cmd, TuiCommand::Quit);
                    let _ = cmd_tx.send(cmd);
                    if is_quit {
                        break;
                    }
                }
                InputResult::ForceQuit => {
                    break;
                }
                InputResult::None => {}
            }
        }

        // Drain all pending TUI events.
        while let Ok(event) = event_rx.try_recv() {
            match event {
                TuiEvent::PromptStarted => {
                    state.is_processing = true;
                }
                TuiEvent::PromptFinished => {
                    state.is_processing = false;
                }
                TuiEvent::Token(text) => {
                    state.push_token(&text);
                }
                TuiEvent::ApprovalRequest {
                    severity,
                    reason,
                    command,
                    respond,
                } => {
                    state.pending_approval = Some(PendingApproval {
                        severity,
                        reason,
                        command,
                        respond,
                    });
                    state.mode = InputMode::Approval;
                }
                TuiEvent::ToolCallStarted {
                    tool_name,
                    call_id,
                    command,
                } => {
                    state.tool_started(tool_name, call_id, command);
                }
                TuiEvent::ToolCallFinished {
                    tool_name,
                    call_id,
                    status,
                    exit_code,
                } => {
                    state.tool_finished(&call_id, &tool_name, &status, exit_code);
                }
                TuiEvent::TurnStats {
                    prompt_tokens,
                    completion_tokens,
                    total_tokens,
                } => {
                    state.update_turn_stats(prompt_tokens, completion_tokens, total_tokens);
                }
                TuiEvent::DiagnosticLog(_) => {
                    // Diagnostic logs are silently dropped in v1.
                    // Future: optional debug panel.
                }
                TuiEvent::SystemMessage(msg) => {
                    state.push_system_message(&msg);
                }
                TuiEvent::HistoryLoaded(entries) => {
                    for entry in entries {
                        match entry.role {
                            HistoryRole::User => state.push_user_prompt(&entry.text),
                            HistoryRole::Assistant => state.push_token(&entry.text),
                            HistoryRole::System => state.push_system_message(&entry.text),
                            HistoryRole::ToolActivity { tool_name, command } => {
                                state.output.push(OutputLine::ToolActivity {
                                    tool_name,
                                    command,
                                    status: crate::tui::state::ToolStatus::Completed {
                                        exit_code: None,
                                    },
                                });
                            }
                        }
                    }
                }
                TuiEvent::AgentError(msg) => {
                    state.push_system_message(&format!("Error: {msg}"));
                }
                TuiEvent::AgentFinished => {
                    break;
                }
            }
        }

        if state.quit_requested && !state.is_processing {
            break;
        }
    }
}
