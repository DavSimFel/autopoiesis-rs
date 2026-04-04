use serde_json::{Value, from_str};
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::debug;

use super::command_path_analysis::{
    command_writes_identity_template_path, command_writes_target_path,
    simple_command_reads_protected_path, simple_command_reads_target_path,
};
use crate::config::ShellPolicy;
use crate::config::{ShellDefaultAction, ShellDefaultSeverity};
use crate::gate::{Guard, GuardContext, GuardEvent, Severity, Verdict};
use crate::llm::ToolCall;

const SHELL_POLICY_GUARD_ID: &str = "shell-policy";
const SHELL_POLICY_COMMAND_FIELD: &str = "command";
const ALLOWLIST_MISS_REASON: &str = "shell command did not match any allowlist pattern";
const COMPOUND_COMMAND_REASON: &str = "compound shell command requires explicit approval";
const STANDING_APPROVAL_LOG_PREFIX: &str = "[standing-approval] command matched pattern: ";

/// Policy-driven shell validator used for tool call argument inspection.
pub struct ShellSafety {
    id: String,
    policy: ShellPolicy,
    skills_dirs: Vec<PathBuf>,
    default_action: ShellDefaultAction,
    default_severity: Severity,
    standing_approvals: Vec<String>,
    standing_approval_matches: Mutex<Vec<StandingApprovalMatch>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StandingApprovalMatch {
    command: String,
    pattern: String,
}

impl ShellSafety {
    pub fn new() -> Self {
        Self::with_policy_and_skills_dirs(ShellPolicy::default(), vec![PathBuf::from("skills")])
    }

    pub fn with_policy(policy: ShellPolicy) -> Self {
        Self::with_policy_and_skills_dirs(policy, vec![PathBuf::from("skills")])
    }

    pub fn with_policy_and_skills_dir(policy: ShellPolicy, skills_dir: PathBuf) -> Self {
        Self::with_policy_and_skills_dirs(policy, vec![skills_dir])
    }

    pub fn with_policy_and_skills_dirs(policy: ShellPolicy, skills_dirs: Vec<PathBuf>) -> Self {
        let default_action = policy.default;
        let default_severity = match policy.default_severity {
            ShellDefaultSeverity::Low => Severity::Low,
            ShellDefaultSeverity::Medium => Severity::Medium,
            ShellDefaultSeverity::High => Severity::High,
        };
        let standing_approvals = policy.standing_approvals.clone();

        Self {
            id: SHELL_POLICY_GUARD_ID.to_string(),
            policy,
            skills_dirs,
            default_action,
            default_severity,
            standing_approvals,
            standing_approval_matches: Mutex::new(Vec::new()),
        }
    }

    // Policy: malformed tool payloads are security failures, not empty commands.
    fn command_from_args(&self, call: &ToolCall) -> std::result::Result<String, Verdict> {
        let value = from_str::<Value>(&call.arguments).map_err(|_| Verdict::Deny {
            reason: "shell tool arguments were malformed JSON".to_string(),
            gate_id: self.id.clone(),
        })?;
        value
            .get(SHELL_POLICY_COMMAND_FIELD)
            .and_then(Value::as_str)
            .map(|command| command.trim().to_string())
            .filter(|command| !command.is_empty())
            .ok_or_else(|| Verdict::Deny {
                reason: "shell tool arguments must include a string command field".to_string(),
                gate_id: self.id.clone(),
            })
    }

    fn glob_matches(pattern: &str, text: &str) -> bool {
        let pattern: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = text.chars().collect();
        let mut pattern_index = 0usize;
        let mut text_index = 0usize;
        let mut star_index: Option<usize> = None;
        let mut match_index = 0usize;

        while text_index < text.len() {
            if pattern_index < pattern.len() && pattern[pattern_index] == text[text_index] {
                pattern_index += 1;
                text_index += 1;
            } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
                star_index = Some(pattern_index);
                pattern_index += 1;
                match_index = text_index;
            } else if let Some(star_position) = star_index {
                pattern_index = star_position + 1;
                match_index += 1;
                text_index = match_index;
            } else {
                return false;
            }
        }

        while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            pattern_index += 1;
        }

        pattern_index == pattern.len()
    }

    fn matches_any_pattern<'a>(patterns: &'a [String], command: &str) -> Option<&'a str> {
        patterns.iter().find_map(|pattern| {
            if Self::glob_matches(pattern, command) {
                Some(pattern.as_str())
            } else {
                None
            }
        })
    }

    fn shell_words(command: &str) -> Result<Vec<String>, shell_words::ParseError> {
        shell_words::split(command)
    }

    fn is_compound_command(command: &str) -> bool {
        let markers = [";", "&&", "||", "|", "`", "$(", "\n", "\r"];
        markers.iter().any(|marker| command.contains(marker))
    }

    fn approval_required_verdict(&self, _command: &str, reason: &str) -> Verdict {
        Verdict::Approve {
            reason: reason.to_string(),
            gate_id: self.id.clone(),
            severity: self.default_severity,
        }
    }

    fn default_verdict(&self, command: &str) -> Verdict {
        match self.default_action {
            ShellDefaultAction::Allow => Verdict::Allow,
            ShellDefaultAction::Approve => Verdict::Approve {
                reason: if command.is_empty() {
                    ALLOWLIST_MISS_REASON.to_string()
                } else {
                    format!("shell command `{command}` did not match any allowlist pattern")
                },
                gate_id: self.id.clone(),
                severity: self.default_severity,
            },
        }
    }

    fn evaluate_command(&self, command: &str, tainted: bool) -> Verdict {
        // Policy: deny patterns always win first.
        if let Some(pattern) = Self::matches_any_pattern(&self.policy.deny_patterns, command) {
            return Verdict::Deny {
                reason: format!("shell command matched deny pattern `{pattern}`"),
                gate_id: self.id.clone(),
            };
        }

        // Policy: path safety checks are hard denies, and malformed shell parsing is not a safe fallback.
        match Self::shell_words(command) {
            Ok(argv)
                if simple_command_reads_protected_path(&argv)
                    || self.skills_dirs.iter().any(|skills_dir| {
                        simple_command_reads_target_path(&argv, &skills_dir.to_string_lossy())
                    }) =>
            {
                return Verdict::Deny {
                    reason: format!("shell command reads protected credential path: `{command}`"),
                    gate_id: self.id.clone(),
                };
            }
            Err(_) => {
                return Verdict::Deny {
                    reason: format!("shell command could not be parsed safely: `{command}`"),
                    gate_id: self.id.clone(),
                };
            }
            _ => {}
        }

        // Policy: prompt and skills directories are write-protected shell boundaries.
        if command_writes_identity_template_path(command)
            || self.skills_dirs.iter().any(|skills_dir| {
                command_writes_target_path(command, &skills_dir.to_string_lossy())
            })
        {
            return Verdict::Deny {
                reason: format!("shell command writes protected prompt path: `{command}`"),
                gate_id: self.id.clone(),
            };
        }

        // Policy: compound commands always require explicit approval, even if a later allowlist would match.
        if Self::is_compound_command(command) {
            return self.approval_required_verdict(command, COMPOUND_COMMAND_REASON);
        }

        if Self::matches_any_pattern(&self.policy.allow_patterns, command).is_some() {
            return Verdict::Allow;
        }

        // Policy: standing approvals are disabled once the turn is tainted.
        if !tainted
            && let Some(pattern) = Self::matches_any_pattern(&self.standing_approvals, command)
        {
            let mut matches = self
                .standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            matches.push(StandingApprovalMatch {
                command: command.to_string(),
                pattern: pattern.to_string(),
            });
            debug!(
                pattern = %pattern,
                tainted,
                "{STANDING_APPROVAL_LOG_PREFIX}{pattern}"
            );
            return Verdict::Allow;
        }

        self.default_verdict(command)
    }
}

impl Default for ShellSafety {
    fn default() -> Self {
        Self::new()
    }
}

impl Guard for ShellSafety {
    fn name(&self) -> &str {
        &self.id
    }

    fn check(&self, event: &mut GuardEvent, context: &GuardContext) -> Verdict {
        match event {
            GuardEvent::ToolCall(call) => {
                let command = match self.command_from_args(call) {
                    Ok(command) => command,
                    Err(verdict) => return verdict,
                };
                self.evaluate_command(&command, context.tainted)
            }
            _ => Verdict::Allow,
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::gate::GuardEvent;
    use crate::llm::ToolCall;
    use crate::principal::Principal;
    use serde_json::json;

    fn make_tool_call(cmd: &str) -> ToolCall {
        ToolCall {
            id: "tool_call_1".to_string(),
            name: "execute".to_string(),
            arguments: json!({ "command": cmd }).to_string(),
        }
    }

    fn make_event_tool<'a>(call: &'a ToolCall) -> GuardEvent<'a> {
        GuardEvent::ToolCall(call)
    }

    fn shell_policy(
        default: &str,
        allow_patterns: &[&str],
        deny_patterns: &[&str],
        standing_approvals: &[&str],
        default_severity: &str,
    ) -> ShellPolicy {
        ShellPolicy {
            default: match default {
                "allow" => crate::config::ShellDefaultAction::Allow,
                "approve" => crate::config::ShellDefaultAction::Approve,
                other => panic!("unexpected shell default in test helper: {other}"),
            },
            allow_patterns: allow_patterns
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            deny_patterns: deny_patterns
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            standing_approvals: standing_approvals
                .iter()
                .map(|pattern| pattern.to_string())
                .collect(),
            default_severity: match default_severity {
                "low" => crate::config::ShellDefaultSeverity::Low,
                "medium" => crate::config::ShellDefaultSeverity::Medium,
                "high" => crate::config::ShellDefaultSeverity::High,
                other => panic!("unexpected shell severity in test helper: {other}"),
            },
            max_output_bytes: crate::config::DEFAULT_SHELL_MAX_OUTPUT_BYTES,
            max_timeout_ms: crate::config::DEFAULT_SHELL_MAX_TIMEOUT_MS,
        }
    }

    fn tainted_messages() -> Vec<crate::llm::ChatMessage> {
        vec![crate::llm::ChatMessage::user_with_principal(
            "tainted input",
            Some(Principal::User),
        )]
    }

    #[test]
    fn whitespace_only_command_is_hard_denied() {
        let gate = ShellSafety::new();
        let call = ToolCall {
            id: "tool_call_1".to_string(),
            name: "execute".to_string(),
            arguments: json!({"command": "   "}).to_string(),
        };
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny {
                gate_id,
                ..
            } if gate_id == SHELL_POLICY_GUARD_ID
        ));
    }

    #[test]
    fn invalid_command_json_is_hard_denied() {
        let gate = ShellSafety::new();
        let call = ToolCall {
            id: "tool_call_1".to_string(),
            name: "execute".to_string(),
            arguments: "not-json".to_string(),
        };
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny {
                gate_id,
                ..
            } if gate_id == SHELL_POLICY_GUARD_ID
        ));
    }

    #[test]
    fn missing_command_field_is_hard_denied() {
        let gate = ShellSafety::new();
        let call = ToolCall {
            id: "tool_call_1".to_string(),
            name: "execute".to_string(),
            arguments: json!({"timeout_ms": 1}).to_string(),
        };
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny {
                gate_id,
                ..
            } if gate_id == SHELL_POLICY_GUARD_ID
        ));
    }

    #[test]
    fn default_config_approves_unmatched_command() {
        let gate = ShellSafety::new();
        let call = make_tool_call("python -c 'print(1)'");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Approve {
                severity: Severity::Medium,
                ..
            }
        ));
    }

    #[test]
    fn custom_skills_directory_is_protected() {
        let gate = ShellSafety::with_policy_and_skills_dir(
            ShellPolicy::default(),
            PathBuf::from("custom-skills"),
        );
        let call = make_tool_call("cp /tmp/source custom-skills/code-review.toml");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn custom_skills_directory_reads_are_protected() {
        let gate = ShellSafety::with_policy_and_skills_dir(
            ShellPolicy::default(),
            PathBuf::from("custom-skills"),
        );
        let call = make_tool_call("cat custom-skills/code-review.toml");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn deny_pattern_takes_precedence_over_allow_and_standing_pattern() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &["git *"],
            &["git push *"],
            &["git push *"],
            "low",
        ));
        let call = make_tool_call("git push origin main");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn deny_pattern_blocks_matching_command() {
        let gate =
            ShellSafety::with_policy(shell_policy("approve", &[], &["rm -rf /*"], &[], "medium"));
        let call = make_tool_call("rm -rf /");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn allow_pattern_allows_matching_command() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &["git *"],
            &[],
            &["git push *"],
            "high",
        ));
        let call = make_tool_call("git status");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn unmatched_command_approves_when_default_is_approve() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &[], &[], &[], "high"));
        let call = make_tool_call("python -c 'print(1)'");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Approve {
                severity: Severity::High,
                ..
            }
        ));
    }

    #[test]
    fn unmatched_command_allows_when_default_is_allow() {
        let gate = ShellSafety::with_policy(shell_policy("allow", &[], &[], &[], "high"));
        let call = make_tool_call("python -c 'print(1)'");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
    }

    #[test]
    fn standing_approval_allows_matching_command_and_records_audit() {
        let gate =
            ShellSafety::with_policy(shell_policy("approve", &[], &[], &["git push *"], "high"));
        let call = make_tool_call("git push origin main");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));

        let matches = gate
            .standing_approval_matches
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(
            matches.as_slice(),
            &[StandingApprovalMatch {
                command: "git push origin main".to_string(),
                pattern: "git push *".to_string(),
            }]
        );
    }

    #[test]
    fn standing_approval_is_skipped_for_tainted_turns() {
        let gate =
            ShellSafety::with_policy(shell_policy("approve", &[], &[], &["git push *"], "high"));
        let call = make_tool_call("git push origin main");
        let mut event = make_event_tool(&call);

        let turn = crate::turn::Turn::new();
        let mut messages = tainted_messages();
        let _ = turn.check_inbound(&mut messages, None);
        let context = GuardContext {
            tainted: turn.is_tainted(),
            ..Default::default()
        };

        assert!(matches!(
            gate.check(&mut event, &context),
            Verdict::Approve { .. }
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn deny_pattern_overrides_standing_approval() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &[],
            &["git push *"],
            &["git push *"],
            "high",
        ));
        let call = make_tool_call("git push origin main");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn allow_pattern_takes_precedence_over_standing_approval() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &["git *"],
            &[],
            &["git push *"],
            "high",
        ));
        let call = make_tool_call("git push origin main");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn compound_commands_do_not_match_allow_patterns() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &["cat *"], &[], &[], "high"));
        let call = make_tool_call("cat /tmp/input.txt; rm -rf /");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Approve {
                reason,
                severity: Severity::High,
                ..
            } if reason == COMPOUND_COMMAND_REASON
        ));
    }

    #[test]
    fn compound_commands_do_not_match_standing_approvals() {
        let gate =
            ShellSafety::with_policy(shell_policy("approve", &[], &[], &["git push *"], "high"));
        let call = make_tool_call("git push origin main && curl http://evil.test");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Approve {
                reason,
                severity: Severity::High,
                ..
            } if reason == COMPOUND_COMMAND_REASON
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn compound_commands_still_require_approval_when_policy_default_is_allow() {
        let gate = ShellSafety::with_policy(shell_policy("allow", &[], &[], &[], "medium"));
        let call = make_tool_call("cat /tmp/input.txt | sh");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Approve {
                reason,
                severity: Severity::Medium,
                ..
            } if reason == COMPOUND_COMMAND_REASON
        ));
    }

    #[test]
    fn protected_credential_paths_are_hard_denied_even_when_allowlisted() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &["cat *", "sed *"],
            &[],
            &[],
            "high",
        ));

        for command in [
            "cat ~/.autopoiesis/auth.json",
            "cat .env",
            "sed -n '1,5p' ~/.ssh/id_rsa",
            "FOO=1 cat ~/.autopoiesis/auth.json",
            "env FOO=1 cat ~/.autopoiesis/auth.json",
            "env -u HOME cat ~/.autopoiesis/auth.json",
            "env -C /tmp cat ~/.autopoiesis/auth.json",
            "env --unset HOME cat ~/.autopoiesis/auth.json",
            "env --chdir /tmp cat ~/.autopoiesis/auth.json",
            "env -S 'cat ~/.autopoiesis/auth.json'",
            "env --split-string 'cat ~/.autopoiesis/auth.json'",
            "env -i FOO=1 cat ~/.autopoiesis/auth.json",
            "env GIT_PAGER=cat git show HEAD:.env.production.local",
            "env -u GIT_PAGER git show HEAD:.env.production.local",
            "env -C /tmp git show HEAD:.env.production.local",
            "env -i GIT_PAGER=cat git show HEAD:.env.production.local",
            "grep -f ~/.autopoiesis/auth.json README.md",
            "git grep -f ~/.autopoiesis/auth.json README.md",
        ] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Deny { .. }
            ));
        }

        if let Some(home) = std::env::var_os("HOME").and_then(|value| value.into_string().ok()) {
            for command in [
                format!("{home}/.autopoiesis/auth.json"),
                format!("{home}/.ssh/id_rsa"),
            ] {
                let call = make_tool_call(&format!("cat {command}"));
                let mut event = make_event_tool(&call);
                assert!(matches!(
                    gate.check(&mut event, &GuardContext::default()),
                    Verdict::Deny { .. }
                ));
            }
        }

        for command in ["cat config/auth.json", "cat .env.example"] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Allow
            ));
        }

        for command in ["grep '.env' README.md", "git grep '.env' README.md"] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Approve { .. }
            ));
        }
    }

    #[test]
    fn git_content_dump_of_protected_path_is_hard_denied() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &["git *"], &[], &[], "high"));

        for command in [
            "git diff --no-index /dev/null ~/.autopoiesis/auth.json",
            "git grep . ~/.autopoiesis/auth.json",
            "git show ~/.autopoiesis/auth.json",
            "git cat-file -p HEAD:auth.json",
            "git show HEAD:.autopoiesis/auth.json",
            "git show HEAD:.ssh/id_rsa",
            "git show HEAD:.aws/credentials",
            "git --no-pager show HEAD:.env.production.local",
            "git -c color.ui=always grep . main:path/to/.env.local",
            "git -c alias.show='!cat ~/.autopoiesis/auth.json' show",
            "GIT_PAGER=cat git show HEAD:.env.production.local",
        ] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Deny { .. }
            ));
        }

        let call = make_tool_call("git show HEAD:.env.example");
        let mut event = make_event_tool(&call);
        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
    }

    #[test]
    fn literal_reference_to_protected_path_is_not_hard_denied() {
        let gate =
            ShellSafety::with_policy(shell_policy("approve", &["printf *"], &[], &[], "high"));
        let call = make_tool_call("printf '%s\\n' ~/.autopoiesis/auth.json");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
    }

    #[test]
    fn protected_paths_do_not_consume_standing_approvals() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &[],
            &[],
            &["cat ~/.autopoiesis/auth.json"],
            "high",
        ));
        let call = make_tool_call("cat ~/.autopoiesis/auth.json");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
        assert!(
            gate.standing_approval_matches
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn protected_paths_still_deny_when_policy_default_is_allow() {
        let gate = ShellSafety::with_policy(shell_policy("allow", &["cat *"], &[], &[], "medium"));
        let call = make_tool_call("cat ~/.autopoiesis/auth.json");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn simple_allowlisted_command_still_allows_after_hardening() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &["ls *"], &[], &[], "high"));
        let call = make_tool_call("ls /tmp");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow
        ));
    }

    #[test]
    fn identity_template_writes_are_hard_denied_even_when_allowlisted() {
        let gate = ShellSafety::with_policy(shell_policy(
            "approve",
            &["cat *", "tee *", "cp *", "mv *", "rm *"],
            &[],
            &[],
            "high",
        ));

        for command in [
            "printf hi > identity-templates/constitution.md",
            "printf hi >identity-templates/constitution.md",
            "bash -c \"cat > identity-templates/context.md\"",
            "bash -c \"touch identity-templates/context.md\"",
            "sh -c \"rm -rf identity-templates\"",
            "bash -c \"git restore -- identity-templates/context.md\"",
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').touch()\"",
            "tee identity-templates/context.md",
            "cp /tmp/x identity-templates/agents/silas/agent.md",
            "mv identity-templates/agents/silas/agent.md /tmp/x",
            "rm -rf identity-templates/agents/silas",
            "touch skills/code-review.toml",
        ] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Deny { .. }
            ));
        }
    }

    #[test]
    fn identity_template_common_write_mechanisms_are_hard_denied() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &[], &[], &[], "high"));

        for command in [
            "touch identity-templates/context.md",
            "chmod 600 identity-templates/constitution.md",
            "chown root identity-templates/agents/silas/agent.md",
            "ln -sf /tmp/source identity-templates/context.md",
            "cp /tmp/source identity-templates",
            "git checkout -- identity-templates/context.md",
            "git restore identity-templates/constitution.md",
            "python -c \"open('identity-templates/context.md', 'w').write('x')\"",
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').write_text('x')\"",
            "cp /tmp/source skills/code-review.toml",
        ] {
            let call = make_tool_call(command);
            let mut event = make_event_tool(&call);
            assert!(matches!(
                gate.check(&mut event, &GuardContext::default()),
                Verdict::Deny { .. }
            ));
        }
    }

    #[test]
    fn identity_template_reads_are_not_hard_denied() {
        let gate = ShellSafety::with_policy(shell_policy("approve", &["cat *"], &[], &[], "high"));
        let call = make_tool_call("cat identity-templates/context.md");
        let mut event = make_event_tool(&call);

        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow | Verdict::Approve { .. }
        ));

        let call =
            make_tool_call("python -c \"print(open('identity-templates/context.md').read())\"");
        let mut event = make_event_tool(&call);
        assert!(matches!(
            gate.check(&mut event, &GuardContext::default()),
            Verdict::Allow | Verdict::Approve { .. }
        ));
    }
}

#[cfg(test)]
mod identity_template_shell_safety_tests {
    use super::{command_writes_identity_template_path, command_writes_target_path};

    #[test]
    fn denies_wrapped_perl_inplace_edit() {
        assert!(command_writes_identity_template_path(
            r#"bash -c "perl -pi -e 's/foo/bar/' identity-templates/context.md""#,
        ));
        assert!(command_writes_target_path(
            r#"bash -c "perl -pi -e 's/foo/bar/' custom-skills/code-review.toml""#,
            "custom-skills"
        ));
    }

    #[test]
    fn allows_read_only_copy_outside_the_tree() {
        assert!(!command_writes_identity_template_path(
            r#"bash -c "cp identity-templates/context.md /tmp/x""#,
        ));
    }
}
