use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;

use crate::observe::{Observer, TraceEvent, runtime_observer};
use crate::plan::notify::{emit_failure_notified_to_t2, notify_plan_failure_in_transaction};
use crate::plan::{CheckOutcome, PlanAction, PlanFailureDetails, PlanStepSpec};
use crate::store::{NullableUpdate, PlanRun, StepAttempt, Store};

pub fn recover_crashed_plans(
    store: &mut Store,
    sessions_dir: &Path,
    stale_after_secs: u64,
) -> Result<u64> {
    recover_crashed_plans_observed(runtime_observer(sessions_dir), store, stale_after_secs)
}

pub fn recover_crashed_plans_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    stale_after_secs: u64,
) -> Result<u64> {
    let stale_runs = store.list_stale_running_plan_runs(stale_after_secs)?;
    let mut recovered = 0u64;

    for plan_run in stale_runs {
        crash_plan_run_to_waiting_t2_observed(observer.clone(), store, &plan_run)
            .with_context(|| format!("failed to recover crashed plan run {}", plan_run.id))?;
        recovered += 1;
    }

    Ok(recovered)
}

pub(crate) fn crash_plan_run_to_waiting_t2_observed(
    observer: Arc<dyn Observer>,
    store: &mut Store,
    plan_run: &PlanRun,
) -> Result<PlanFailureDetails> {
    let max_attempt_index = store
        .max_step_attempt_index_for_run(&plan_run.id)
        .context("failed to derive maximum step attempt index for crashed plan run")?;
    let (failure, notified) = store
        .with_transaction(|tx| {
            let crashed_attempts =
                Store::crash_running_step_attempts_for_run_in_transaction(tx, &plan_run.id)
                    .context("failed to crash running step attempts for stale plan run")?;
            let failure =
                build_failure_details(tx, plan_run, &crashed_attempts, max_attempt_index)?;
            let notified = notify_plan_failure_in_transaction(tx, plan_run, &failure)
                .context("failed to transition plan run to waiting_t2 and enqueue failure")?;
            Ok((failure, notified))
        })
        .context("failed to hand off crashed plan run to T2")?;

    emit_plan_recovered(observer.as_ref(), plan_run);
    observer.emit(&TraceEvent::PlanWaitingT2 {
        session_id: plan_run.owner_session_id.clone(),
        plan_run_id: plan_run.id.clone(),
        step_index: failure.step_index,
        reason: Some(failure.reason.clone()),
    });
    if notified {
        emit_failure_notified_to_t2(observer.as_ref(), plan_run);
    }

    Ok(failure)
}

fn emit_plan_recovered(observer: &dyn Observer, plan_run: &PlanRun) {
    observer.emit(&TraceEvent::PlanRecovered {
        session_id: plan_run.owner_session_id.clone(),
        plan_run_id: plan_run.id.clone(),
        reason: Some("crashed".to_string()),
    });
}

fn build_failure_details(
    tx: &rusqlite::Transaction<'_>,
    plan_run: &PlanRun,
    crashed_attempts: &[StepAttempt],
    max_attempt_index: i64,
) -> Result<PlanFailureDetails> {
    if let Some(attempt) = crashed_attempts.first() {
        let checks: Vec<CheckOutcome> = serde_json::from_str(&attempt.checks_json)
            .context("failed to parse crashed plan step checks_json")?;
        let child_session_id = attempt
            .child_session_id
            .clone()
            .or_else(|| plan_run.active_child_session_id.clone());
        return Ok(PlanFailureDetails {
            step_index: attempt.step_index,
            step_id: attempt.step_id.clone(),
            attempt: attempt.attempt,
            reason: "crashed".to_string(),
            payload_child_session_id: child_session_id.clone(),
            active_child_session_update: nullable_update_for_child_session(child_session_id),
            checks,
        });
    }

    let step_id =
        derive_step_id_from_definition(plan_run).unwrap_or("__unknown_step__".to_string());
    let child_session_id = plan_run.active_child_session_id.clone();
    let attempt = if step_id == "__unknown_step__" {
        if max_attempt_index >= 0 {
            max_attempt_index + 1
        } else {
            0
        }
    } else {
        next_attempt_index_for_step(tx, plan_run)?
    };

    Ok(PlanFailureDetails {
        step_index: plan_run.current_step_index,
        step_id,
        attempt,
        reason: "crashed".to_string(),
        payload_child_session_id: child_session_id.clone(),
        active_child_session_update: nullable_update_for_child_session(child_session_id),
        checks: Vec::new(),
    })
}

fn derive_step_id_from_definition(plan_run: &PlanRun) -> Option<String> {
    if plan_run.current_step_index < 0 {
        return None;
    }

    let plan_action: PlanAction = serde_json::from_str(&plan_run.definition_json).ok()?;
    let step = plan_action
        .steps
        .get(plan_run.current_step_index as usize)?;
    match step {
        PlanStepSpec::Spawn { id, .. } | PlanStepSpec::Shell { id, .. } => Some(id.clone()),
    }
}

fn next_attempt_index_for_step(tx: &rusqlite::Transaction<'_>, plan_run: &PlanRun) -> Result<i64> {
    let mut statement = tx
        .prepare(
            "SELECT attempt
             FROM plan_step_attempts
             WHERE plan_run_id = ?1
               AND revision = ?2
               AND step_index = ?3
             ORDER BY attempt DESC, id DESC
             LIMIT 1",
        )
        .context("failed to prepare next attempt derivation query")?;

    match statement.query_row(
        rusqlite::params![plan_run.id, plan_run.revision, plan_run.current_step_index],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(max_attempt) => Ok(max_attempt + 1),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        Err(error) => Err(error).context("failed to derive next step attempt index"),
    }
}

fn nullable_update_for_child_session(child_session_id: Option<String>) -> NullableUpdate<String> {
    match child_session_id {
        Some(value) => NullableUpdate::Value(value),
        None => NullableUpdate::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::TraceEvent;
    use crate::observe::test_support::RecordingObserver;
    use crate::plan::notify::set_force_notify_failure_tx_error_after;
    use crate::store::StepAttemptRecord;
    use crate::test_support::new_test_store;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_store() -> (Store, std::path::PathBuf) {
        new_test_store("plan_recovery_test")
    }

    fn valid_definition(step_id: &str) -> String {
        format!(
            r#"{{"kind":"plan","plan_run_id":null,"replace_from_step":null,"note":null,"steps":[{{"kind":"shell","id":"{}","command":"echo hi","checks":[],"max_attempts":1}}]}}"#,
            step_id
        )
    }

    fn unix_timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn stale_claim(store: &mut Store, plan_run_id: &str, offset_secs: i64) {
        store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                    rusqlite::params![unix_timestamp() - offset_secs, plan_run_id],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn recover_crashed_plans_marks_running_attempts_crashed_and_sets_waiting_t2() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &valid_definition("step-1"), None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: Some("child-a".to_string()),
                summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                checks_json: r#"[{"check_id":"check-a","verdict":"Pass","observed":{"exit_code":0,"stdout":"","stderr":"","artifact_path":null}}]"#.to_string(),
            })
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 1,
                status: "running".to_string(),
                child_session_id: Some("child-b".to_string()),
                summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                checks_json: r#"[{"check_id":"check-b","verdict":"Fail","observed":{"exit_code":1,"stdout":"","stderr":"","artifact_path":null}}]"#.to_string(),
            })
            .unwrap();
        stale_claim(&mut store, "plan-1", 301);

        let recovered = recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap();
        assert_eq!(recovered, 1);

        let plan_run = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(plan_run.status, "waiting_t2");
        assert_eq!(plan_run.claimed_at, None);
        assert!(plan_run.last_failure_json.is_some());

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert!(attempts.iter().all(|attempt| attempt.status == "crashed"));
        assert!(attempts.iter().all(|attempt| attempt.finished_at.is_some()));
        assert_eq!(
            store.dequeue_next_message("owner").unwrap().unwrap().source,
            "agent-plan-plan-1"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn crash_plan_run_to_waiting_t2_observed_emits_recovery_events() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &valid_definition("step-1"), None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: Some("child-a".to_string()),
                summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                checks_json: r#"[]"#.to_string(),
            })
            .unwrap();
        let plan_run = store.get_plan_run("plan-1").unwrap().unwrap();
        let observer = RecordingObserver::new();

        let failure = crash_plan_run_to_waiting_t2_observed(
            std::sync::Arc::new(observer.clone()),
            &mut store,
            &plan_run,
        )
        .unwrap();

        let events = observer.events();
        assert!(matches!(
            events.as_slice(),
            [
                TraceEvent::PlanRecovered { plan_run_id, .. },
                TraceEvent::PlanWaitingT2 { plan_run_id: waiting_id, .. },
                TraceEvent::FailureNotifiedToT2 { plan_run_id: notified_id, .. }
            ]
            if plan_run_id == &plan_run.id
                && waiting_id == &plan_run.id
                && notified_id == &plan_run.id
        ));
        assert_eq!(failure.reason, "crashed");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recover_crashed_plans_notifies_owner_t2_session() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &valid_definition("step-1"), None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                checks_json: r#"[]"#.to_string(),
            })
            .unwrap();
        stale_claim(&mut store, "plan-1", 301);

        let recovered = recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap();
        assert_eq!(recovered, 1);

        let message = store.dequeue_next_message("owner").unwrap().unwrap();
        assert_eq!(message.role, "user");
        assert_eq!(message.source, "agent-plan-plan-1");
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["kind"], "plan_failure");
        assert_eq!(payload["plan_run_id"], "plan-1");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recover_crashed_plans_derives_step_and_attempt_when_no_running_attempt_exists() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &valid_definition("step-a"), None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO plan_step_attempts (
                        plan_run_id,
                        revision,
                        step_index,
                        step_id,
                        attempt,
                        status,
                        child_session_id,
                        summary_json,
                        checks_json,
                        started_at,
                        finished_at
                    ) VALUES (?1, 1, 0, ?2, 2, 'passed', NULL, ?3, '[]', ?4, ?4)",
                    rusqlite::params!["plan-1", "step-a", "{}", unix_timestamp()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        stale_claim(&mut store, "plan-1", 301);

        let recovered = recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap();
        assert_eq!(recovered, 1);

        let message = store.dequeue_next_message("owner").unwrap().unwrap();
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["step_id"], "step-a");
        assert_eq!(payload["attempt"], 3);
        assert_eq!(payload["checks"], serde_json::json!([]));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recover_crashed_plans_uses_unknown_step_fallback_when_definition_lookup_is_invalid() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{\"kind\":\"plan\"}", None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .with_transaction(|tx| {
                tx.execute(
                    "UPDATE plan_runs SET claimed_at = ?1, current_step_index = 9 WHERE id = ?2",
                    rusqlite::params![unix_timestamp() - 301, "plan-1"],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO plan_step_attempts (
                        plan_run_id,
                        revision,
                        step_index,
                        step_id,
                        attempt,
                        status,
                        child_session_id,
                        summary_json,
                        checks_json,
                        started_at,
                        finished_at
                    ) VALUES (?1, 1, 0, ?2, 4, 'passed', NULL, ?3, '[]', ?4, ?4)",
                    rusqlite::params!["plan-1", "step-z", "{}", unix_timestamp()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        stale_claim(&mut store, "plan-1", 301);

        let recovered = recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap();
        assert_eq!(recovered, 1);

        let message = store.dequeue_next_message("owner").unwrap().unwrap();
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(payload["step_id"], "__unknown_step__");
        assert_eq!(payload["attempt"], 5);
        assert_eq!(payload["checks"], serde_json::json!([]));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recover_crashed_plans_fails_fast_without_partial_waiting_t2_state_on_notification_error() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        for id in ["plan-1", "plan-2"] {
            store
                .create_plan_run(id, "owner", &valid_definition("step-1"), None, None)
                .unwrap();
            store
                .update_plan_run_status(id, "running", Default::default())
                .unwrap();
            store
                .record_step_attempt(StepAttemptRecord {
                    plan_run_id: id.to_string(),
                    revision: 1,
                    step_index: 0,
                    step_id: "step-1".to_string(),
                    attempt: 0,
                    status: "running".to_string(),
                    child_session_id: None,
                    summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                    checks_json: r#"[]"#.to_string(),
                })
                .unwrap();
            stale_claim(&mut store, id, 301);
        }

        set_force_notify_failure_tx_error_after(Some(2));
        let err = recover_crashed_plans(&mut store, Path::new("sessions"), 300)
            .expect_err("second recovery should fail");
        set_force_notify_failure_tx_error_after(None);

        assert!(
            err.to_string()
                .contains("failed to recover crashed plan run plan-2")
        );
        assert_eq!(
            store.get_plan_run("plan-1").unwrap().unwrap().status,
            "waiting_t2"
        );
        let plan_2 = store.get_plan_run("plan-2").unwrap().unwrap();
        assert_eq!(plan_2.status, "running");
        assert!(store.dequeue_next_message("owner").unwrap().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn recover_crashed_plans_is_idempotent_after_successful_pass() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", &valid_definition("step-1"), None, None)
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", Default::default())
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"kind":"plan_step_summary"}"#.to_string(),
                checks_json: r#"[]"#.to_string(),
            })
            .unwrap();
        stale_claim(&mut store, "plan-1", 301);

        assert_eq!(
            recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap(),
            1
        );
        assert_eq!(
            recover_crashed_plans(&mut store, Path::new("sessions"), 300).unwrap(),
            0
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
