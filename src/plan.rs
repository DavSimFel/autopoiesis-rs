use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) mod executor;
pub(crate) mod notify;
pub(crate) mod patch;
pub mod recovery;
pub mod runner;

pub use recovery::recover_crashed_plans;
pub use runner::{
    CheckOutcome, CheckVerdict, ObservedOutput, PlanFailureDetails, StepOutcome, run_plan_step,
    tick_plan_runner,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAction {
    pub kind: PlanActionKind,
    pub plan_run_id: Option<String>,
    pub replace_from_step: Option<usize>,
    pub note: Option<String>,
    pub steps: Vec<PlanStepSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanActionKind {
    Plan,
    Done,
    Escalate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanStepSpec {
    Spawn {
        id: String,
        spawn: SpawnStepSpec,
        checks: Vec<ShellCheckSpec>,
        max_attempts: u32,
    },
    Shell {
        id: String,
        command: String,
        timeout_ms: Option<u64>,
        checks: Vec<ShellCheckSpec>,
        max_attempts: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnStepSpec {
    pub task: String,
    pub task_kind: Option<String>,
    pub tier: String,
    pub model_override: Option<String>,
    pub reasoning_override: Option<String>,
    pub skills: Vec<String>,
    pub skill_token_budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellCheckSpec {
    pub id: String,
    pub command: String,
    pub expect: ShellExpectation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellExpectation {
    pub exit_code: Option<i32>,
    pub stdout_contains: Option<String>,
    pub stderr_contains: Option<String>,
    pub stdout_equals: Option<String>,
}

/// Extracts the first `plan-json` fenced block and validates it as a `PlanAction`.
pub fn extract_plan_action(assistant_text: &str) -> Result<Option<PlanAction>> {
    let Some(block) = extract_plan_json_block(assistant_text)? else {
        return Ok(None);
    };

    let action: PlanAction =
        serde_json::from_str(&block).context("failed to parse plan-json block")?;
    validate_plan_action(&action).context("invalid plan-json block")?;
    Ok(Some(action))
}

/// Validates the semantic rules for plan actions.
pub fn validate_plan_action(action: &PlanAction) -> Result<()> {
    match action.kind {
        PlanActionKind::Plan => ensure!(
            !action.steps.is_empty(),
            "plan actions must contain at least one step"
        ),
        PlanActionKind::Done | PlanActionKind::Escalate => ensure!(
            action.steps.is_empty(),
            "done and escalate actions must not contain steps"
        ),
    }

    let mut ids = HashSet::new();
    for step in &action.steps {
        let step_id = match step {
            PlanStepSpec::Spawn {
                id,
                spawn,
                checks,
                max_attempts,
            } => {
                validate_step_id(id)?;
                validate_max_attempts(*max_attempts)?;
                validate_spawn_tier(&spawn.tier)?;
                validate_shell_checks(checks)?;
                id
            }
            PlanStepSpec::Shell {
                id,
                command,
                checks,
                max_attempts,
                ..
            } => {
                validate_step_id(id)?;
                validate_non_empty(command, "shell step command")?;
                validate_shell_checks(checks)?;
                validate_max_attempts(*max_attempts)?;
                id
            }
        };

        ensure!(ids.insert(step_id), "duplicate step id: {step_id}");
    }

    Ok(())
}

fn extract_plan_json_block(assistant_text: &str) -> Result<Option<String>> {
    let lines: Vec<&str> = assistant_text.lines().collect();
    let mut index = 0;

    while index < lines.len() {
        let line = lines[index].trim();
        if let Some(info) = line.strip_prefix("```")
            && info.trim() == "plan-json"
        {
            let mut block = String::new();
            index += 1;
            while index < lines.len() {
                let current = lines[index].trim();
                if current == "```" {
                    return Ok(Some(block));
                }
                if !block.is_empty() {
                    block.push('\n');
                }
                block.push_str(lines[index]);
                index += 1;
            }

            return Err(anyhow::anyhow!("unterminated plan-json fenced block"));
        }
        index += 1;
    }

    Ok(None)
}

fn validate_step_id(id: &str) -> Result<()> {
    validate_non_empty(id, "step id")
}

fn validate_max_attempts(max_attempts: u32) -> Result<()> {
    ensure!(max_attempts >= 1, "step max_attempts must be at least 1");
    Ok(())
}

fn validate_spawn_tier(tier: &str) -> Result<()> {
    ensure!(
        matches!(tier, "t1" | "t2" | "t3"),
        "spawn tier must be t1, t2, or t3"
    );
    Ok(())
}

fn validate_shell_checks(checks: &[ShellCheckSpec]) -> Result<()> {
    for check in checks {
        validate_non_empty(&check.command, "shell check command")?;
    }
    Ok(())
}

fn validate_non_empty(value: &str, label: &str) -> Result<()> {
    ensure!(!value.trim().is_empty(), "{label} must not be empty");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_step(id: &str) -> PlanStepSpec {
        PlanStepSpec::Spawn {
            id: id.to_string(),
            spawn: SpawnStepSpec {
                task: "do the thing".to_string(),
                task_kind: Some("analysis".to_string()),
                tier: "t2".to_string(),
                model_override: None,
                reasoning_override: None,
                skills: vec!["skill-a".to_string()],
                skill_token_budget: Some(100),
            },
            checks: vec![ShellCheckSpec {
                id: "check-1".to_string(),
                command: "echo ok".to_string(),
                expect: ShellExpectation {
                    exit_code: Some(0),
                    stdout_contains: Some("ok".to_string()),
                    stderr_contains: None,
                    stdout_equals: None,
                },
            }],
            max_attempts: 2,
        }
    }

    fn shell_step(id: &str) -> PlanStepSpec {
        PlanStepSpec::Shell {
            id: id.to_string(),
            command: "echo run".to_string(),
            timeout_ms: Some(5000),
            checks: vec![ShellCheckSpec {
                id: "check-1".to_string(),
                command: "echo ok".to_string(),
                expect: ShellExpectation {
                    exit_code: Some(0),
                    stdout_contains: Some("ok".to_string()),
                    stderr_contains: None,
                    stdout_equals: None,
                },
            }],
            max_attempts: 1,
        }
    }

    #[test]
    fn extract_plan_action_returns_none_when_no_plan_json_block_exists() {
        let text = "assistant prose\n```rust\nlet x = 1;\n```";

        assert_eq!(extract_plan_action(text).unwrap(), None);
    }

    #[test]
    fn extract_plan_action_parses_plan_json_block() {
        let text = r#"
assistant prose
```plan-json
{"kind":"plan","plan_run_id":null,"replace_from_step":null,"note":"hi","steps":[{"kind":"shell","id":"step-1","command":"echo run","timeout_ms":5000,"checks":[{"id":"check-1","command":"echo ok","expect":{"exit_code":0,"stdout_contains":"ok","stderr_contains":null,"stdout_equals":null}}],"max_attempts":1}]}
```
closing prose
"#;

        let action = extract_plan_action(text).unwrap().unwrap();
        assert_eq!(action.kind, PlanActionKind::Plan);
        assert_eq!(action.note.as_deref(), Some("hi"));
        assert_eq!(action.steps.len(), 1);
    }

    #[test]
    fn extract_plan_action_rejects_malformed_json_in_plan_block() {
        let text = "```plan-json\n{not json}\n```";

        assert!(extract_plan_action(text).is_err());
    }

    #[test]
    fn extract_plan_action_rejects_unterminated_plan_json_block() {
        let text = "```plan-json\n{\"kind\":\"done\",\"steps\":[]}";

        assert!(extract_plan_action(text).is_err());
    }

    #[test]
    fn validate_plan_action_accepts_valid_plan() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: Some("run-1".to_string()),
            replace_from_step: Some(1),
            note: Some("note".to_string()),
            steps: vec![spawn_step("step-1"), shell_step("step-2")],
        };

        validate_plan_action(&action).unwrap();
    }

    #[test]
    fn validate_plan_action_rejects_duplicate_step_ids() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![shell_step("step-1"), shell_step("step-1")],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_rejects_empty_step_id() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![shell_step("")],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_rejects_zero_max_attempts() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Shell {
                id: "step-1".to_string(),
                command: "echo run".to_string(),
                timeout_ms: None,
                checks: vec![],
                max_attempts: 0,
            }],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_rejects_empty_shell_step_command() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Shell {
                id: "step-1".to_string(),
                command: " ".to_string(),
                timeout_ms: None,
                checks: vec![],
                max_attempts: 1,
            }],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_rejects_empty_shell_check_command() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Shell {
                id: "step-1".to_string(),
                command: "echo run".to_string(),
                timeout_ms: None,
                checks: vec![ShellCheckSpec {
                    id: "check-1".to_string(),
                    command: " ".to_string(),
                    expect: ShellExpectation {
                        exit_code: None,
                        stdout_contains: None,
                        stderr_contains: None,
                        stdout_equals: None,
                    },
                }],
                max_attempts: 1,
            }],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_rejects_invalid_spawn_tier() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Spawn {
                id: "step-1".to_string(),
                spawn: SpawnStepSpec {
                    task: "task".to_string(),
                    task_kind: None,
                    tier: "t9".to_string(),
                    model_override: None,
                    reasoning_override: None,
                    skills: vec![],
                    skill_token_budget: None,
                },
                checks: vec![],
                max_attempts: 1,
            }],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_requires_steps_for_plan_kind() {
        let action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_requires_empty_steps_for_done_kind() {
        let action = PlanAction {
            kind: PlanActionKind::Done,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![shell_step("step-1")],
        };

        assert!(validate_plan_action(&action).is_err());
    }

    #[test]
    fn validate_plan_action_requires_empty_steps_for_escalate_kind() {
        let action = PlanAction {
            kind: PlanActionKind::Escalate,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![shell_step("step-1")],
        };

        assert!(validate_plan_action(&action).is_err());
    }
}
