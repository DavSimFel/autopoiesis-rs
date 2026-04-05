//! crossterm key event handling and mode dispatch.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::event::TuiCommand;
use super::state::{InputMode, TuiState};

/// Result of processing a single crossterm event.
pub enum InputResult {
    /// No action needed.
    None,
    /// Send this command to the worker.
    Command(TuiCommand),
    /// Force-quit (second Ctrl+C).
    ForceQuit,
}

/// Process a crossterm event and update TUI state.
///
/// Returns an [`InputResult`] indicating what action the caller should take.
pub fn handle_event(event: Event, state: &mut TuiState) -> InputResult {
    let Event::Key(key) = event else {
        return InputResult::None;
    };

    match state.mode {
        InputMode::Normal => handle_normal_mode(key, state),
        InputMode::Approval => handle_approval_mode(key, state),
    }
}

fn handle_normal_mode(key: KeyEvent, state: &mut TuiState) -> InputResult {
    // Ctrl+C / Ctrl+D — quit
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d') => {
                if state.quit_requested {
                    return InputResult::ForceQuit;
                }
                state.quit_requested = true;
                return InputResult::Command(TuiCommand::Quit);
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter => {
            if state.is_processing {
                return InputResult::None;
            }
            let text = state.take_input();
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                return InputResult::None;
            }
            if trimmed == "exit" || trimmed == "quit" {
                state.quit_requested = true;
                return InputResult::Command(TuiCommand::Quit);
            }
            state.push_user_prompt(&trimmed);
            InputResult::Command(TuiCommand::UserPrompt(trimmed))
        }
        KeyCode::Char(c) => {
            if !state.is_processing {
                state.insert_char(c);
            }
            InputResult::None
        }
        KeyCode::Backspace => {
            if !state.is_processing {
                state.backspace();
            }
            InputResult::None
        }
        KeyCode::Left => {
            state.cursor_left();
            InputResult::None
        }
        KeyCode::Right => {
            state.cursor_right();
            InputResult::None
        }
        KeyCode::PageUp => {
            state.scroll_up(10);
            InputResult::None
        }
        KeyCode::PageDown => {
            state.scroll_down(10);
            InputResult::None
        }
        KeyCode::Up => {
            state.scroll_up(1);
            InputResult::None
        }
        KeyCode::Down => {
            state.scroll_down(1);
            InputResult::None
        }
        _ => InputResult::None,
    }
}

fn handle_approval_mode(key: KeyEvent, state: &mut TuiState) -> InputResult {
    // Ctrl+C in approval mode also quits.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('d'))
    {
        // Deny the pending approval and quit.
        if let Some(approval) = state.pending_approval.take() {
            let _ = approval.respond.send(false);
        }
        state.mode = InputMode::Normal;
        state.quit_requested = true;
        return InputResult::Command(TuiCommand::Quit);
    }

    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(approval) = state.pending_approval.take() {
                let _ = approval.respond.send(true);
            }
            state.mode = InputMode::Normal;
            InputResult::None
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            if let Some(approval) = state.pending_approval.take() {
                let _ = approval.respond.send(false);
            }
            state.mode = InputMode::Normal;
            InputResult::None
        }
        _ => InputResult::None,
    }
}
