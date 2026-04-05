//! Channel protocol types for TUI ↔ worker communication.

use crate::gate::Severity;
use tokio::sync::oneshot;

/// Events sent from the async worker loop to the TUI render thread.
pub enum TuiEvent {
    /// The worker has started processing a prompt (set busy state).
    PromptStarted,

    /// The worker has finished processing a prompt (clear busy state).
    /// Emitted on all paths: success, denial, and error.
    PromptFinished,

    /// Coalesced streamed token text from the LLM.
    Token(String),

    /// The approval handler needs user input.
    ApprovalRequest {
        severity: Severity,
        reason: String,
        command: String,
        respond: oneshot::Sender<bool>,
    },

    /// A tool call has started (from TuiObserver forwarding TraceEvent).
    ToolCallStarted {
        tool_name: String,
        call_id: String,
        command: Option<String>,
    },

    /// A tool call has completed (from TuiObserver forwarding TraceEvent).
    ToolCallFinished {
        tool_name: String,
        call_id: String,
        status: String,
        exit_code: Option<i64>,
    },

    /// Token count update from a completed turn.
    TurnStats {
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        total_tokens: Option<i64>,
    },

    /// A diagnostic log line (from the tracing diagnostic layer).
    DiagnosticLog(String),

    /// A user-facing system message (from the tracing stderr layer).
    SystemMessage(String),

    /// Session history loaded at startup.
    HistoryLoaded(Vec<HistoryEntry>),

    /// An error occurred in the worker loop.
    AgentError(String),

    /// The worker loop has exited.
    AgentFinished,
}

/// Commands sent from the TUI render thread to the async worker loop.
pub enum TuiCommand {
    /// User submitted a prompt.
    UserPrompt(String),

    /// User requested quit.
    Quit,
}

/// Simplified history entry for transcript hydration at startup.
pub struct HistoryEntry {
    pub role: HistoryRole,
    pub text: String,
}

/// Role of a history entry for rendering purposes.
pub enum HistoryRole {
    User,
    Assistant,
    System,
    ToolActivity {
        tool_name: String,
        command: Option<String>,
    },
}
