//! Trait implementations bridging the agent loop to the TUI event channel.
//!
//! - [`TuiTokenSink`]: Coalescing [`TokenSink`] that batches tokens before sending.
//! - [`TuiApprovalHandler`]: Sends approval requests to the TUI and blocks via oneshot.
//! - [`TuiObserver`]: Forwards select [`TraceEvent`]s as [`TuiEvent`]s.
//! - [`TuiLogWriter`]: Line-buffered [`Write`] that emits tracing output as TUI events.

use std::io::{self, Write};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::agent::{ApprovalHandler, TokenSink};
use crate::gate::Severity;
use crate::observe::{Observer, TraceEvent};

use super::event::TuiEvent;

// ---------------------------------------------------------------------------
// TuiTokenSink
// ---------------------------------------------------------------------------

const COALESCE_THRESHOLD: usize = 256;

/// Token sink that coalesces streamed chunks and flushes on newline or size
/// threshold.  Remaining bytes are flushed in [`TokenSink::on_complete`].
pub struct TuiTokenSink {
    tx: UnboundedSender<TuiEvent>,
    buffer: String,
}

impl TuiTokenSink {
    pub fn new(tx: UnboundedSender<TuiEvent>) -> Self {
        Self {
            tx,
            buffer: String::new(),
        }
    }

    fn flush_buffer(&mut self) {
        if !self.buffer.is_empty() {
            let chunk = std::mem::take(&mut self.buffer);
            let _ = self.tx.send(TuiEvent::Token(chunk));
        }
    }
}

impl TokenSink for TuiTokenSink {
    fn on_token(&mut self, token: String) {
        self.buffer.push_str(&token);
        if self.buffer.len() >= COALESCE_THRESHOLD || self.buffer.contains('\n') {
            self.flush_buffer();
        }
    }

    fn on_complete(&mut self) {
        self.flush_buffer();
    }
}

// ---------------------------------------------------------------------------
// TuiApprovalHandler
// ---------------------------------------------------------------------------

/// Approval handler that sends a request to the TUI render thread and blocks
/// until the user responds.  Uses [`tokio::task::block_in_place`] following
/// the same pattern as the WebSocket approval handler.
pub struct TuiApprovalHandler {
    tx: UnboundedSender<TuiEvent>,
}

impl TuiApprovalHandler {
    pub fn new(tx: UnboundedSender<TuiEvent>) -> Self {
        Self { tx }
    }
}

impl ApprovalHandler for TuiApprovalHandler {
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        let (respond_tx, respond_rx) = oneshot::channel();
        let event = TuiEvent::ApprovalRequest {
            severity: *severity,
            reason: reason.to_string(),
            command: command.to_string(),
            respond: respond_tx,
        };
        if self.tx.send(event).is_err() {
            return false;
        }
        tokio::task::block_in_place(|| respond_rx.blocking_recv().unwrap_or(false))
    }
}

// ---------------------------------------------------------------------------
// TuiObserver
// ---------------------------------------------------------------------------

/// Observer that forwards select trace events to the TUI channel.
pub struct TuiObserver {
    tx: UnboundedSender<TuiEvent>,
}

impl TuiObserver {
    pub fn new(tx: UnboundedSender<TuiEvent>) -> Self {
        Self { tx }
    }
}

impl Observer for TuiObserver {
    fn emit(&self, event: &TraceEvent) {
        let tui_event = match event {
            TraceEvent::ToolCallStarted {
                tool_name,
                call_id,
                command,
                ..
            } => Some(TuiEvent::ToolCallStarted {
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
                command: command.clone(),
            }),
            TraceEvent::ToolCallFinished {
                tool_name,
                call_id,
                status,
                exit_code,
                ..
            } => Some(TuiEvent::ToolCallFinished {
                tool_name: tool_name.clone(),
                call_id: call_id.clone(),
                status: status.clone(),
                exit_code: *exit_code,
            }),
            TraceEvent::TurnFinished {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => Some(TuiEvent::TurnStats {
                prompt_tokens: *prompt_tokens,
                completion_tokens: *completion_tokens,
                total_tokens: *total_tokens,
            }),
            _ => None,
        };
        if let Some(e) = tui_event {
            let _ = self.tx.send(e);
        }
    }
}

// ---------------------------------------------------------------------------
// TuiLogWriter
// ---------------------------------------------------------------------------

/// Line-buffered writer that emits complete lines as TUI events.
///
/// Bytes are accumulated until a newline is encountered, then the complete
/// line (without trailing newline) is sent as a [`TuiEvent`].  This ensures
/// tracing output is line-stable and never tears a partial line into the
/// TUI output buffer.
pub struct TuiLogWriter {
    tx: UnboundedSender<TuiEvent>,
    buffer: Vec<u8>,
    make_event: fn(String) -> TuiEvent,
}

impl TuiLogWriter {
    /// Create a writer that emits [`TuiEvent::SystemMessage`] for each line.
    pub fn system(tx: UnboundedSender<TuiEvent>) -> Self {
        Self {
            tx,
            buffer: Vec::new(),
            make_event: TuiEvent::SystemMessage,
        }
    }

    /// Create a writer that emits [`TuiEvent::DiagnosticLog`] for each line.
    pub fn diagnostic(tx: UnboundedSender<TuiEvent>) -> Self {
        Self {
            tx,
            buffer: Vec::new(),
            make_event: TuiEvent::DiagnosticLog,
        }
    }

    fn drain_lines(&mut self) {
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
            if !text.is_empty() {
                let _ = self.tx.send((self.make_event)(text));
            }
        }
    }
}

impl Write for TuiLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        self.drain_lines();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Clone for TuiLogWriter {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            buffer: Vec::new(),
            make_event: self.make_event,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn token_sink_coalesces_small_tokens() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = TuiTokenSink::new(tx);
        sink.on_token("a".into());
        sink.on_token("b".into());
        // Below threshold, no newline — nothing sent yet.
        assert!(rx.try_recv().is_err());
        sink.on_complete();
        match rx.try_recv() {
            Ok(TuiEvent::Token(text)) => assert_eq!(text, "ab"),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn token_sink_flushes_on_newline() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = TuiTokenSink::new(tx);
        sink.on_token("hello\n".into());
        match rx.try_recv() {
            Ok(TuiEvent::Token(text)) => assert_eq!(text, "hello\n"),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn token_sink_flushes_on_size_threshold() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = TuiTokenSink::new(tx);
        let big = "x".repeat(COALESCE_THRESHOLD);
        sink.on_token(big.clone());
        match rx.try_recv() {
            Ok(TuiEvent::Token(text)) => assert_eq!(text.len(), COALESCE_THRESHOLD),
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn log_writer_buffers_until_newline() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut writer = TuiLogWriter::system(tx);
        writer.write_all(b"partial").ok();
        assert!(rx.try_recv().is_err());
        writer.write_all(b" line\n").ok();
        match rx.try_recv() {
            Ok(TuiEvent::SystemMessage(text)) => assert_eq!(text, "partial line"),
            other => panic!("expected SystemMessage, got {other:?}"),
        }
    }

    #[test]
    fn log_writer_handles_multiple_lines_in_one_write() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut writer = TuiLogWriter::diagnostic(tx);
        writer.write_all(b"line1\nline2\n").ok();
        match rx.try_recv() {
            Ok(TuiEvent::DiagnosticLog(text)) => assert_eq!(text, "line1"),
            other => panic!("expected DiagnosticLog line1, got {other:?}"),
        }
        match rx.try_recv() {
            Ok(TuiEvent::DiagnosticLog(text)) => assert_eq!(text, "line2"),
            other => panic!("expected DiagnosticLog line2, got {other:?}"),
        }
    }

    // TuiEvent is not Debug, so we use a helper for assertions in tests above.
    impl std::fmt::Debug for TuiEvent {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Token(t) => write!(f, "Token({t:?})"),
                Self::SystemMessage(t) => write!(f, "SystemMessage({t:?})"),
                Self::DiagnosticLog(t) => write!(f, "DiagnosticLog({t:?})"),
                _ => write!(f, "<TuiEvent>"),
            }
        }
    }
}
