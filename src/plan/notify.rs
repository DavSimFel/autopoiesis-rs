use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use serde::Serialize;
#[cfg(test)]
use std::cell::Cell;

use crate::plan::runner::{CheckVerdict, ObservedOutput, PlanFailureDetails};
use crate::store::{NullableUpdate, PlanRun, Store};
use crate::time::utc_timestamp;

#[cfg(test)]
thread_local! {
    static FORCE_NOTIFY_FAILURE_TX_ERROR_AFTER: Cell<Option<usize>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_force_notify_failure_tx_error(value: bool) {
    set_force_notify_failure_tx_error_after(value.then_some(1));
}

#[cfg(test)]
pub(crate) fn set_force_notify_failure_tx_error_after(value: Option<usize>) {
    FORCE_NOTIFY_FAILURE_TX_ERROR_AFTER.with(|flag| flag.set(value));
}

#[cfg(test)]
fn should_force_notify_failure_tx_error() -> bool {
    FORCE_NOTIFY_FAILURE_TX_ERROR_AFTER.with(|flag| {
        let current = flag.get();
        match current {
            Some(1) => {
                flag.set(None);
                true
            }
            Some(n) => {
                flag.set(Some(n.saturating_sub(1)));
                false
            }
            None => false,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PlanFailureNotificationPayload {
    kind: String,
    plan_run_id: String,
    revision: i64,
    step_index: i64,
    step_id: String,
    attempt: i64,
    reason: String,
    child_session_id: Option<String>,
    checks: Vec<PlanFailureCheckPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PlanFailureCheckPayload {
    id: String,
    verdict: String,
    observed: ObservedOutput,
}

pub(crate) fn notify_plan_failure(
    store: &mut Store,
    plan_run: &PlanRun,
    failure: &PlanFailureDetails,
) -> Result<()> {
    store
        .with_transaction(|tx| notify_plan_failure_in_transaction(tx, plan_run, failure))
        .context("failed to notify plan failure atomically")?;

    Ok(())
}

pub(crate) fn notify_plan_failure_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    plan_run: &PlanRun,
    failure: &PlanFailureDetails,
) -> Result<()> {
    let current_status: Option<String> = tx
        .query_row(
            "SELECT status FROM plan_runs WHERE id = ?1",
            rusqlite::params![plan_run.id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to inspect plan run before notifying failure")?;
    match current_status.as_deref() {
        Some("failed") => return Ok(()),
        Some(_) => {}
        None => {
            return Err(anyhow::anyhow!(
                "failed to update plan run after failure: plan run not found"
            ));
        }
    }

    let payload = build_plan_failure_payload(plan_run, failure);
    let payload_json = serde_json::to_string(&payload)
        .context("failed to serialize plan failure notification payload")?;
    crate::store::Store::enqueue_message_in_transaction(
        tx,
        &plan_run.owner_session_id,
        "user",
        &payload_json,
        &format!("agent-plan-{}", plan_run.id),
    )
    .context("failed to enqueue plan failure notification")?;

    #[cfg(test)]
    if should_force_notify_failure_tx_error() {
        return Err(anyhow::anyhow!("forced plan failure transaction error"));
    }

    match &failure.active_child_session_update {
        NullableUpdate::Unchanged => {
            let changed = tx
                .execute(
                    "UPDATE plan_runs
                     SET status = 'waiting_t2',
                         updated_at = ?2,
                         last_failure_json = ?3,
                         claimed_at = NULL
                     WHERE id = ?1",
                    rusqlite::params![plan_run.id, utc_timestamp(), &payload_json],
                )
                .context("failed to update plan run after failure")?;
            if changed == 0 {
                return Err(anyhow::anyhow!(
                    "failed to update plan run after failure: plan run not found"
                ));
            }
        }
        NullableUpdate::Null => {
            let changed = tx
                .execute(
                    "UPDATE plan_runs
                     SET status = 'waiting_t2',
                         updated_at = ?2,
                         last_failure_json = ?3,
                         claimed_at = NULL,
                         active_child_session_id = ?4
                     WHERE id = ?1",
                    rusqlite::params![
                        plan_run.id,
                        utc_timestamp(),
                        &payload_json,
                        Option::<String>::None,
                    ],
                )
                .context("failed to update plan run after failure")?;
            if changed == 0 {
                return Err(anyhow::anyhow!(
                    "failed to update plan run after failure: plan run not found"
                ));
            }
        }
        NullableUpdate::Value(value) => {
            let changed = tx
                .execute(
                    "UPDATE plan_runs
                     SET status = 'waiting_t2',
                         updated_at = ?2,
                         last_failure_json = ?3,
                         claimed_at = NULL,
                         active_child_session_id = ?4
                     WHERE id = ?1",
                    rusqlite::params![plan_run.id, utc_timestamp(), &payload_json, value],
                )
                .context("failed to update plan run after failure")?;
            if changed == 0 {
                return Err(anyhow::anyhow!(
                    "failed to update plan run after failure: plan run not found"
                ));
            }
        }
    }

    Ok(())
}

fn build_plan_failure_payload(
    plan_run: &PlanRun,
    failure: &PlanFailureDetails,
) -> PlanFailureNotificationPayload {
    PlanFailureNotificationPayload {
        kind: "plan_failure".to_string(),
        plan_run_id: plan_run.id.clone(),
        revision: plan_run.revision,
        step_index: failure.step_index,
        step_id: failure.step_id.clone(),
        attempt: failure.attempt,
        reason: failure.reason.clone(),
        child_session_id: failure.payload_child_session_id.clone(),
        checks: failure
            .checks
            .iter()
            .map(|check| PlanFailureCheckPayload {
                id: check.check_id.clone(),
                verdict: verdict_to_string(&check.verdict).to_string(),
                observed: check.observed.clone(),
            })
            .collect(),
    }
}

fn verdict_to_string(verdict: &CheckVerdict) -> &'static str {
    match verdict {
        CheckVerdict::Pass => "pass",
        CheckVerdict::Fail => "fail",
        CheckVerdict::Inconclusive => "inconclusive",
    }
}

#[cfg(test)]
mod tests {
    use super::set_force_notify_failure_tx_error;
    use super::*;
    use crate::plan::runner::{CheckOutcome, ObservedOutput, PlanFailureDetails};
    use crate::store::{NullableUpdate, PlanRunUpdateFields};

    fn test_store() -> (Store, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_plan_notify_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::new(root.join("queue.sqlite")).unwrap();
        (store, root)
    }

    fn test_plan_run(store: &mut Store) -> PlanRun {
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan","plan_run_id":null,"replace_from_step":null,"note":null,"steps":[]}"#,
                None,
                None,
            )
            .unwrap();
        store.get_plan_run("plan-1").unwrap().unwrap()
    }

    #[test]
    fn notify_plan_failure_updates_plan_run_and_enqueues_message() {
        let (mut store, root) = test_store();
        let plan_run = test_plan_run(&mut store);
        let failure = PlanFailureDetails {
            step_index: 2,
            step_id: "step-2".to_string(),
            attempt: 3,
            reason: "check_failed".to_string(),
            payload_child_session_id: Some("child-1".to_string()),
            active_child_session_update: NullableUpdate::Null,
            checks: vec![CheckOutcome {
                check_id: "check-1".to_string(),
                verdict: CheckVerdict::Fail,
                observed: ObservedOutput {
                    exit_code: Some(1),
                    stdout: "stdout".to_string(),
                    stderr: "stderr".to_string(),
                    artifact_path: Some("artifact.txt".to_string()),
                },
            }],
        };

        notify_plan_failure(&mut store, &plan_run, &failure).unwrap();

        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        let payload_json = updated.last_failure_json.as_deref().unwrap();
        let payload: serde_json::Value = serde_json::from_str(payload_json).unwrap();
        assert_eq!(payload["kind"], "plan_failure");
        assert_eq!(payload["plan_run_id"], plan_run.id);
        assert_eq!(payload["revision"], plan_run.revision);
        assert_eq!(payload["step_index"], 2);
        assert_eq!(payload["step_id"], "step-2");
        assert_eq!(payload["attempt"], 3);
        assert_eq!(payload["reason"], "check_failed");
        assert_eq!(payload["child_session_id"], "child-1");
        assert_eq!(payload["checks"][0]["id"], "check-1");
        assert_eq!(payload["checks"][0]["verdict"], "fail");
        assert_eq!(payload["checks"][0]["observed"]["exit_code"], 1);

        let message = store.dequeue_next_message("owner").unwrap().unwrap();
        assert_eq!(message.role, "user");
        assert_eq!(message.source, format!("agent-plan-{}", plan_run.id));
        assert_eq!(message.content, payload_json);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notify_plan_failure_can_clear_active_child_session_id() {
        let (mut store, root) = test_store();
        let plan_run = test_plan_run(&mut store);
        store
            .update_plan_run_status(
                &plan_run.id,
                "pending",
                PlanRunUpdateFields {
                    active_child_session_id: NullableUpdate::Value("child-2".to_string()),
                    ..PlanRunUpdateFields::default()
                },
            )
            .unwrap();
        let plan_run = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        let failure = PlanFailureDetails {
            step_index: 0,
            step_id: "spawn".to_string(),
            attempt: 0,
            reason: "spawn_failed".to_string(),
            payload_child_session_id: Some("child-2".to_string()),
            active_child_session_update: NullableUpdate::Null,
            checks: vec![],
        };

        notify_plan_failure(&mut store, &plan_run, &failure).unwrap();
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert!(updated.active_child_session_id.is_none());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notify_plan_failure_rolls_back_when_transaction_fails() {
        let (mut store, root) = test_store();
        let plan_run = test_plan_run(&mut store);
        let failure = PlanFailureDetails {
            step_index: 1,
            step_id: "step-1".to_string(),
            attempt: 0,
            reason: "boom".to_string(),
            payload_child_session_id: None,
            active_child_session_update: NullableUpdate::Unchanged,
            checks: vec![],
        };

        set_force_notify_failure_tx_error(true);
        let err = notify_plan_failure(&mut store, &plan_run, &failure).expect_err("tx should fail");
        set_force_notify_failure_tx_error(false);

        assert!(
            err.to_string()
                .contains("failed to notify plan failure atomically")
        );
        let restored = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(restored.status, "pending");
        assert_eq!(restored.last_failure_json, None);
        assert!(store.dequeue_next_message("owner").unwrap().is_none());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notify_plan_failure_does_not_resurrect_failed_plan_run() {
        let (mut store, root) = test_store();
        let plan_run = test_plan_run(&mut store);
        store
            .update_plan_run_status("plan-1", "failed", Default::default())
            .unwrap();
        let failure = PlanFailureDetails {
            step_index: 1,
            step_id: "step-1".to_string(),
            attempt: 0,
            reason: "boom".to_string(),
            payload_child_session_id: None,
            active_child_session_update: NullableUpdate::Unchanged,
            checks: vec![],
        };

        notify_plan_failure(&mut store, &plan_run, &failure).unwrap();

        let restored = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(restored.status, "failed");
        assert_eq!(restored.last_failure_json, None);
        assert!(store.dequeue_next_message("owner").unwrap().is_none());

        std::fs::remove_dir_all(root).unwrap();
    }
}
