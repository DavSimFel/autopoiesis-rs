#![cfg(not(clippy))]
#![allow(clippy::all)]

use anyhow::{Result, anyhow};
use autopoiesis::plan::{PlanAction, PlanActionKind, extract_plan_action, validate_plan_action};

fn valid_plan_json() -> &'static str {
    r#"{"kind":"plan","plan_run_id":"run-1","replace_from_step":1,"note":"build a plan","steps":[{"kind":"spawn","id":"spawn-1","spawn":{"task":"spin up worker","task_kind":"analysis","tier":"t2","model_override":null,"reasoning_override":null,"skills":["skill-a"],"skill_token_budget":128},"checks":[{"id":"check-1","command":"echo ok","expect":{"exit_code":0,"stdout_contains":"ok","stderr_contains":null,"stdout_equals":null}}],"max_attempts":2},{"kind":"shell","id":"shell-1","command":"echo run","timeout_ms":5000,"checks":[{"id":"check-2","command":"echo ok","expect":{"exit_code":0,"stdout_contains":"ok","stderr_contains":null,"stdout_equals":null}}],"max_attempts":1}]}"#
}

#[test]
fn extracts_plan_json_block_from_assistant_text() -> Result<()> {
    let assistant_text = format!(
        "assistant prose\n```plan-json\n{}\n```\nclosing prose",
        valid_plan_json()
    );

    let action = extract_plan_action(&assistant_text)?
        .ok_or_else(|| anyhow!("expected a plan-json block"))?;
    assert_eq!(action.kind, PlanActionKind::Plan);
    assert_eq!(action.plan_run_id.as_deref(), Some("run-1"));
    assert_eq!(action.replace_from_step, Some(1));
    assert_eq!(action.note.as_deref(), Some("build a plan"));
    assert_eq!(action.steps.len(), 2);
    Ok(())
}

#[test]
fn extracts_first_plan_json_block_when_multiple_exist() -> Result<()> {
    let assistant_text = format!(
        "intro\n```plan-json\n{}\n```\nmiddle\n```plan-json\n{}\n```",
        valid_plan_json(),
        valid_plan_json().replace("run-1", "run-2")
    );

    let action = extract_plan_action(&assistant_text)?
        .ok_or_else(|| anyhow!("expected a plan-json block"))?;
    assert_eq!(action.plan_run_id.as_deref(), Some("run-1"));
    Ok(())
}

#[test]
fn validates_plan_action_with_serde() -> Result<()> {
    let action: PlanAction = serde_json::from_str(valid_plan_json())?;

    validate_plan_action(&action)?;
    Ok(())
}

#[test]
fn rejects_malformed_plan_blocks() {
    let malformed_json = "```plan-json\n{not json}\n```";
    assert!(extract_plan_action(malformed_json).is_err());

    let malformed_first_with_later_valid = format!(
        "```plan-json\n{{not json}}\n```\n```plan-json\n{}\n```",
        valid_plan_json()
    );
    assert!(extract_plan_action(&malformed_first_with_later_valid).is_err());

    let unterminated = "```plan-json\n{\"kind\":\"done\",\"steps\":[]}";
    assert!(extract_plan_action(unterminated).is_err());
}

#[test]
fn rejects_semantically_invalid_plan_blocks() {
    let invalid = r#"```plan-json
{"kind":"plan","plan_run_id":null,"replace_from_step":null,"note":null,"steps":[]}
```"#;

    assert!(extract_plan_action(invalid).is_err());
}

#[test]
fn parses_done_action_with_empty_steps() -> Result<()> {
    let action = PlanAction {
        kind: PlanActionKind::Done,
        plan_run_id: Some("run-2".to_string()),
        replace_from_step: None,
        note: Some("done".to_string()),
        steps: vec![],
    };

    validate_plan_action(&action)?;

    let text = r#"```plan-json
{"kind":"done","plan_run_id":"run-2","replace_from_step":null,"note":"done","steps":[]}
```"#;
    let parsed = extract_plan_action(text)?.ok_or_else(|| anyhow!("expected plan-json block"))?;
    assert_eq!(parsed.kind, PlanActionKind::Done);
    assert!(parsed.steps.is_empty());
    Ok(())
}

#[test]
fn parses_escalate_action_with_empty_steps() -> Result<()> {
    let action = PlanAction {
        kind: PlanActionKind::Escalate,
        plan_run_id: Some("run-3".to_string()),
        replace_from_step: Some(2),
        note: Some("escalate".to_string()),
        steps: vec![],
    };

    validate_plan_action(&action)?;

    let text = r#"```plan-json
{"kind":"escalate","plan_run_id":"run-3","replace_from_step":2,"note":"escalate","steps":[]}
```"#;
    let parsed = extract_plan_action(text)?.ok_or_else(|| anyhow!("expected plan-json block"))?;
    assert_eq!(parsed.kind, PlanActionKind::Escalate);
    assert!(parsed.steps.is_empty());
    Ok(())
}
