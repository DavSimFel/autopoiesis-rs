//! Plan run persistence helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, ToSql, params};

enum PlanRunUpdateValue {
    Text(String),
    Integer(i64),
    Null,
}

impl ToSql for PlanRunUpdateValue {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            Self::Text(value) => value.to_sql(),
            Self::Integer(value) => value.to_sql(),
            Self::Null => Option::<i64>::None.to_sql(),
        }
    }
}

use super::{NullableUpdate, PlanRun, PlanRunUpdateFields, Store};
use crate::util::utc_timestamp;

pub(super) fn validate_plan_run_status(status: &str) -> Result<()> {
    match status {
        "pending" | "running" | "waiting_t2" | "completed" | "failed" => Ok(()),
        other => Err(anyhow::anyhow!("invalid plan run status: {other}")),
    }
}

pub(super) fn plan_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanRun> {
    Ok(PlanRun {
        id: row.get(0)?,
        owner_session_id: row.get(1)?,
        topic: row.get(2)?,
        trigger_source: row.get(3)?,
        status: row.get(4)?,
        revision: row.get(5)?,
        current_step_index: row.get(6)?,
        active_child_session_id: row.get(7)?,
        definition_json: row.get(8)?,
        last_failure_json: row.get(9)?,
        claimed_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn build_plan_run_status_update_sql(
    id: &str,
    status: &str,
    updated_fields: &PlanRunUpdateFields,
    include_failed_guard: bool,
) -> Result<(String, Vec<PlanRunUpdateValue>)> {
    validate_plan_run_status(status)?;
    if let Some(revision) = updated_fields.revision
        && revision <= 0
    {
        return Err(anyhow::anyhow!("invalid plan run revision: {revision}"));
    }
    if let Some(current_step_index) = updated_fields.current_step_index
        && current_step_index < 0
    {
        return Err(anyhow::anyhow!(
            "invalid plan run current step index: {current_step_index}"
        ));
    }

    let updated_at = utc_timestamp();
    let mut sql = String::from("UPDATE plan_runs SET status = ?1, updated_at = ?2");
    let mut bindings: Vec<PlanRunUpdateValue> = vec![
        PlanRunUpdateValue::Text(status.to_string()),
        PlanRunUpdateValue::Text(updated_at),
    ];
    let mut next_index = 3usize;

    if let Some(revision) = updated_fields.revision {
        sql.push_str(&format!(", revision = ?{next_index}"));
        bindings.push(PlanRunUpdateValue::Integer(revision));
        next_index += 1;
    }

    if let Some(current_step_index) = updated_fields.current_step_index {
        sql.push_str(&format!(", current_step_index = ?{next_index}"));
        bindings.push(PlanRunUpdateValue::Integer(current_step_index));
        next_index += 1;
    }

    if let Some(definition_json) = updated_fields.definition_json.clone() {
        if definition_json.is_empty() {
            return Err(anyhow::anyhow!(
                "invalid plan run definition json: definition_json must not be empty"
            ));
        }
        sql.push_str(&format!(", definition_json = ?{next_index}"));
        bindings.push(PlanRunUpdateValue::Text(definition_json));
        next_index += 1;
    }

    match &updated_fields.active_child_session_id {
        NullableUpdate::Unchanged => {}
        NullableUpdate::Null => {
            sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
            bindings.push(PlanRunUpdateValue::Null);
            next_index += 1;
        }
        NullableUpdate::Value(value) => {
            sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
            bindings.push(PlanRunUpdateValue::Text(value.clone()));
            next_index += 1;
        }
    }

    match &updated_fields.last_failure_json {
        NullableUpdate::Unchanged => {}
        NullableUpdate::Null => {
            sql.push_str(&format!(", last_failure_json = ?{next_index}"));
            bindings.push(PlanRunUpdateValue::Null);
            next_index += 1;
        }
        NullableUpdate::Value(value) => {
            sql.push_str(&format!(", last_failure_json = ?{next_index}"));
            bindings.push(PlanRunUpdateValue::Text(value.clone()));
            next_index += 1;
        }
    }

    if include_failed_guard {
        sql.push_str(&format!(" WHERE id = ?{next_index} AND status != 'failed'"));
    } else {
        sql.push_str(&format!(" WHERE id = ?{next_index}"));
    }
    bindings.push(PlanRunUpdateValue::Text(id.to_string()));

    Ok((sql, bindings))
}

pub(super) fn create_plan_run(
    conn: &Connection,
    id: &str,
    owner_session_id: &str,
    definition_json: &str,
    topic: Option<&str>,
    trigger_source: Option<&str>,
) -> Result<()> {
    if definition_json.is_empty() {
        return Err(anyhow::anyhow!(
            "invalid plan run definition json: definition_json must not be empty"
        ));
    }
    let created_at = utc_timestamp();
    conn.execute(
        "INSERT INTO plan_runs (
            id,
            owner_session_id,
            topic,
            trigger_source,
            status,
            revision,
            current_step_index,
            definition_json,
            created_at,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, 'pending', 1, 0, ?5, ?6, ?6)",
        params![
            id,
            owner_session_id,
            topic,
            trigger_source,
            definition_json,
            created_at
        ],
    )
    .context("failed to create plan run")?;
    Ok(())
}

pub(super) fn get_plan_run(conn: &Connection, id: &str) -> Result<Option<PlanRun>> {
    let mut statement = conn
        .prepare(
            "SELECT
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at
             FROM plan_runs
             WHERE id = ?1",
        )
        .context("failed to prepare get_plan_run query")?;

    match statement.query_row(params![id], plan_run_from_row) {
        Ok(plan_run) => Ok(Some(plan_run)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context("failed to read plan run"),
    }
}

pub(super) fn update_plan_run_status(
    conn: &Connection,
    id: &str,
    status: &str,
    updated_fields: PlanRunUpdateFields,
) -> Result<()> {
    let (sql, bindings) = build_plan_run_status_update_sql(id, status, &updated_fields, false)?;
    let changed = conn
        .execute(&sql, rusqlite::params_from_iter(bindings.iter()))
        .context("failed to update plan run status")?;
    if changed == 0 {
        return Err(anyhow::anyhow!(
            "failed to update plan run status: plan run not found"
        ));
    }

    Ok(())
}

pub(super) fn update_plan_run_status_preserving_failed(
    conn: &Connection,
    id: &str,
    status: &str,
    updated_fields: PlanRunUpdateFields,
) -> Result<bool> {
    let (sql, bindings) = build_plan_run_status_update_sql(id, status, &updated_fields, true)?;
    let changed = conn
        .execute(&sql, rusqlite::params_from_iter(bindings.iter()))
        .context("failed to update plan run status")?;
    Ok(changed > 0)
}

pub(super) fn claim_next_pending_plan_run(
    store: &mut Store,
    stale_after_secs: u64,
) -> Result<Option<PlanRun>> {
    store.with_transaction(|tx| claim_pending_plan_run_in_transaction(tx, stale_after_secs))
}

fn claim_pending_plan_run_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    stale_after_secs: u64,
) -> Result<Option<PlanRun>> {
    let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
    let stale_before = crate::store::unix_timestamp().saturating_sub(stale_after_secs);
    let claimed_at = crate::store::unix_timestamp();
    let updated_at = utc_timestamp();
    let mut statement = tx
        .prepare(
            "UPDATE plan_runs
             SET status = 'running', claimed_at = ?2, updated_at = ?3
             WHERE id = (
                 SELECT id
                 FROM plan_runs
                 WHERE status = 'pending'
                   AND (claimed_at IS NULL OR claimed_at <= ?1)
                 ORDER BY created_at ASC, id ASC
                 LIMIT 1
             )
             AND status = 'pending'
             AND (claimed_at IS NULL OR claimed_at <= ?1)
             RETURNING
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at",
        )
        .context("failed to prepare plan run claim query")?;

    match statement.query_row(
        params![stale_before, claimed_at, updated_at],
        plan_run_from_row,
    ) {
        Ok(plan_run) => Ok(Some(plan_run)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context("failed to claim plan run"),
    }
}

pub(super) fn claim_next_runnable_plan_run(
    conn: &Connection,
    stale_after_secs: u64,
) -> Result<Option<PlanRun>> {
    let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
    let stale_before = crate::store::unix_timestamp().saturating_sub(stale_after_secs);
    let claimed_at = crate::store::unix_timestamp();
    let updated_at = utc_timestamp();
    let mut statement = conn
        .prepare(
            "UPDATE plan_runs
             SET status = 'running', claimed_at = ?2, updated_at = ?3
             WHERE id = (
                 SELECT id
                 FROM plan_runs
                 WHERE status = 'pending'
                   AND (claimed_at IS NULL OR claimed_at <= ?1)
                 ORDER BY
                     created_at ASC,
                     id ASC
                 LIMIT 1
             )
             AND status = 'pending'
             AND (claimed_at IS NULL OR claimed_at <= ?1)
             RETURNING
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at",
        )
        .context("failed to prepare runnable plan run claim query")?;

    match statement.query_row(
        params![stale_before, claimed_at, updated_at],
        plan_run_from_row,
    ) {
        Ok(plan_run) => Ok(Some(plan_run)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context("failed to claim runnable plan run"),
    }
}

pub(super) fn release_plan_run_claim(conn: &Connection, id: &str) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE plan_runs
             SET claimed_at = NULL,
                 updated_at = ?2
             WHERE id = ?1
               AND status != 'running'",
            params![id, utc_timestamp()],
        )
        .context("failed to release plan run claim")?;
    if changed == 0 {
        let current_status: Option<String> = conn
            .query_row(
                "SELECT status FROM plan_runs WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read plan run status before release")?;

        let Some(current_status) = current_status else {
            return Err(anyhow::anyhow!(
                "failed to release plan run claim: plan run not found"
            ));
        };
        if current_status == "running" {
            return Err(anyhow::anyhow!(
                "failed to release plan run claim: plan run is still running"
            ));
        }
        validate_plan_run_status(&current_status)?;
        return Err(anyhow::anyhow!(
            "failed to release plan run claim: plan run claim was not updated"
        ));
    }
    Ok(())
}

pub(super) fn list_plan_runs_by_session(
    conn: &Connection,
    owner_session_id: &str,
) -> Result<Vec<PlanRun>> {
    let mut statement = conn
        .prepare(
            "SELECT
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at
             FROM plan_runs
             WHERE owner_session_id = ?1
             ORDER BY created_at ASC, id ASC",
        )
        .context("failed to prepare list_plan_runs_by_session query")?;

    let plan_runs = statement
        .query_map(params![owner_session_id], plan_run_from_row)
        .context("failed to query plan runs")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect plan runs")?;

    Ok(plan_runs)
}

pub(super) fn list_recent_plan_runs(conn: &Connection, limit: usize) -> Result<Vec<PlanRun>> {
    let mut statement = conn
        .prepare(
            "SELECT
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at
             FROM plan_runs
             ORDER BY updated_at DESC, created_at DESC, id DESC
             LIMIT ?1",
        )
        .context("failed to prepare list_recent_plan_runs query")?;

    let plan_runs = statement
        .query_map(params![limit as i64], plan_run_from_row)
        .context("failed to query recent plan runs")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect recent plan runs")?;

    Ok(plan_runs)
}

pub(super) fn list_recent_active_plan_runs(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<PlanRun>> {
    let mut statement = conn
        .prepare(
            "SELECT
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at
             FROM plan_runs
             WHERE status IN ('pending', 'running', 'waiting_t2')
             ORDER BY updated_at DESC, created_at DESC, id DESC
             LIMIT ?1",
        )
        .context("failed to prepare list_recent_active_plan_runs query")?;

    let plan_runs = statement
        .query_map(params![limit as i64], plan_run_from_row)
        .context("failed to query recent active plan runs")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect recent active plan runs")?;

    Ok(plan_runs)
}

pub(super) fn recover_stale_plan_runs(store: &mut Store, stale_after_secs: u64) -> Result<u64> {
    let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
    let stale_before = crate::store::unix_timestamp().saturating_sub(stale_after_secs);
    store.with_transaction(|tx| {
        let pending_count = tx
            .execute(
                "UPDATE plan_runs
                 SET claimed_at = NULL,
                     updated_at = ?2
                 WHERE status = 'pending'
                   AND claimed_at IS NOT NULL
                   AND claimed_at <= ?1",
                params![stale_before, utc_timestamp()],
            )
            .context("failed to recover stale pending plan runs")?;
        let leaked_count = tx
            .execute(
                "UPDATE plan_runs
                 SET claimed_at = NULL,
                     updated_at = ?2
                 WHERE status IN ('waiting_t2', 'completed', 'failed')
                   AND claimed_at IS NOT NULL
                   AND claimed_at <= ?1",
                params![stale_before, utc_timestamp()],
            )
            .context("failed to clear stale plan run claims")?;
        Ok((pending_count + leaked_count) as u64)
    })
}

pub(super) fn resume_waiting_plan_run(conn: &Connection, id: &str) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE plan_runs
             SET status = 'pending',
                 claimed_at = NULL,
                 last_failure_json = NULL,
                 updated_at = ?2
             WHERE id = ?1
               AND status = 'waiting_t2'",
            params![id, utc_timestamp()],
        )
        .context("failed to resume waiting plan run")?;
    Ok(changed > 0)
}

pub(super) fn cancel_plan_run(conn: &Connection, id: &str) -> Result<bool> {
    let changed = conn
        .execute(
            "UPDATE plan_runs
             SET status = 'failed',
                 claimed_at = NULL,
                 last_failure_json = NULL,
                 updated_at = ?2
             WHERE id = ?1
               AND status IN ('pending', 'running', 'waiting_t2')",
            params![id, utc_timestamp()],
        )
        .context("failed to cancel plan run")?;
    Ok(changed > 0)
}

pub(super) fn list_stale_running_plan_runs(
    conn: &Connection,
    stale_after_secs: u64,
) -> Result<Vec<PlanRun>> {
    let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
    let stale_before = crate::store::unix_timestamp().saturating_sub(stale_after_secs);
    let mut statement = conn
        .prepare(
            "SELECT
                id,
                owner_session_id,
                topic,
                trigger_source,
                status,
                revision,
                current_step_index,
                active_child_session_id,
                definition_json,
                last_failure_json,
                claimed_at,
                created_at,
                updated_at
             FROM plan_runs
             WHERE status = 'running'
               AND claimed_at IS NOT NULL
               AND claimed_at <= ?1
             ORDER BY claimed_at ASC, created_at ASC, id ASC",
        )
        .context("failed to prepare list_stale_running_plan_runs query")?;

    let plan_runs = statement
        .query_map(params![stale_before], plan_run_from_row)
        .context("failed to query stale running plan runs")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect stale running plan runs")?;

    Ok(plan_runs)
}
