use std::io::{self, BufRead, Write};

use crate::agent::{ApprovalHandler, TokenSink};
use crate::gate::Severity;

const LOW_APPROVAL_PREFIX: &str = "⚠️";
const MEDIUM_APPROVAL_PREFIX: &str = "🟡";
const HIGH_APPROVAL_PREFIX: &str = "🔴";
const APPROVAL_PROMPT_TEXT: &str = "Approve? [y/n]:";
const APPROVAL_YES_TOKEN: &str = "y";

fn severity_prefix(severity: &Severity) -> &'static str {
    match severity {
        Severity::Low => LOW_APPROVAL_PREFIX,
        Severity::Medium => MEDIUM_APPROVAL_PREFIX,
        Severity::High => HIGH_APPROVAL_PREFIX,
    }
}

/// CLI token sink implementation.
pub struct CliTokenSink;

impl CliTokenSink {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CliTokenSink {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenSink for CliTokenSink {
    fn on_token(&mut self, token: String) {
        print!("{token}");
        if let Err(err) = io::stdout().flush() {
            eprintln!("failed to flush stdout: {err}");
        }
    }

    fn on_complete(&mut self) {
        println!();
    }
}

/// CLI approval handler implementation.
pub struct CliApprovalHandler;

impl CliApprovalHandler {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CliApprovalHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalHandler for CliApprovalHandler {
    fn request_approval(&mut self, severity: &Severity, reason: &str, command: &str) -> bool {
        let prefix = severity_prefix(severity);

        eprintln!("\n{prefix} {reason}");
        eprintln!("  Command: {command}");
        eprint!("  {APPROVAL_PROMPT_TEXT} ");
        if io::stderr().flush().is_err() {
            return false;
        }

        let mut input = String::new();
        let mut stdin = io::stdin().lock();
        match stdin.read_line(&mut input) {
            Ok(_) => input.trim().eq_ignore_ascii_case(APPROVAL_YES_TOKEN),
            Err(error) => {
                eprintln!("failed to read approval input: {error}");
                false
            }
        }
    }
}

/// Format a denial message for CLI and server output.
pub fn format_denial_message(reason: &str, gate_id: &str) -> String {
    format!("Command hard-denied by {gate_id}: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::Severity;

    #[test]
    fn format_denial_message_uses_gate_and_reason() {
        assert_eq!(
            format_denial_message("blocked by policy", "shell-policy"),
            "Command hard-denied by shell-policy: blocked by policy"
        );
    }

    #[test]
    fn severity_prefix_maps_all_levels() {
        let cases = [
            (Severity::Low, "⚠️"),
            (Severity::Medium, "🟡"),
            (Severity::High, "🔴"),
        ];

        for (severity, expected) in cases {
            assert_eq!(severity_prefix(&severity), expected);
        }
    }
}
