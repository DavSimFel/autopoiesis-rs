//! Step attempt persistence helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

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
    let attempts = {
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
        statement
            .query_map(params![plan_run_id], step_attempt_from_row)
            .context("failed to query crashed step attempts")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect crashed step attempts")?
    };

    for attempt in &attempts {
        let changed = tx
            .execute(
                "UPDATE plan_step_attempts
                 SET status = 'crashed',
                     finished_at = ?2
                 WHERE id = ?1
                   AND status = 'running'
                   AND finished_at IS NULL",
                params![attempt.id, utc_timestamp()],
            )
            .context("failed to crash plan step attempt")?;
        if changed == 0 {
            return Err(anyhow::anyhow!(
                "failed to crash plan step attempt: attempt not found or already finalized"
            ));
        }
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
    let changed = conn
        .execute(
            "UPDATE plan_step_attempts
             SET status = ?2,
                 finished_at = ?3
             WHERE id = ?1
               AND status = 'running'
               AND finished_at IS NULL",
            params![attempt_id, status, finished_at],
        )
        .context("failed to update plan step attempt")?;
    if changed == 0 {
        return Err(anyhow::anyhow!(
            "failed to update plan step attempt: attempt not found or already finalized"
        ));
    }
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
    let attempts = {
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
        statement
            .query_map(params![plan_run_id, revision], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .context("failed to query stale step attempts")?
            .collect::<Result<Vec<_>, _>>()
            .context("failed to read stale step attempts")?
    };

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
