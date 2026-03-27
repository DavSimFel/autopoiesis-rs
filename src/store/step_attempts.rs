//! Step attempt persistence helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, Params, params};

use super::{PlanRun, StepAttempt, StepAttemptRecord, Store, step_attempt_from_row};
use crate::time::utc_timestamp;

fn validate_step_attempt_start_status(status: &str) -> Result<()> {
    match status {
        "running" => Ok(()),
        other => Err(anyhow::anyhow!(
            "invalid plan step attempt start status: {other}"
        )),
    }
}

fn validate_step_attempt_terminal_status(status: &str) -> Result<()> {
    match status {
        "passed" | "failed" | "crashed" => Ok(()),
        other => Err(anyhow::anyhow!(
            "invalid plan step attempt terminal status: {other}"
        )),
    }
}

fn execute_running_step_attempt_update<P>(
    conn: &Connection,
    sql: &str,
    params: P,
    context: &str,
) -> Result<()>
where
    P: Params,
{
    let changed = conn
        .execute(sql, params)
        .with_context(|| context.to_string())?;
    if changed == 0 {
        return Err(anyhow::anyhow!(
            "failed to update plan step attempt: attempt not found or already finalized"
        ));
    }
    Ok(())
}

fn collect_step_attempt_rows<T, F>(
    statement: &mut rusqlite::Statement<'_>,
    params: impl Params,
    context: &str,
    mut map: F,
) -> Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
{
    let rows = statement
        .query_map(params, |row| map(row))
        .with_context(|| context.to_string())?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| context.to_string())
}

pub(super) fn next_step_attempt_index_for_run(
    conn: &Connection,
    plan_run: &PlanRun,
) -> Result<i64> {
    let attempts = get_step_attempts(conn, &plan_run.id, plan_run.current_step_index)?;
    Ok(attempts
        .into_iter()
        .filter(|attempt| attempt.revision == plan_run.revision)
        .map(|attempt| attempt.attempt)
        .max()
        .map(|attempt| attempt + 1)
        .unwrap_or(0))
}

pub(super) fn max_step_attempt_index_for_run(conn: &Connection, plan_run_id: &str) -> Result<i64> {
    let max_attempt: Option<i64> = conn
        .query_row(
            "SELECT MAX(attempt) FROM plan_step_attempts WHERE plan_run_id = ?1",
            params![plan_run_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .context("failed to query maximum plan step attempt")?;
    Ok(max_attempt.unwrap_or(-1))
}

pub(super) fn crash_running_step_attempts_for_run(
    store: &mut Store,
    plan_run_id: &str,
) -> Result<Vec<StepAttempt>> {
    store.with_transaction(|tx| crash_running_step_attempts_for_run_in_transaction(tx, plan_run_id))
}

pub(super) fn crash_running_step_attempts_for_run_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    plan_run_id: &str,
) -> Result<Vec<StepAttempt>> {
    let mut statement = tx
        .prepare(
            "SELECT
                    id,
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
                 FROM plan_step_attempts
                 WHERE plan_run_id = ?1
                   AND status = 'running'
                   AND finished_at IS NULL
                 ORDER BY revision DESC, step_index DESC, attempt DESC, id DESC",
        )
        .context("failed to prepare crashed step attempt recovery query")?;
    let attempts = collect_step_attempt_rows(
        &mut statement,
        params![plan_run_id],
        "failed to query crashed step attempts",
        step_attempt_from_row,
    )?;

    for attempt in &attempts {
        execute_running_step_attempt_update(
            tx,
            "UPDATE plan_step_attempts
             SET status = 'crashed',
                 finished_at = ?2
             WHERE id = ?1
               AND status = 'running'
               AND finished_at IS NULL",
            params![attempt.id, utc_timestamp()],
            "failed to crash plan step attempt",
        )?;
    }

    Ok(attempts)
}

pub(super) fn record_step_attempt(conn: &Connection, record: StepAttemptRecord) -> Result<i64> {
    validate_step_attempt_start_status(&record.status)?;
    if record.revision <= 0 {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt revision: {}",
            record.revision
        ));
    }
    if record.step_index < 0 {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt step index: {}",
            record.step_index
        ));
    }
    if record.step_id.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt step id: step_id must not be empty"
        ));
    }
    if record.attempt < 0 {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt index: {}",
            record.attempt
        ));
    }
    conn.execute(
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
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
        params![
            record.plan_run_id,
            record.revision,
            record.step_index,
            record.step_id,
            record.attempt,
            record.status,
            record.child_session_id,
            record.summary_json,
            record.checks_json,
            utc_timestamp(),
        ],
    )
    .context("failed to record plan step attempt")?;
    Ok(conn.last_insert_rowid())
}

pub(super) fn update_step_attempt_status(
    conn: &Connection,
    attempt_id: i64,
    status: &str,
    finished_at: &str,
) -> Result<()> {
    validate_step_attempt_terminal_status(status)?;
    if finished_at.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt finished_at: finished_at must not be empty"
        ));
    }
    execute_running_step_attempt_update(
        conn,
        "UPDATE plan_step_attempts
         SET status = ?2,
             finished_at = ?3
         WHERE id = ?1
           AND status = 'running'
           AND finished_at IS NULL",
        params![attempt_id, status, finished_at],
        "failed to update plan step attempt",
    )?;
    Ok(())
}

pub(super) fn update_step_attempt_child_session(
    conn: &Connection,
    attempt_id: i64,
    child_session_id: Option<&str>,
) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE plan_step_attempts
             SET child_session_id = ?2
             WHERE id = ?1
               AND status = 'running'
               AND finished_at IS NULL",
            params![attempt_id, child_session_id],
        )
        .context("failed to update plan step attempt child session")?;
    if changed == 0 {
        return Err(anyhow::anyhow!(
            "failed to update plan step attempt child session: attempt not found or already finalized"
        ));
    }
    Ok(())
}

pub(super) fn finalize_step_attempt(
    conn: &Connection,
    attempt_id: i64,
    status: &str,
    finished_at: &str,
    summary_json: &str,
    checks_json: &str,
) -> Result<()> {
    validate_step_attempt_terminal_status(status)?;
    if finished_at.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt finished_at: finished_at must not be empty"
        ));
    }
    if summary_json.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt summary_json: summary_json must not be empty"
        ));
    }
    if checks_json.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan step attempt checks_json: checks_json must not be empty"
        ));
    }
    let changed = conn
        .execute(
            "UPDATE plan_step_attempts
             SET status = ?2,
                 summary_json = ?3,
                 checks_json = ?4,
                 finished_at = ?5
             WHERE id = ?1
               AND status = 'running'
               AND finished_at IS NULL",
            params![attempt_id, status, summary_json, checks_json, finished_at],
        )
        .context("failed to finalize plan step attempt")?;
    if changed == 0 {
        return Err(anyhow::anyhow!(
            "failed to finalize plan step attempt: attempt not found or already finalized"
        ));
    }
    Ok(())
}

pub(super) fn finalize_stale_step_attempts(
    conn: &Connection,
    plan_run_id: &str,
    revision: i64,
) -> Result<u64> {
    let mut statement = conn
        .prepare(
            "SELECT id, summary_json, checks_json
                 FROM plan_step_attempts
                 WHERE plan_run_id = ?1
                   AND revision = ?2
                   AND status = 'running'
                   AND finished_at IS NULL",
        )
        .context("failed to prepare stale step attempt recovery query")?;
    let attempts = collect_step_attempt_rows(
        &mut statement,
        params![plan_run_id, revision],
        "failed to query stale step attempts",
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;

    let mut finalized = 0;
    for attempt in attempts {
        let (attempt_id, summary_json, checks_json) = attempt;
        finalize_step_attempt(
            conn,
            attempt_id,
            "crashed",
            &utc_timestamp(),
            &summary_json,
            &checks_json,
        )?;
        finalized += 1;
    }
    Ok(finalized)
}

pub(super) fn get_step_attempts(
    conn: &Connection,
    plan_run_id: &str,
    step_index: i64,
) -> Result<Vec<StepAttempt>> {
    let mut statement = conn
        .prepare(
            "SELECT
                id,
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
             FROM plan_step_attempts
             WHERE plan_run_id = ?1 AND step_index = ?2
             ORDER BY revision ASC, attempt ASC, id ASC",
        )
        .context("failed to prepare get_step_attempts query")?;

    let attempts = statement
        .query_map(params![plan_run_id, step_index], step_attempt_from_row)
        .context("failed to query plan step attempts")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect plan step attempts")?;

    Ok(attempts)
}

pub(super) fn total_step_attempts_for_run(conn: &Connection, plan_run_id: &str) -> Result<i64> {
    let total_attempts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM plan_step_attempts WHERE plan_run_id = ?1",
            params![plan_run_id],
            |row| row.get(0),
        )
        .context("failed to count plan step attempts")?;
    Ok(total_attempts)
}

impl super::Store {
    pub fn next_step_attempt_index_for_run(&self, plan_run: &PlanRun) -> Result<i64> {
        next_step_attempt_index_for_run(&self.conn, plan_run)
    }

    pub fn max_step_attempt_index_for_run(&self, plan_run_id: &str) -> Result<i64> {
        max_step_attempt_index_for_run(&self.conn, plan_run_id)
    }

    pub fn total_step_attempts_for_run(&self, plan_run_id: &str) -> Result<i64> {
        total_step_attempts_for_run(&self.conn, plan_run_id)
    }

    pub fn crash_running_step_attempts_for_run(
        &mut self,
        plan_run_id: &str,
    ) -> Result<Vec<StepAttempt>> {
        crash_running_step_attempts_for_run(self, plan_run_id)
    }

    pub(crate) fn crash_running_step_attempts_for_run_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        plan_run_id: &str,
    ) -> Result<Vec<StepAttempt>> {
        crash_running_step_attempts_for_run_in_transaction(tx, plan_run_id)
    }

    pub fn record_step_attempt(&mut self, record: StepAttemptRecord) -> Result<i64> {
        record_step_attempt(&self.conn, record)
    }

    pub fn update_step_attempt_status(
        &mut self,
        attempt_id: i64,
        status: &str,
        finished_at: &str,
    ) -> Result<()> {
        update_step_attempt_status(&self.conn, attempt_id, status, finished_at)
    }

    pub fn update_step_attempt_child_session(
        &mut self,
        attempt_id: i64,
        child_session_id: Option<&str>,
    ) -> Result<()> {
        update_step_attempt_child_session(&self.conn, attempt_id, child_session_id)
    }

    pub fn finalize_step_attempt(
        &mut self,
        attempt_id: i64,
        status: &str,
        finished_at: &str,
        summary_json: &str,
        checks_json: &str,
    ) -> Result<()> {
        finalize_step_attempt(
            &self.conn,
            attempt_id,
            status,
            finished_at,
            summary_json,
            checks_json,
        )
    }

    pub fn finalize_stale_step_attempts(
        &mut self,
        plan_run_id: &str,
        revision: i64,
    ) -> Result<u64> {
        finalize_stale_step_attempts(&self.conn, plan_run_id, revision)
    }

    pub fn get_step_attempts(
        &self,
        plan_run_id: &str,
        step_index: i64,
    ) -> Result<Vec<StepAttempt>> {
        get_step_attempts(&self.conn, plan_run_id, step_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::new_test_store;

    fn test_store() -> (Store, std::path::PathBuf) {
        new_test_store("step_attempts_test")
    }

    #[test]
    fn total_step_attempts_for_run_counts_all_attempts() {
        let (mut store, root) = test_store();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan","plan_run_id":null,"replace_from_step":null,"note":null,"steps":[{"kind":"shell","id":"step-1","command":"echo hi","timeout_ms":null,"checks":[],"max_attempts":1}]}"#,
                None,
                None,
            )
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
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
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
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 2,
                step_index: 0,
                step_id: "step-1".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        assert_eq!(store.total_step_attempts_for_run("plan-1").unwrap(), 3);
        let _ = std::fs::remove_dir_all(root);
    }
}
