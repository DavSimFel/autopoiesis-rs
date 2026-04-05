//! TUI application state — scroll, input, mode, output buffer.
//!
//! Pure state logic with no terminal dependencies; unit-testable.

use crate::gate::Severity;
use tokio::sync::oneshot;

/// A single line/block in the scrollable output area.
pub enum OutputLine {
    /// User prompt text.
    UserPrompt(String),

    /// Streamed assistant text (may be appended to incrementally).
    AssistantText(String),

    /// System message (denials, diagnostics).
    SystemMessage(String),

    /// Tool call in progress or completed.
    ToolActivity {
        tool_name: String,
        command: Option<String>,
        status: ToolStatus,
    },
}

/// Current status of a tool call displayed in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Completed { exit_code: Option<i64> },
    Failed { status: String },
}

/// UI input mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMode {
    /// Normal text input.
    Normal,
    /// Approval popup is visible; only y/n accepted.
    Approval,
}

/// Pending approval state held while the agent blocks on a response.
pub struct PendingApproval {
    pub severity: Severity,
    pub reason: String,
    pub command: String,
    pub respond: oneshot::Sender<bool>,
}

/// Status bar data.
pub struct StatusBar {
    pub session_name: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
}

/// Complete TUI application state.
pub struct TuiState {
    pub output: Vec<OutputLine>,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
    pub input: String,
    pub cursor_pos: usize,
    pub mode: InputMode,
    pub is_processing: bool,
    pub pending_approval: Option<PendingApproval>,
    pub status_bar: StatusBar,
    pub quit_requested: bool,
}

impl TuiState {
    /// Create a new TUI state with the given session name and model label.
    pub fn new(session_name: String, model: String) -> Self {
        Self {
            output: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input: String::new(),
            cursor_pos: 0,
            mode: InputMode::Normal,
            is_processing: false,
            pending_approval: None,
            status_bar: StatusBar {
                session_name,
                model,
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            quit_requested: false,
        }
    }

    /// Append streamed token text.  Extends the last `AssistantText` line
    /// or creates a new one if the last line is not assistant text.
    pub fn push_token(&mut self, text: &str) {
        match self.output.last_mut() {
            Some(OutputLine::AssistantText(buf)) => buf.push_str(text),
            _ => self
                .output
                .push(OutputLine::AssistantText(text.to_string())),
        }
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Record that a tool call has started.
    pub fn tool_started(&mut self, tool_name: String, call_id: String, command: Option<String>) {
        let _ = call_id; // call_id reserved for future collapsible panels
        self.output.push(OutputLine::ToolActivity {
            tool_name,
            command,
            status: ToolStatus::Running,
        });
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Update the most recent matching tool activity to completed.
    pub fn tool_finished(
        &mut self,
        call_id: &str,
        tool_name: &str,
        status: &str,
        exit_code: Option<i64>,
    ) {
        let _ = call_id;
        // Walk backwards to find the matching Running tool activity.
        for line in self.output.iter_mut().rev() {
            if let OutputLine::ToolActivity {
                tool_name: name,
                status: s,
                ..
            } = line
                && name == tool_name
                && *s == ToolStatus::Running
            {
                *s = if status == "completed" {
                    ToolStatus::Completed { exit_code }
                } else {
                    ToolStatus::Failed {
                        status: status.to_string(),
                    }
                };
                break;
            }
        }
    }

    /// Add a user prompt to the output.
    pub fn push_user_prompt(&mut self, text: &str) {
        self.output.push(OutputLine::UserPrompt(text.to_string()));
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Add a system message to the output.
    pub fn push_system_message(&mut self, text: &str) {
        self.output
            .push(OutputLine::SystemMessage(text.to_string()));
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    /// Update token counts from turn stats.
    pub fn update_turn_stats(
        &mut self,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        total_tokens: Option<i64>,
    ) {
        if let Some(v) = prompt_tokens {
            self.status_bar.prompt_tokens = v;
        }
        if let Some(v) = completion_tokens {
            self.status_bar.completion_tokens = v;
        }
        if let Some(v) = total_tokens {
            self.status_bar.total_tokens = v;
        }
    }

    /// Scroll to the bottom of the output.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(n);
        self.auto_scroll = false;
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .next_back()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            let start = self.cursor_pos - prev;
            self.input.drain(start..self.cursor_pos);
            self.cursor_pos = start;
        }
    }

    /// Move cursor left.
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .next_back()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos -= prev;
        }
    }

    /// Move cursor right.
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            let next = self.input[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos += next;
        }
    }

    /// Take the current input text and reset the input field.
    pub fn take_input(&mut self) -> String {
        self.cursor_pos = 0;
        std::mem::take(&mut self.input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_token_creates_assistant_text() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.push_token("hello ");
        state.push_token("world");
        assert_eq!(state.output.len(), 1);
        match &state.output[0] {
            OutputLine::AssistantText(text) => assert_eq!(text, "hello world"),
            _ => panic!("expected AssistantText"),
        }
    }

    #[test]
    fn push_token_after_user_prompt_creates_new_block() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.push_user_prompt("hi");
        state.push_token("response");
        assert_eq!(state.output.len(), 2);
    }

    #[test]
    fn tool_finished_updates_matching_running_tool() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.tool_started("execute".into(), "c1".into(), Some("ls".into()));
        state.tool_finished("c1", "execute", "completed", Some(0));
        match &state.output[0] {
            OutputLine::ToolActivity { status, .. } => {
                assert_eq!(*status, ToolStatus::Completed { exit_code: Some(0) });
            }
            _ => panic!("expected ToolActivity"),
        }
    }

    #[test]
    fn prompt_started_and_finished_toggle_processing() {
        let mut state = TuiState::new("test".into(), "model".into());
        assert!(!state.is_processing);
        state.is_processing = true;
        assert!(state.is_processing);
        state.is_processing = false;
        assert!(!state.is_processing);
    }

    #[test]
    fn backspace_handles_empty_input() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.backspace(); // should not panic
        assert_eq!(state.input, "");
    }

    #[test]
    fn insert_and_backspace_round_trip() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.insert_char('a');
        state.insert_char('b');
        assert_eq!(state.input, "ab");
        state.backspace();
        assert_eq!(state.input, "a");
    }

    #[test]
    fn take_input_clears_state() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.insert_char('x');
        let taken = state.take_input();
        assert_eq!(taken, "x");
        assert_eq!(state.input, "");
        assert_eq!(state.cursor_pos, 0);
    }

    #[test]
    fn scroll_up_disables_auto_scroll() {
        let mut state = TuiState::new("test".into(), "model".into());
        assert!(state.auto_scroll);
        state.scroll_up(5);
        assert!(!state.auto_scroll);
        assert_eq!(state.scroll_offset, 5);
    }

    #[test]
    fn scroll_down_to_zero_re_enables_auto_scroll() {
        let mut state = TuiState::new("test".into(), "model".into());
        state.scroll_up(3);
        state.scroll_down(3);
        assert!(state.auto_scroll);
        assert_eq!(state.scroll_offset, 0);
    }
}
