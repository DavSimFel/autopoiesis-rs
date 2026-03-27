use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};

use crate::observe::{Observer, TraceEvent};
use crate::plan::notify::emit_failure_notified_to_t2;
use crate::plan::{PlanAction, PlanActionKind, PlanStepSpec, validate_plan_action};
use crate::store::{PlanRun, Store};
use crate::time::utc_timestamp;

static NEXT_PLAN_RUN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn emit_plan_run_created(
    observer: &dyn Observer,
    plan_run_id: &str,
    owner_session_id: &str,
    caused_by_turn_id: Option<&str>,
) {
    observer.emit(&TraceEvent::PlanRunCreated {
        session_id: owner_session_id.to_string(),
        plan_run_id: plan_run_id.to_string(),
        caused_by_turn_id: caused_by_turn_id.map(ToString::to_string),
        owner_session_id: owner_session_id.to_string(),
    });
}

fn emit_plan_run_patched(
    observer: &dyn Observer,
    plan_run_id: &str,
    owner_session_id: &str,
    caused_by_turn_id: Option<&str>,
) {
    observer.emit(&TraceEvent::PlanRunPatched {
        session_id: owner_session_id.to_string(),
        plan_run_id: plan_run_id.to_string(),
        caused_by_turn_id: caused_by_turn_id.map(ToString::to_string),
        owner_session_id: owner_session_id.to_string(),
    });
}

fn emit_plan_completed(observer: &dyn Observer, plan_run: &PlanRun, total_attempts: i64) {
    observer.emit(&TraceEvent::PlanCompleted {
        session_id: plan_run.owner_session_id.clone(),
        plan_run_id: plan_run.id.clone(),
        total_attempts,
    });
}

fn emit_plan_failed(
    observer: &dyn Observer,
    plan_run: &PlanRun,
    total_attempts: i64,
    reason: Option<String>,
) {
    observer.emit(&TraceEvent::PlanFailed {
        session_id: plan_run.owner_session_id.clone(),
        plan_run_id: plan_run.id.clone(),
        total_attempts,
        reason,
    });
}

#[cfg(test)]
pub(crate) fn apply_plan_patch(
    store: &mut Store,
    owner_session_id: &str,
    plan_run_id: &str,
    action: &PlanAction,
) -> Result<()> {
    apply_plan_patch_observed(
        Arc::new(crate::observe::NoopObserver),
        store,
        owner_session_id,
        None,
        plan_run_id,
        action,
    )
}

pub(crate) fn apply_plan_patch_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    owner_session_id: &str,
    caused_by_turn_id: Option<&str>,
    plan_run_id: &str,
    action: &PlanAction,
) -> Result<()> {
    ensure!(
        matches!(action.kind, PlanActionKind::Plan),
        "apply_plan_patch only accepts plan actions"
    );

    let plan_run = store
        .get_plan_run(plan_run_id)?
        .ok_or_else(|| anyhow::anyhow!("plan run not found: {plan_run_id}"))?;
    ensure!(
        plan_run.status == "waiting_t2",
        "plan run must be waiting_t2 before patching"
    );
    ensure!(
        plan_run.owner_session_id == owner_session_id,
        "plan run does not belong to the supplied owner session"
    );
    ensure!(
        action.plan_run_id.as_deref() == Some(plan_run_id),
        "plan patch plan_run_id must match the target run"
    );

    let replace_from_step = action
        .replace_from_step
        .context("plan patch actions must include replace_from_step")?;
    let current_action = load_plan_action(&plan_run)?;
    let current_step_index = plan_run.current_step_index as usize;
    ensure!(
        replace_from_step == current_step_index,
        "plan patch may only replace the remaining suffix"
    );
    ensure!(
        current_step_index <= current_action.steps.len(),
        "plan run current_step_index is out of range for stored plan"
    );

    let merged = merge_plan_actions(&plan_run, &current_action, action, current_step_index)?;
    let definition_json =
        serde_json::to_string(&merged).context("failed to serialize merged plan action")?;

    store
        .with_transaction(|tx| {
            let changed = tx
                .execute(
                    "UPDATE plan_runs
                 SET status = 'pending',
                     updated_at = ?2,
                     revision = ?3,
                     definition_json = ?4,
                     last_failure_json = NULL,
                     active_child_session_id = NULL,
                     claimed_at = NULL
                 WHERE id = ?1
                   AND owner_session_id = ?5
                   AND status = 'waiting_t2'
                   AND current_step_index = ?6",
                    rusqlite::params![
                        plan_run.id,
                        utc_timestamp(),
                        plan_run.revision + 1,
                        definition_json,
                        owner_session_id,
                        plan_run.current_step_index,
                    ],
                )
                .context("failed to store patched plan run")?;
            ensure!(
                changed == 1,
                "failed to store patched plan run: plan run not found or state changed"
            );
            Ok(())
        })
        .context("failed to store patched plan run atomically")?;

    emit_plan_run_patched(
        observer.as_ref(),
        &plan_run.id,
        owner_session_id,
        caused_by_turn_id,
    );

    Ok(())
}

#[cfg(test)]
pub(crate) fn apply_plan_action(
    store: &mut Store,
    owner_session_id: &str,
    action: &PlanAction,
) -> Result<()> {
    apply_plan_action_observed(
        Arc::new(crate::observe::NoopObserver),
        store,
        owner_session_id,
        None,
        action,
    )
}

pub(crate) fn apply_plan_action_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    owner_session_id: &str,
    caused_by_turn_id: Option<&str>,
    action: &PlanAction,
) -> Result<()> {
    validate_plan_action(action)?;

    match action.kind {
        PlanActionKind::Plan => {
            if action.plan_run_id.is_some() {
                let Some(plan_run_id) = action.plan_run_id.as_deref() else {
                    unreachable!("checked is_some above");
                };
                apply_plan_patch_observed(
                    observer,
                    store,
                    owner_session_id,
                    caused_by_turn_id,
                    plan_run_id,
                    action,
                )
            } else {
                create_plan_run_from_action_observed(
                    observer,
                    store,
                    owner_session_id,
                    caused_by_turn_id,
                    action,
                )
            }
        }
        PlanActionKind::Done => apply_plan_terminal_action_observed(
            observer,
            store,
            owner_session_id,
            caused_by_turn_id,
            action,
            "completed",
        ),
        PlanActionKind::Escalate => apply_plan_escalation_observed(
            observer,
            store,
            owner_session_id,
            caused_by_turn_id,
            action,
        ),
    }
}

fn create_plan_run_from_action_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    owner_session_id: &str,
    caused_by_turn_id: Option<&str>,
    action: &PlanAction,
) -> Result<()> {
    ensure!(
        action.replace_from_step.is_none(),
        "new plan actions must not include replace_from_step"
    );

    let plan_run_id = generate_plan_run_id();
    let normalized = normalize_plan_action(action, &plan_run_id)?;
    let definition_json =
        serde_json::to_string(&normalized).context("failed to serialize new plan action")?;

    store
        .create_plan_run(
            &plan_run_id,
            owner_session_id,
            &definition_json,
            None,
            Some("agent"),
        )
        .context("failed to create plan run from T2 output")?;

    emit_plan_run_created(
        observer.as_ref(),
        &plan_run_id,
        owner_session_id,
        caused_by_turn_id,
    );

    Ok(())
}

fn apply_plan_terminal_action_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    owner_session_id: &str,
    _caused_by_turn_id: Option<&str>,
    action: &PlanAction,
    status: &str,
) -> Result<()> {
    let plan_run_id = action
        .plan_run_id
        .as_deref()
        .context("terminal plan actions must include plan_run_id")?;
    let plan_run = store
        .get_plan_run(plan_run_id)?
        .ok_or_else(|| anyhow::anyhow!("plan run not found: {plan_run_id}"))?;
    ensure!(
        plan_run.status == "waiting_t2",
        "terminal plan actions are only allowed from waiting_t2"
    );
    ensure!(
        plan_run.owner_session_id == owner_session_id,
        "plan run does not belong to the supplied owner session"
    );

    match status {
        "completed" => {
            store
                .with_transaction(|tx| {
                    let changed = tx
                        .execute(
                            "UPDATE plan_runs
                         SET status = 'completed',
                             updated_at = ?2,
                             last_failure_json = NULL,
                             active_child_session_id = NULL,
                             claimed_at = NULL
                         WHERE id = ?1
                           AND owner_session_id = ?3
                           AND status = 'waiting_t2'",
                            rusqlite::params![plan_run.id, utc_timestamp(), owner_session_id],
                        )
                        .context("failed to mark plan run completed")?;
                    ensure!(
                        changed == 1,
                        "failed to mark plan run completed: plan run not found or state changed"
                    );
                    Ok(())
                })
                .context("failed to mark plan run completed atomically")?;
            let total_attempts = store.total_step_attempts_for_run(&plan_run.id)?;
            emit_plan_completed(observer.as_ref(), &plan_run, total_attempts);
        }
        other => return Err(anyhow::anyhow!("unexpected terminal status: {other}")),
    }

    Ok(())
}

fn apply_plan_escalation_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    owner_session_id: &str,
    _caused_by_turn_id: Option<&str>,
    action: &PlanAction,
) -> Result<()> {
    let plan_run_id = action
        .plan_run_id
        .as_deref()
        .context("escalate plan actions must include plan_run_id")?;
    let plan_run = store
        .get_plan_run(plan_run_id)?
        .ok_or_else(|| anyhow::anyhow!("plan run not found: {plan_run_id}"))?;
    ensure!(
        plan_run.status == "waiting_t2",
        "terminal plan actions are only allowed from waiting_t2"
    );
    ensure!(
        plan_run.owner_session_id == owner_session_id,
        "plan run does not belong to the supplied owner session"
    );

    let payload_json = build_plan_escalation_payload(&plan_run, action)?;
    let parent_session_id = store
        .get_parent_session(&plan_run.owner_session_id)?
        .unwrap_or_else(|| plan_run.owner_session_id.clone());
    let plan_run_id = plan_run.id.clone();
    let source = format!("agent-plan-{plan_run_id}");
    store
        .with_transaction(|tx| {
            let changed = tx
                .execute(
                    "UPDATE plan_runs
                 SET status = 'failed',
                     updated_at = ?2,
                     claimed_at = NULL,
                     active_child_session_id = NULL
                 WHERE id = ?1
                   AND owner_session_id = ?3
                   AND status = 'waiting_t2'",
                    rusqlite::params![plan_run_id, utc_timestamp(), owner_session_id],
                )
                .context("failed to mark plan run escalated as failed")?;
            if changed == 0 {
                return Err(anyhow::anyhow!(
                    "failed to mark plan run escalated as failed: plan run not found or state changed"
                ));
            }
            crate::store::Store::enqueue_message_in_transaction(
                tx,
                &parent_session_id,
                "user",
                &payload_json,
                &source,
            )
            .context("failed to enqueue plan escalation")?;
            Ok(())
        })
        .context("failed to apply plan escalation atomically")?;

    let total_attempts = store.total_step_attempts_for_run(&plan_run.id)?;
    emit_plan_failed(
        observer.as_ref(),
        &plan_run,
        total_attempts,
        action.note.clone(),
    );
    emit_failure_notified_to_t2(observer.as_ref(), &plan_run);

    Ok(())
}

fn load_plan_action(plan_run: &PlanRun) -> Result<PlanAction> {
    let action: PlanAction = serde_json::from_str(&plan_run.definition_json)
        .context("failed to parse stored plan definition")?;
    validate_plan_action(&action).context("stored plan definition is invalid")?;
    Ok(action)
}

fn merge_plan_actions(
    plan_run: &PlanRun,
    current_action: &PlanAction,
    patch_action: &PlanAction,
    current_step_index: usize,
) -> Result<PlanAction> {
    let mut merged_steps = current_action
        .steps
        .iter()
        .take(current_step_index)
        .cloned()
        .collect::<Vec<_>>();
    let mut seen_ids = merged_steps
        .iter()
        .map(|step| step_id(step).to_string())
        .collect::<HashSet<_>>();
    for step in &patch_action.steps {
        let id = step_id(step).to_string();
        ensure!(
            seen_ids.insert(id.clone()),
            "patched plan would reuse an immutable completed step id: {id}"
        );
        merged_steps.push(step.clone());
    }

    Ok(PlanAction {
        kind: PlanActionKind::Plan,
        plan_run_id: Some(plan_run.id.clone()),
        replace_from_step: None,
        note: patch_action.note.clone(),
        steps: merged_steps,
    })
}

fn normalize_plan_action(action: &PlanAction, plan_run_id: &str) -> Result<PlanAction> {
    Ok(PlanAction {
        kind: PlanActionKind::Plan,
        plan_run_id: Some(plan_run_id.to_string()),
        replace_from_step: None,
        note: action.note.clone(),
        steps: action.steps.clone(),
    })
}

fn build_plan_escalation_payload(plan_run: &PlanRun, action: &PlanAction) -> Result<String> {
    let payload = serde_json::json!({
        "kind": "plan_escalation",
        "plan_run_id": plan_run.id,
        "revision": plan_run.revision,
        "owner_session_id": plan_run.owner_session_id,
        "note": action.note,
        "last_failure": plan_run.last_failure_json.as_ref().and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok()),
    });
    serde_json::to_string(&payload).context("failed to serialize plan escalation payload")
}

fn step_id(step: &PlanStepSpec) -> &str {
    match step {
        PlanStepSpec::Spawn { id, .. } | PlanStepSpec::Shell { id, .. } => id,
    }
}

fn generate_plan_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_PLAN_RUN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let process_id = std::process::id();
    format!("plan-run-{nanos}-{process_id}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::test_support::RecordingObserver;
    use crate::store::{NullableUpdate, PlanRunUpdateFields};
    use crate::test_support::new_test_store;

    fn test_store(prefix: &str) -> (Store, std::path::PathBuf) {
        new_test_store(&format!("plan_patch_test_{prefix}"))
    }

    fn shell_step(id: &str, command: &str) -> PlanStepSpec {
        PlanStepSpec::Shell {
            id: id.to_string(),
            command: command.to_string(),
            timeout_ms: None,
            checks: vec![],
            max_attempts: 1,
        }
    }

    fn plan_action(
        plan_run_id: Option<&str>,
        replace_from_step: Option<usize>,
        steps: Vec<PlanStepSpec>,
    ) -> PlanAction {
        PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: plan_run_id.map(ToString::to_string),
            replace_from_step,
            note: None,
            steps,
        }
    }

    fn create_waiting_plan_run(
        store: &mut Store,
        id: &str,
        owner: &str,
        definition_json: &str,
        current_step_index: i64,
    ) -> PlanRun {
        store.create_session(owner, None).unwrap();
        store
            .create_plan_run(id, owner, definition_json, None, Some("agent"))
            .unwrap();
        store
            .update_plan_run_status(
                id,
                "waiting_t2",
                PlanRunUpdateFields {
                    current_step_index: Some(current_step_index),
                    ..PlanRunUpdateFields::default()
                },
            )
            .unwrap();
        store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE plan_runs SET claimed_at = ?2 WHERE id = ?1",
                    rusqlite::params![id, 12345_i64],
                )
                .context("failed to seed claimed_at for waiting_t2 test")?;
                Ok(())
            })
            .unwrap();
        store.get_plan_run(id).unwrap().unwrap()
    }

    #[test]
    fn apply_plan_patch_replaces_remaining_suffix_and_increments_revision() {
        let (mut store, root) = test_store("replace_suffix");
        let current = plan_action(
            Some("plan-1"),
            None,
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 1);
        let patch = plan_action(
            Some("plan-1"),
            Some(1),
            vec![shell_step("step-2b", "echo patched")],
        );

        apply_plan_patch(&mut store, "owner", "plan-1", &patch).unwrap();

        let updated = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(updated.status, "pending");
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.current_step_index, 1);
        assert!(updated.claimed_at.is_none());
        let stored: PlanAction = serde_json::from_str(&updated.definition_json).unwrap();
        assert_eq!(stored.steps.len(), 2);
        assert_eq!(stored.steps[0], current.steps[0]);
        assert_eq!(stored.steps[1], patch.steps[0]);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_rejects_reused_completed_step_id() {
        let (mut store, root) = test_store("reject_completed_step_id");
        let current = plan_action(
            Some("plan-1"),
            None,
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 1);
        let patch = plan_action(
            Some("plan-1"),
            Some(1),
            vec![shell_step("step-1", "echo duplicate")],
        );

        let err = apply_plan_patch(&mut store, "owner", "plan-1", &patch).unwrap_err();
        assert!(err.to_string().contains("immutable completed step id"));
        let stored = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(stored.revision, 1);
        assert_eq!(stored.status, "waiting_t2");
        assert!(stored.claimed_at.is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_rejects_wrong_replace_from_step() {
        let (mut store, root) = test_store("reject_replace_step");
        let current = plan_action(
            Some("plan-1"),
            None,
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 1);
        let patch = plan_action(
            Some("plan-1"),
            Some(0),
            vec![shell_step("step-2b", "echo patched")],
        );

        let err = apply_plan_patch(&mut store, "owner", "plan-1", &patch).unwrap_err();
        assert!(err.to_string().contains("remaining suffix"));
        let stored = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(stored.revision, 1);
        assert_eq!(stored.status, "waiting_t2");
        assert!(stored.claimed_at.is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_rejects_wrong_owner_session() {
        let (mut store, root) = test_store("reject_owner_patch");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 0);
        let patch = plan_action(
            Some("plan-1"),
            Some(0),
            vec![shell_step("step-1b", "echo patched")],
        );

        let err = apply_plan_patch(&mut store, "other", "plan-1", &patch).unwrap_err();
        assert!(err.to_string().contains("owner"));
        let stored = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(stored.revision, 1);
        assert_eq!(stored.status, "waiting_t2");
        assert!(stored.claimed_at.is_some());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_rejects_terminal_run_mutation() {
        let (mut store, root) = test_store("reject_terminal");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &current_json, None, Some("agent"))
            .unwrap();
        store
            .update_plan_run_status("plan-1", "completed", PlanRunUpdateFields::default())
            .unwrap();
        let patch = plan_action(
            Some("plan-1"),
            Some(0),
            vec![shell_step("step-1b", "echo patched")],
        );

        let err = apply_plan_patch(&mut store, "owner", "plan-1", &patch).unwrap_err();
        assert!(err.to_string().contains("waiting_t2"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_action_rejects_wrong_owner_session_for_terminal_actions() {
        let (mut store, root) = test_store("reject_owner_terminal");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 0);
        let done = PlanAction {
            kind: PlanActionKind::Done,
            plan_run_id: Some("plan-1".to_string()),
            replace_from_step: None,
            note: None,
            steps: vec![],
        };
        let escalate = PlanAction {
            kind: PlanActionKind::Escalate,
            plan_run_id: Some("plan-1".to_string()),
            replace_from_step: None,
            note: Some("need human review".to_string()),
            steps: vec![],
        };

        let done_err = apply_plan_action(&mut store, "other", &done).unwrap_err();
        assert!(done_err.to_string().contains("owner"));
        let escalate_err = apply_plan_action(&mut store, "other", &escalate).unwrap_err();
        assert!(escalate_err.to_string().contains("owner"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_done_marks_completed() {
        let (mut store, root) = test_store("done");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 0);
        let done = PlanAction {
            kind: PlanActionKind::Done,
            plan_run_id: Some("plan-1".to_string()),
            replace_from_step: None,
            note: None,
            steps: vec![],
        };

        apply_plan_action(&mut store, "owner", &done).unwrap();

        let stored = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(stored.status, "completed");
        assert!(stored.claimed_at.is_none());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_escalate_marks_failed_and_enqueues_upward() {
        let (mut store, root) = test_store("escalate");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        store.create_session("parent", None).unwrap();
        store.create_child_session("parent", "owner", None).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 0);
        let failure =
            serde_json::json!({"kind":"plan_failure","reason":"check_failed"}).to_string();
        store
            .update_plan_run_status(
                "plan-1",
                "waiting_t2",
                PlanRunUpdateFields {
                    last_failure_json: NullableUpdate::Value(failure.clone()),
                    ..PlanRunUpdateFields::default()
                },
            )
            .unwrap();
        let escalate = PlanAction {
            kind: PlanActionKind::Escalate,
            plan_run_id: Some("plan-1".to_string()),
            replace_from_step: None,
            note: Some("need human review".to_string()),
            steps: vec![],
        };

        apply_plan_action(&mut store, "owner", &escalate).unwrap();

        let stored = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(stored.status, "failed");
        assert!(stored.claimed_at.is_none());
        let message = store.dequeue_next_message("parent").unwrap().unwrap();
        assert_eq!(message.role, "user");
        assert_eq!(message.source, "agent-plan-plan-1");
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["kind"], "plan_escalation");
        assert_eq!(payload["plan_run_id"], "plan-1");
        assert_eq!(payload["owner_session_id"], "owner");
        assert_eq!(payload["note"], "need human review");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_action_observed_escalate_emits_failed_and_notified_events() {
        let (mut store, root) = test_store("escalate_observed");
        let current = plan_action(Some("plan-1"), None, vec![shell_step("step-1", "echo one")]);
        let current_json = serde_json::to_string(&current).unwrap();
        store.create_session("owner", None).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 0);
        store
            .record_step_attempt(crate::store::StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        let observer = RecordingObserver::new();
        let escalate = PlanAction {
            kind: PlanActionKind::Escalate,
            plan_run_id: Some("plan-1".to_string()),
            replace_from_step: None,
            note: Some("need human review".to_string()),
            steps: vec![],
        };

        apply_plan_action_observed(
            std::sync::Arc::new(observer.clone()),
            &mut store,
            "owner",
            Some("turn-3"),
            &escalate,
        )
        .unwrap();

        let events = observer.events();
        assert!(matches!(
            events.as_slice(),
            [
                TraceEvent::PlanFailed { total_attempts, reason, .. },
                TraceEvent::FailureNotifiedToT2 { .. }
            ]
            if *total_attempts == 1
                && reason.as_deref() == Some("need human review")
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_action_creates_normalized_new_run() {
        let (mut store, root) = test_store("create");
        store.create_session("owner", None).unwrap();
        let action = plan_action(
            None,
            None,
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );

        apply_plan_action(&mut store, "owner", &action).unwrap();

        let runs = store.list_plan_runs_by_session("owner").unwrap();
        assert_eq!(runs.len(), 1);
        let stored: PlanAction = serde_json::from_str(&runs[0].definition_json).unwrap();
        assert!(stored.plan_run_id.is_some());
        assert_eq!(stored.replace_from_step, None);
        assert_eq!(stored.steps, action.steps);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_action_observed_emits_created_turn_id() {
        let (mut store, root) = test_store("create_observed");
        store.create_session("owner", None).unwrap();
        let observer = RecordingObserver::new();
        let action = plan_action(None, None, vec![shell_step("step-1", "echo one")]);

        apply_plan_action_observed(
            std::sync::Arc::new(observer.clone()),
            &mut store,
            "owner",
            Some("turn-1"),
            &action,
        )
        .unwrap();

        let events = observer.events();
        assert!(matches!(
            events.as_slice(),
            [TraceEvent::PlanRunCreated { caused_by_turn_id, .. }]
            if caused_by_turn_id.as_deref() == Some("turn-1")
        ));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_patch_observed_emits_patched_turn_id() {
        let (mut store, root) = test_store("patch_observed");
        let current = plan_action(
            Some("plan-1"),
            None,
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );
        let current_json = serde_json::to_string(&current).unwrap();
        let _plan_run = create_waiting_plan_run(&mut store, "plan-1", "owner", &current_json, 1);
        let observer = RecordingObserver::new();
        let patch = plan_action(
            Some("plan-1"),
            Some(1),
            vec![shell_step("step-2b", "echo patched")],
        );

        apply_plan_patch_observed(
            std::sync::Arc::new(observer.clone()),
            &mut store,
            "owner",
            Some("turn-2"),
            "plan-1",
            &patch,
        )
        .unwrap();

        let events = observer.events();
        assert!(matches!(
            events.as_slice(),
            [TraceEvent::PlanRunPatched { caused_by_turn_id, .. }]
            if caused_by_turn_id.as_deref() == Some("turn-2")
        ));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn apply_plan_action_rejects_replace_from_step_on_create() {
        let (mut store, root) = test_store("create_reject");
        store.create_session("owner", None).unwrap();
        let action = plan_action(
            None,
            Some(0),
            vec![
                shell_step("step-1", "echo one"),
                shell_step("step-2", "echo two"),
            ],
        );

        let err = apply_plan_action(&mut store, "owner", &action).unwrap_err();
        assert!(err.to_string().contains("replace_from_step"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
