use std::io::{self, BufRead, Write};

use crate::agent::{ApprovalHandler, TokenSink};
use crate::gate::Severity;
use crate::util::{STDERR_USER_OUTPUT_TARGET, STDOUT_USER_OUTPUT_TARGET};
use tracing::{info, warn};

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

pub(crate) fn write_token_output(output: &mut dyn Write, token: &str) -> io::Result<()> {
    write!(output, "{token}")?;
    output.flush()
}

#[cfg(test)]
pub(crate) fn write_completion_newline(output: &mut dyn Write) -> io::Result<()> {
    writeln!(output)
}

#[cfg(test)]
pub(crate) fn render_approval_banner(
    output: &mut dyn Write,
    severity: &Severity,
    reason: &str,
    command: &str,
) -> io::Result<()> {
    let prefix = severity_prefix(severity);
    writeln!(output, "\n{prefix} {reason}")?;
    writeln!(output, "  Command: {command}")?;
    write!(output, "  {APPROVAL_PROMPT_TEXT} ")?;
    output.flush()
}

pub(crate) fn read_approval_response(input: &mut dyn BufRead) -> io::Result<bool> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case(APPROVAL_YES_TOKEN))
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
        if let Err(err) = write_token_output(&mut io::stdout(), &token) {
            warn!("failed to flush stdout: {err}");
        }
    }

    fn on_complete(&mut self) {
        info!(target: STDOUT_USER_OUTPUT_TARGET, "");
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
        info!(target: STDERR_USER_OUTPUT_TARGET, "");
        info!(target: STDERR_USER_OUTPUT_TARGET, "{prefix} {reason}");
        info!(target: STDERR_USER_OUTPUT_TARGET, "  Command: {command}");
        eprint!("  {APPROVAL_PROMPT_TEXT} ");
        if io::stderr().flush().is_err() {
            return false;
        }

        let mut stdin = io::stdin().lock();
        match read_approval_response(&mut stdin) {
            Ok(approved) => approved,
            Err(error) => {
                warn!("failed to read approval input: {error}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::format_denial_message;
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

    #[test]
    fn render_approval_banner_writes_prompt_structure() {
        let mut output = Vec::new();
        render_approval_banner(
            &mut output,
            &Severity::High,
            "needs review",
            "rm -rf /tmp/demo",
        )
        .expect("banner should render");

        let text = String::from_utf8(output).expect("utf8");
        assert!(text.contains("\n🔴 needs review\n"));
        assert!(text.contains("  Command: rm -rf /tmp/demo\n"));
        assert!(text.ends_with("  Approve? [y/n]: "));
    }

    #[test]
    fn token_output_helpers_preserve_stream_shape() {
        let mut output = Vec::new();
        write_token_output(&mut output, "abc").expect("token should render");
        write_completion_newline(&mut output).expect("newline should render");

        let text = String::from_utf8(output).expect("utf8");
        assert_eq!(text, "abc\n");
    }

    #[test]
    fn read_approval_response_accepts_yes_and_no() {
        let mut yes = io::Cursor::new(b"y\n".to_vec());
        let mut no = io::Cursor::new(b"n\n".to_vec());

        assert!(read_approval_response(&mut yes).expect("yes should parse"));
        assert!(!read_approval_response(&mut no).expect("no should parse"));
    }
}
