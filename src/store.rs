//! SQLite-backed session registry and per-session message queue.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params, types::ToSql};

use crate::util::utc_timestamp;

#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub source: String,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct SubscriptionRow {
    pub id: i64,
    pub topic: String,
    pub path: String,
    pub filter: Option<String>,
    pub activated_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRun {
    pub id: String,
    pub owner_session_id: String,
    pub topic: Option<String>,
    pub trigger_source: Option<String>,
    pub status: String,
    pub revision: i64,
    pub current_step_index: i64,
    pub active_child_session_id: Option<String>,
    pub definition_json: String,
    pub last_failure_json: Option<String>,
    pub claimed_at: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAttempt {
    pub id: i64,
    pub plan_run_id: String,
    pub revision: i64,
    pub step_index: i64,
    pub step_id: String,
    pub attempt: i64,
    pub status: String,
    pub child_session_id: Option<String>,
    pub summary_json: String,
    pub checks_json: String,
    pub started_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StepAttemptRecord {
    pub plan_run_id: String,
    pub revision: i64,
    pub step_index: i64,
    pub step_id: String,
    pub attempt: i64,
    pub status: String,
    pub child_session_id: Option<String>,
    pub summary_json: String,
    pub checks_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NullableUpdate<T> {
    #[default]
    Unchanged,
    Null,
    Value(T),
}

#[derive(Debug, Clone, Default)]
pub struct PlanRunUpdateFields {
    pub revision: Option<i64>,
    pub current_step_index: Option<i64>,
    pub definition_json: Option<String>,
    pub active_child_session_id: NullableUpdate<String>,
    pub last_failure_json: NullableUpdate<String>,
}

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(path).context("failed to open sqlite store")?;
        conn.busy_timeout(Duration::from_secs(5))
            .context("failed to configure sqlite busy timeout")?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL,
                parent_session_id TEXT REFERENCES sessions(id)
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                claimed_at INTEGER,
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session_status_created_at
                ON messages(session_id, status, created_at, id);
            CREATE TABLE IF NOT EXISTS subscriptions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                topic TEXT NOT NULL DEFAULT '_default',
                path TEXT NOT NULL,
                filter TEXT,
                activated_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_subscriptions_topic_path
                ON subscriptions(topic, path);
            CREATE INDEX IF NOT EXISTS idx_subscriptions_timestamps
                ON subscriptions(updated_at, activated_at, id);
            "#,
        )
        .context("failed to initialize sqlite schema")?;
        ensure_sessions_parent_session_id_column(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_parent_session_id_created_at
             ON sessions(parent_session_id, created_at, id)",
            [],
        )
        .context("failed to initialize parent session index")?;
        ensure_messages_claimed_at_column(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_status_claimed_at
             ON messages(status, claimed_at, id)",
            [],
        )
        .context("failed to initialize claimed_at index")?;
        ensure_plan_runs_table(&conn)?;

        Ok(Self { conn })
    }

    pub fn create_session(&mut self, session_id: &str, metadata: Option<&str>) -> Result<()> {
        let metadata = metadata.unwrap_or("{}");
        self.conn
            .execute(
                "INSERT OR IGNORE INTO sessions (id, created_at, metadata) VALUES (?1, ?2, ?3)",
                params![session_id, utc_timestamp(), metadata],
            )
            .context("failed to create session")?;
        Ok(())
    }

    /// Insert a child session that records its parent session id.
    pub fn create_child_session(
        &mut self,
        parent_id: &str,
        child_id: &str,
        metadata: Option<&str>,
    ) -> Result<()> {
        let metadata = metadata.unwrap_or("{}");
        self.conn
            .execute(
                "INSERT INTO sessions (id, created_at, metadata, parent_session_id) VALUES (?1, ?2, ?3, ?4)",
                params![child_id, utc_timestamp(), metadata, parent_id],
            )
            .context("failed to create child session")?;
        Ok(())
    }

    /// Insert a child session and queue its initial task atomically.
    pub fn create_child_session_with_task(
        &mut self,
        parent_id: &str,
        child_id: &str,
        metadata: Option<&str>,
        task: &str,
        source: &str,
    ) -> Result<()> {
        let metadata = metadata.unwrap_or("{}");
        self.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO sessions (id, created_at, metadata, parent_session_id) VALUES (?1, ?2, ?3, ?4)",
                params![child_id, utc_timestamp(), metadata, parent_id],
            )
            .context("failed to create child session")?;
            tx.execute(
                "INSERT INTO messages (session_id, role, content, source, status, created_at) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                params![child_id, "user", task, source, utc_timestamp()],
            )
            .context("failed to enqueue child task")?;
            Ok(())
        })
    }

    pub fn list_sessions(&self) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM sessions ORDER BY created_at ASC, id ASC")
            .context("failed to prepare list_sessions query")?;
        let sessions = statement
            .query_map([], |row| row.get::<_, String>(0))
            .context("failed to query sessions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect sessions")?;

        Ok(sessions)
    }

    /// Return the parent session id for a child session, if one exists.
    pub fn get_parent_session(&self, child_id: &str) -> Result<Option<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT parent_session_id FROM sessions WHERE id = ?1")
            .context("failed to prepare get_parent_session query")?;
        match statement.query_row(params![child_id], |row| row.get::<_, Option<String>>(0)) {
            Ok(parent_id) => Ok(parent_id),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("failed to read parent session"),
        }
    }

    /// Return the stored metadata for a session, if the session exists.
    pub fn get_session_metadata(&self, session_id: &str) -> Result<Option<String>> {
        let mut statement = self
            .conn
            .prepare("SELECT metadata FROM sessions WHERE id = ?1")
            .context("failed to prepare get_session_metadata query")?;
        match statement.query_row(params![session_id], |row| row.get::<_, Option<String>>(0)) {
            Ok(metadata) => Ok(metadata),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("failed to read session metadata"),
        }
    }

    /// Return child session ids ordered by creation time.
    pub fn list_child_sessions(&self, parent_id: &str) -> Result<Vec<String>> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id FROM sessions WHERE parent_session_id = ?1 ORDER BY created_at ASC, id ASC",
            )
            .context("failed to prepare list_child_sessions query")?;
        let sessions = statement
            .query_map(params![parent_id], |row| row.get::<_, String>(0))
            .context("failed to query child sessions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect child sessions")?;

        Ok(sessions)
    }

    /// Run a SQLite transaction and commit only if the closure succeeds.
    pub fn with_transaction<T, F>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T>,
    {
        let tx = self
            .conn
            .transaction()
            .context("failed to start sqlite transaction")?;
        let result = f(&tx)?;
        tx.commit().context("failed to commit sqlite transaction")?;
        Ok(result)
    }

    pub fn enqueue_message(
        &mut self,
        session_id: &str,
        role: &str,
        content: &str,
        source: &str,
    ) -> Result<i64> {
        self.conn
            .execute(
                "INSERT INTO messages (session_id, role, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session_id, role, content, source, utc_timestamp()],
            )
            .context("failed to enqueue message")?;
        Ok(self.conn.last_insert_rowid())
    }

    pub(crate) fn enqueue_message_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        session_id: &str,
        role: &str,
        content: &str,
        source: &str,
    ) -> Result<()> {
        tx.execute(
            "INSERT INTO messages (session_id, role, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, role, content, source, utc_timestamp()],
        )
        .context("failed to enqueue message")?;
        Ok(())
    }

    pub fn dequeue_next_message(&mut self, session_id: &str) -> Result<Option<QueuedMessage>> {
        let claimed_at = unix_timestamp();
        let mut statement = self
            .conn
            .prepare(
                "UPDATE messages
                 SET status = 'processing', claimed_at = ?2
                 WHERE id = (
                     SELECT id
                     FROM messages
                     WHERE session_id = ?1 AND status = 'pending'
                     ORDER BY created_at ASC, id ASC
                     LIMIT 1
                 )
                 AND status = 'pending'
                 RETURNING id, session_id, role, content, source, status, created_at",
            )
            .context("failed to prepare dequeue query")?;

        match statement.query_row(params![session_id, claimed_at], |row| {
            Ok(QueuedMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                source: row.get(4)?,
                status: row.get(5)?,
                created_at: row.get(6)?,
            })
        }) {
            Ok(message) => Ok(Some(message)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("failed to dequeue message"),
        }
    }

    pub fn mark_processed(&mut self, message_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE messages SET status = 'processed' WHERE id = ?1",
                params![message_id],
            )
            .context("failed to mark message processed")?;
        Ok(())
    }

    pub fn mark_failed(&mut self, message_id: i64) -> Result<()> {
        self.conn
            .execute(
                "UPDATE messages SET status = 'failed' WHERE id = ?1",
                params![message_id],
            )
            .context("failed to mark message failed")?;
        Ok(())
    }

    /// Recover messages stuck in 'processing' state (e.g., after a crash).
    /// Resets them to 'pending' so they can be retried once they are stale.
    pub fn recover_stale_messages(&mut self, stale_after_secs: u64) -> Result<u64> {
        let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
        let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
        let count = self
            .conn
            .execute(
                "UPDATE messages
                 SET status = 'pending', claimed_at = NULL
                 WHERE status = 'processing'
                   AND (claimed_at IS NULL OR claimed_at <= ?1)",
                params![stale_before],
            )
            .context("failed to recover stale messages")?;
        Ok(count as u64)
    }

    pub fn create_plan_run(
        &mut self,
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
        self.conn
            .execute(
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

    pub fn get_plan_run(&self, id: &str) -> Result<Option<PlanRun>> {
        let mut statement = self
            .conn
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

    pub fn update_plan_run_status(
        &mut self,
        id: &str,
        status: &str,
        updated_fields: PlanRunUpdateFields,
    ) -> Result<()> {
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
        let mut bindings: Vec<Box<dyn ToSql>> =
            vec![Box::new(status.to_string()), Box::new(updated_at)];
        let mut next_index = 3usize;

        if let Some(revision) = updated_fields.revision {
            sql.push_str(&format!(", revision = ?{next_index}"));
            bindings.push(Box::new(revision));
            next_index += 1;
        }

        if let Some(current_step_index) = updated_fields.current_step_index {
            sql.push_str(&format!(", current_step_index = ?{next_index}"));
            bindings.push(Box::new(current_step_index));
            next_index += 1;
        }

        if let Some(definition_json) = updated_fields.definition_json {
            if definition_json.is_empty() {
                return Err(anyhow::anyhow!(
                    "invalid plan run definition json: definition_json must not be empty"
                ));
            }
            sql.push_str(&format!(", definition_json = ?{next_index}"));
            bindings.push(Box::new(definition_json));
            next_index += 1;
        }

        match updated_fields.active_child_session_id {
            NullableUpdate::Unchanged => {}
            NullableUpdate::Null => {
                sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
                bindings.push(Box::new(Option::<String>::None));
                next_index += 1;
            }
            NullableUpdate::Value(value) => {
                sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
                bindings.push(Box::new(Some(value)));
                next_index += 1;
            }
        }

        match updated_fields.last_failure_json {
            NullableUpdate::Unchanged => {}
            NullableUpdate::Null => {
                sql.push_str(&format!(", last_failure_json = ?{next_index}"));
                bindings.push(Box::new(Option::<String>::None));
                next_index += 1;
            }
            NullableUpdate::Value(value) => {
                sql.push_str(&format!(", last_failure_json = ?{next_index}"));
                bindings.push(Box::new(Some(value)));
                next_index += 1;
            }
        }

        sql.push_str(&format!(" WHERE id = ?{next_index}"));
        bindings.push(Box::new(id.to_string()));

        let changed = self
            .conn
            .execute(
                &sql,
                rusqlite::params_from_iter(bindings.iter().map(|value| value.as_ref())),
            )
            .context("failed to update plan run status")?;
        if changed == 0 {
            return Err(anyhow::anyhow!(
                "failed to update plan run status: plan run not found"
            ));
        }

        Ok(())
    }

    pub fn claim_next_pending_plan_run(
        &mut self,
        stale_after_secs: u64,
    ) -> Result<Option<PlanRun>> {
        self.claim_pending_plan_run(stale_after_secs)
    }

    pub fn claim_next_runnable_plan_run(
        &mut self,
        stale_after_secs: u64,
    ) -> Result<Option<PlanRun>> {
        let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
        let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
        let claimed_at = unix_timestamp();
        let updated_at = utc_timestamp();
        let mut statement = self
            .conn
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

    pub fn release_plan_run_claim(&mut self, id: &str) -> Result<()> {
        let changed = self
            .conn
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
            let current_status: Option<String> = self
                .conn
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

    pub fn list_plan_runs_by_session(&self, owner_session_id: &str) -> Result<Vec<PlanRun>> {
        let mut statement = self
            .conn
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

    pub fn list_recent_plan_runs(&self, limit: usize) -> Result<Vec<PlanRun>> {
        let mut statement = self
            .conn
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

    pub fn list_recent_active_plan_runs(&self, limit: usize) -> Result<Vec<PlanRun>> {
        let mut statement = self
            .conn
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

    pub fn recover_stale_plan_runs(&mut self, stale_after_secs: u64) -> Result<u64> {
        let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
        let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
        self.with_transaction(|tx| {
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

    pub fn resume_waiting_plan_run(&mut self, id: &str) -> Result<bool> {
        let changed = self
            .conn
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

    pub fn cancel_plan_run(&mut self, id: &str) -> Result<bool> {
        let changed = self
            .conn
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

    pub fn update_plan_run_status_preserving_failed(
        &mut self,
        id: &str,
        status: &str,
        updated_fields: PlanRunUpdateFields,
    ) -> Result<bool> {
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
        let mut bindings: Vec<Box<dyn ToSql>> =
            vec![Box::new(status.to_string()), Box::new(updated_at)];
        let mut next_index = 3usize;

        if let Some(revision) = updated_fields.revision {
            sql.push_str(&format!(", revision = ?{next_index}"));
            bindings.push(Box::new(revision));
            next_index += 1;
        }

        if let Some(current_step_index) = updated_fields.current_step_index {
            sql.push_str(&format!(", current_step_index = ?{next_index}"));
            bindings.push(Box::new(current_step_index));
            next_index += 1;
        }

        if let Some(definition_json) = updated_fields.definition_json {
            if definition_json.is_empty() {
                return Err(anyhow::anyhow!(
                    "invalid plan run definition json: definition_json must not be empty"
                ));
            }
            sql.push_str(&format!(", definition_json = ?{next_index}"));
            bindings.push(Box::new(definition_json));
            next_index += 1;
        }

        match updated_fields.active_child_session_id {
            NullableUpdate::Unchanged => {}
            NullableUpdate::Null => {
                sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
                bindings.push(Box::new(Option::<String>::None));
                next_index += 1;
            }
            NullableUpdate::Value(value) => {
                sql.push_str(&format!(", active_child_session_id = ?{next_index}"));
                bindings.push(Box::new(Some(value)));
                next_index += 1;
            }
        }

        match updated_fields.last_failure_json {
            NullableUpdate::Unchanged => {}
            NullableUpdate::Null => {
                sql.push_str(&format!(", last_failure_json = ?{next_index}"));
                bindings.push(Box::new(Option::<String>::None));
                next_index += 1;
            }
            NullableUpdate::Value(value) => {
                sql.push_str(&format!(", last_failure_json = ?{next_index}"));
                bindings.push(Box::new(Some(value)));
                next_index += 1;
            }
        }

        sql.push_str(&format!(" WHERE id = ?{next_index} AND status != 'failed'"));
        bindings.push(Box::new(id.to_string()));

        let changed = self
            .conn
            .execute(
                &sql,
                rusqlite::params_from_iter(bindings.iter().map(|value| value.as_ref())),
            )
            .context("failed to update plan run status")?;
        Ok(changed > 0)
    }

    pub fn list_stale_running_plan_runs(&self, stale_after_secs: u64) -> Result<Vec<PlanRun>> {
        let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
        let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
        let mut statement = self
            .conn
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

    pub fn next_step_attempt_index_for_run(&self, plan_run: &PlanRun) -> Result<i64> {
        let attempts = self.get_step_attempts(&plan_run.id, plan_run.current_step_index)?;
        Ok(attempts
            .into_iter()
            .filter(|attempt| attempt.revision == plan_run.revision)
            .map(|attempt| attempt.attempt)
            .max()
            .map(|attempt| attempt + 1)
            .unwrap_or(0))
    }

    pub fn max_step_attempt_index_for_run(&self, plan_run_id: &str) -> Result<i64> {
        let max_attempt: Option<i64> = self
            .conn
            .query_row(
                "SELECT MAX(attempt) FROM plan_step_attempts WHERE plan_run_id = ?1",
                params![plan_run_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .context("failed to query maximum plan step attempt")?;
        Ok(max_attempt.unwrap_or(-1))
    }

    pub fn crash_running_step_attempts_for_run(
        &mut self,
        plan_run_id: &str,
    ) -> Result<Vec<StepAttempt>> {
        self.with_transaction(|tx| {
            Self::crash_running_step_attempts_for_run_in_transaction(tx, plan_run_id)
        })
    }

    pub(crate) fn crash_running_step_attempts_for_run_in_transaction(
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

    pub fn record_step_attempt(&mut self, record: StepAttemptRecord) -> Result<i64> {
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
        self.conn
            .execute(
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
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_step_attempt_status(
        &mut self,
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
        let changed = self
            .conn
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

    pub fn update_step_attempt_child_session(
        &mut self,
        attempt_id: i64,
        child_session_id: Option<&str>,
    ) -> Result<()> {
        let changed = self
            .conn
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

    pub fn finalize_step_attempt(
        &mut self,
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
        let changed = self
            .conn
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

    pub fn finalize_stale_step_attempts(
        &mut self,
        plan_run_id: &str,
        revision: i64,
    ) -> Result<u64> {
        let attempts = {
            let mut statement = self
                .conn
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
            self.finalize_step_attempt(
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

    fn claim_pending_plan_run(&mut self, stale_after_secs: u64) -> Result<Option<PlanRun>> {
        let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
        let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
        let claimed_at = unix_timestamp();
        let updated_at = utc_timestamp();
        let mut statement = self
            .conn
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

    pub fn get_step_attempts(
        &self,
        plan_run_id: &str,
        step_index: i64,
    ) -> Result<Vec<StepAttempt>> {
        let mut statement = self
            .conn
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

    pub fn create_subscription(
        &mut self,
        topic: &str,
        path: &str,
        filter: Option<&str>,
    ) -> Result<i64> {
        let timestamp = format_system_time(std::time::SystemTime::now());
        self.conn
            .execute(
                "INSERT INTO subscriptions (topic, path, filter, activated_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![topic, path, filter, timestamp, timestamp],
            )
            .context("failed to insert subscription")?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn delete_subscription(&mut self, topic: &str, path: &str) -> Result<usize> {
        let count = self
            .conn
            .execute(
                "DELETE FROM subscriptions WHERE topic = ?1 AND path = ?2",
                params![topic, path],
            )
            .context("failed to delete subscription")?;
        Ok(count)
    }

    pub fn list_subscriptions(&self, topic: Option<&str>) -> Result<Vec<SubscriptionRow>> {
        let mut statement = if topic.is_some() {
            self.conn
                .prepare(
                    "SELECT id, topic, path, filter, activated_at, updated_at
                     FROM subscriptions
                     WHERE topic = ?1
                     ORDER BY CASE WHEN updated_at > activated_at THEN updated_at ELSE activated_at END ASC, id ASC",
                )
                .context("failed to prepare list_subscriptions query")?
        } else {
            self.conn
                .prepare(
                    "SELECT id, topic, path, filter, activated_at, updated_at
                     FROM subscriptions
                     ORDER BY CASE WHEN updated_at > activated_at THEN updated_at ELSE activated_at END ASC, id ASC",
                )
                .context("failed to prepare list_subscriptions query")?
        };

        let rows = if let Some(topic) = topic {
            statement
                .query_map(params![topic], |row| {
                    Ok(SubscriptionRow {
                        id: row.get(0)?,
                        topic: row.get(1)?,
                        path: row.get(2)?,
                        filter: row.get(3)?,
                        activated_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                })
                .context("failed to query subscriptions")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to collect subscriptions")?
        } else {
            statement
                .query_map([], |row| {
                    Ok(SubscriptionRow {
                        id: row.get(0)?,
                        topic: row.get(1)?,
                        path: row.get(2)?,
                        filter: row.get(3)?,
                        activated_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                })
                .context("failed to query subscriptions")?
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to collect subscriptions")?
        };

        Ok(rows)
    }

    pub fn refresh_subscription_timestamps(&mut self) -> Result<u64> {
        #[cfg(test)]
        {
            self.refresh_subscription_timestamps_with(|path| {
                std::fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
            })
        }

        #[cfg(not(test))]
        {
            let rows = self.list_subscriptions(None)?;
            let mut refreshed = 0u64;

            for row in rows {
                let path = std::path::Path::new(&row.path);
                let Ok(metadata) = std::fs::metadata(path) else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                let updated_at = format_system_time(modified);
                if updated_at > row.updated_at {
                    self.conn
                        .execute(
                            "UPDATE subscriptions SET updated_at = ?1 WHERE id = ?2",
                            params![updated_at, row.id],
                        )
                        .context("failed to update subscription timestamp")?;
                    refreshed += 1;
                }
            }

            Ok(refreshed)
        }
    }

    #[cfg(test)]
    pub(crate) fn refresh_subscription_timestamps_with<F>(
        &mut self,
        mut modified_for: F,
    ) -> Result<u64>
    where
        F: FnMut(&Path) -> Option<SystemTime>,
    {
        let rows = self.list_subscriptions(None)?;
        let mut refreshed = 0u64;

        for row in rows {
            let path = std::path::Path::new(&row.path);
            let Some(modified) = modified_for(path) else {
                continue;
            };
            let updated_at = format_system_time(modified);
            if updated_at > row.updated_at {
                self.conn
                    .execute(
                        "UPDATE subscriptions SET updated_at = ?1 WHERE id = ?2",
                        params![updated_at, row.id],
                    )
                    .context("failed to update subscription timestamp")?;
                refreshed += 1;
            }
        }

        Ok(refreshed)
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn ensure_messages_claimed_at_column(conn: &Connection) -> Result<()> {
    if has_column(conn, "messages", "claimed_at")? {
        return Ok(());
    }

    if let Err(error) = conn.execute("ALTER TABLE messages ADD COLUMN claimed_at INTEGER", [])
        && !has_column(conn, "messages", "claimed_at")?
    {
        return Err(error).context("failed to migrate messages.claimed_at column");
    }

    Ok(())
}

fn ensure_sessions_parent_session_id_column(conn: &Connection) -> Result<()> {
    if has_column(conn, "sessions", "parent_session_id")? {
        return Ok(());
    }

    if let Err(error) = conn.execute(
        "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT REFERENCES sessions(id)",
        [],
    ) && !has_column(conn, "sessions", "parent_session_id")?
    {
        return Err(error).context("failed to migrate sessions.parent_session_id column");
    }

    Ok(())
}

fn ensure_plan_runs_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS plan_runs (
            id TEXT PRIMARY KEY,
            owner_session_id TEXT NOT NULL,
            topic TEXT,
            trigger_source TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            revision INTEGER NOT NULL DEFAULT 1,
            current_step_index INTEGER NOT NULL DEFAULT 0,
            active_child_session_id TEXT,
            definition_json TEXT NOT NULL,
            last_failure_json TEXT,
            claimed_at INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY(owner_session_id) REFERENCES sessions(id)
        );
        CREATE TABLE IF NOT EXISTS plan_step_attempts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            plan_run_id TEXT NOT NULL,
            revision INTEGER NOT NULL,
            step_index INTEGER NOT NULL,
            step_id TEXT NOT NULL,
            attempt INTEGER NOT NULL,
            status TEXT NOT NULL,
            child_session_id TEXT,
            summary_json TEXT NOT NULL,
            checks_json TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            FOREIGN KEY(plan_run_id) REFERENCES plan_runs(id) ON DELETE CASCADE
        );
        "#,
    )
    .context("failed to initialize plan engine tables")?;
    ensure_plan_run_column(conn, "owner_session_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_run_column(conn, "topic", "TEXT")?;
    ensure_plan_run_column(conn, "trigger_source", "TEXT")?;
    ensure_plan_run_column(conn, "status", "TEXT NOT NULL DEFAULT 'pending'")?;
    ensure_plan_run_column(conn, "revision", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_plan_run_column(conn, "current_step_index", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_plan_run_column(conn, "active_child_session_id", "TEXT")?;
    ensure_plan_run_column(conn, "definition_json", "TEXT NOT NULL DEFAULT '{}'")?;
    ensure_plan_run_column(conn, "last_failure_json", "TEXT")?;
    ensure_plan_run_column(conn, "claimed_at", "INTEGER")?;
    ensure_plan_run_column(conn, "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_run_column(conn, "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_step_attempt_column(conn, "plan_run_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_step_attempt_column(conn, "revision", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_plan_step_attempt_column(conn, "step_index", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_plan_step_attempt_column(conn, "step_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_step_attempt_column(conn, "attempt", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_plan_step_attempt_column(conn, "status", "TEXT NOT NULL DEFAULT 'running'")?;
    ensure_plan_step_attempt_column(conn, "child_session_id", "TEXT")?;
    ensure_plan_step_attempt_column(conn, "summary_json", "TEXT NOT NULL DEFAULT '{}'")?;
    ensure_plan_step_attempt_column(conn, "checks_json", "TEXT NOT NULL DEFAULT '[]'")?;
    ensure_plan_step_attempt_column(conn, "started_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_plan_step_attempt_column(conn, "finished_at", "TEXT")?;
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_plan_runs_owner_session_created_at
            ON plan_runs(owner_session_id, created_at, id);
        CREATE INDEX IF NOT EXISTS idx_plan_runs_status_claimed_at
            ON plan_runs(status, claimed_at, id);
        CREATE INDEX IF NOT EXISTS idx_plan_step_attempts_run_step_attempt
            ON plan_step_attempts(plan_run_id, step_index, attempt, id);
        CREATE TRIGGER IF NOT EXISTS trg_plan_runs_owner_session_fk
            BEFORE INSERT ON plan_runs
            WHEN NOT EXISTS (
                SELECT 1 FROM sessions WHERE id = NEW.owner_session_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_plan_runs_owner_session_update_fk
            BEFORE UPDATE OF owner_session_id ON plan_runs
            WHEN NOT EXISTS (
                SELECT 1 FROM sessions WHERE id = NEW.owner_session_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_plan_runs_restrict_update_id
            BEFORE UPDATE OF id ON plan_runs
            WHEN EXISTS (
                SELECT 1 FROM plan_step_attempts WHERE plan_run_id = OLD.id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_plan_step_attempts_plan_run_fk
            BEFORE INSERT ON plan_step_attempts
            WHEN NOT EXISTS (
                SELECT 1 FROM plan_runs WHERE id = NEW.plan_run_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_plan_step_attempts_plan_run_update_fk
            BEFORE UPDATE OF plan_run_id ON plan_step_attempts
            WHEN NOT EXISTS (
                SELECT 1 FROM plan_runs WHERE id = NEW.plan_run_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_sessions_restrict_delete_plan_runs
            BEFORE DELETE ON sessions
            WHEN EXISTS (
                SELECT 1 FROM plan_runs WHERE owner_session_id = OLD.id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_sessions_restrict_update_id
            BEFORE UPDATE OF id ON sessions
            WHEN NEW.id != OLD.id AND EXISTS (
                SELECT 1 FROM plan_runs WHERE owner_session_id = OLD.id
            )
            BEGIN
                SELECT RAISE(ABORT, 'foreign key constraint failed');
            END;
        CREATE TRIGGER IF NOT EXISTS trg_plan_runs_step_attempts_cascade
            AFTER DELETE ON plan_runs
            BEGIN
                DELETE FROM plan_step_attempts WHERE plan_run_id = OLD.id;
            END;
        "#,
    )
    .context("failed to initialize plan engine indexes")?;
    cleanup_legacy_plan_rows(conn)?;
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("failed to inspect sqlite schema for {table}"))?;
    let mut rows = statement
        .query([])
        .with_context(|| format!("failed to query sqlite schema for {table}"))?;

    while let Some(row) = rows
        .next()
        .with_context(|| format!("failed to read sqlite schema for {table}"))?
    {
        let name: String = row.get(1).context("failed to read sqlite column name")?;
        if name == column {
            return Ok(true);
        }
    }

    Ok(false)
}

fn ensure_plan_run_column(conn: &Connection, column: &str, declaration: &str) -> Result<()> {
    if !has_column(conn, "plan_runs", column)? {
        conn.execute(
            &format!("ALTER TABLE plan_runs ADD COLUMN {column} {declaration}"),
            [],
        )
        .with_context(|| format!("failed to add plan_runs.{column} column"))?;
    }
    Ok(())
}

fn ensure_plan_step_attempt_column(
    conn: &Connection,
    column: &str,
    declaration: &str,
) -> Result<()> {
    if !has_column(conn, "plan_step_attempts", column)? {
        conn.execute(
            &format!("ALTER TABLE plan_step_attempts ADD COLUMN {column} {declaration}"),
            [],
        )
        .with_context(|| format!("failed to add plan_step_attempts.{column} column"))?;
    }
    Ok(())
}

fn cleanup_legacy_plan_rows(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM plan_step_attempts
         WHERE plan_run_id NOT IN (SELECT id FROM plan_runs)
            OR revision <= 0
            OR step_index < 0
            OR attempt < 0
            OR step_id = ''
            OR started_at = ''
            OR status NOT IN ('running', 'passed', 'failed', 'crashed')
            OR (status = 'running' AND finished_at IS NOT NULL)
            OR (status IN ('passed', 'failed', 'crashed') AND (finished_at IS NULL OR finished_at = ''));
        DELETE FROM plan_runs
         WHERE owner_session_id NOT IN (SELECT id FROM sessions)
            OR revision <= 0
            OR current_step_index < 0
            OR created_at = ''
            OR updated_at = ''
            OR definition_json = ''
            OR status NOT IN ('pending', 'running', 'waiting_t2', 'completed', 'failed');
        DELETE FROM plan_step_attempts
         WHERE plan_run_id NOT IN (SELECT id FROM plan_runs)
            OR revision <= 0
            OR step_index < 0
            OR attempt < 0
            OR step_id = ''
            OR started_at = ''
            OR status NOT IN ('running', 'passed', 'failed', 'crashed')
            OR (status = 'running' AND finished_at IS NOT NULL)
            OR (status IN ('passed', 'failed', 'crashed') AND (finished_at IS NULL OR finished_at = ''));
        "#,
    )
    .context("failed to clean legacy plan rows")?;
    Ok(())
}

#[cfg(test)]
fn has_table(conn: &Connection, table: &str) -> Result<bool> {
    let mut statement = conn
        .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1")
        .context("failed to prepare sqlite table lookup")?;
    match statement.query_row(params![table], |row| row.get::<_, i64>(0)) {
        Ok(_) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error).context("failed to inspect sqlite table catalog"),
    }
}

fn plan_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanRun> {
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

fn step_attempt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StepAttempt> {
    Ok(StepAttempt {
        id: row.get(0)?,
        plan_run_id: row.get(1)?,
        revision: row.get(2)?,
        step_index: row.get(3)?,
        step_id: row.get(4)?,
        attempt: row.get(5)?,
        status: row.get(6)?,
        child_session_id: row.get(7)?,
        summary_json: row.get(8)?,
        checks_json: row.get(9)?,
        started_at: row.get(10)?,
        finished_at: row.get(11)?,
    })
}

fn validate_plan_run_status(status: &str) -> Result<()> {
    match status {
        "pending" | "running" | "waiting_t2" | "completed" | "failed" => Ok(()),
        other => Err(anyhow::anyhow!("invalid plan run status: {other}")),
    }
}

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

pub fn format_system_time(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = duration.as_secs() as i64;
    let micros = duration.subsec_micros();

    let mut days = seconds / 86_400;
    let mut rem = seconds % 86_400;

    let hour = rem / 3_600;
    rem %= 3_600;
    let minute = rem / 60;
    let second = rem % 60;

    days += 719_468;
    let era = if days >= 0 {
        days / 146_097
    } else {
        (days - 146_096) / 146_097
    };
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as i32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if month <= 2 { 1 } else { 0 };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        year, month, day, hour, minute, second, micros
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_db_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_store_test_{prefix}_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn has_index(conn: &Connection, index: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?1 LIMIT 1",
            [index],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    }

    #[test]
    fn store_new_creates_plan_tables() {
        let path = temp_db_path("plan_schema");
        let store = Store::new(&path).unwrap();

        assert!(has_table(&store.conn, "plan_runs").unwrap());
        assert!(has_table(&store.conn, "plan_step_attempts").unwrap());
        assert!(has_index(
            &store.conn,
            "idx_plan_runs_owner_session_created_at"
        ));
        assert!(has_index(&store.conn, "idx_plan_runs_status_claimed_at"));
        assert!(has_index(
            &store.conn,
            "idx_plan_step_attempts_run_step_attempt"
        ));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ensure_plan_runs_table_migrates_legacy_store() {
        let path = temp_db_path("plan_schema_migration");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE TABLE plan_runs (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE plan_step_attempts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                plan_run_id TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plan_runs (id, created_at, updated_at) VALUES (?1, ?2, ?3)",
            params![
                "legacy-plan",
                "2024-01-01T00:00:00Z",
                "2024-01-01T00:00:00Z"
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO plan_step_attempts (plan_run_id) VALUES (?1)",
            params!["legacy-plan"],
        )
        .unwrap();
        drop(conn);

        let mut store = Store::new(&path).unwrap();
        assert!(has_table(&store.conn, "plan_runs").unwrap());
        assert!(has_table(&store.conn, "plan_step_attempts").unwrap());
        assert!(has_column(&store.conn, "plan_runs", "owner_session_id").unwrap());
        assert!(has_column(&store.conn, "plan_runs", "definition_json").unwrap());
        assert!(has_column(&store.conn, "plan_runs", "claimed_at").unwrap());
        assert!(has_column(&store.conn, "plan_step_attempts", "revision").unwrap());
        assert!(has_column(&store.conn, "plan_step_attempts", "summary_json").unwrap());
        assert!(has_column(&store.conn, "plan_step_attempts", "finished_at").unwrap());
        assert!(has_index(
            &store.conn,
            "idx_plan_runs_owner_session_created_at"
        ));
        assert!(has_index(&store.conn, "idx_plan_runs_status_claimed_at"));
        assert!(has_index(
            &store.conn,
            "idx_plan_step_attempts_run_step_attempt"
        ));
        assert!(store.get_plan_run("legacy-plan").unwrap().is_none());
        assert!(
            store
                .get_step_attempts("legacy-plan", 0)
                .unwrap()
                .is_empty()
        );
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        assert!(
            store
                .conn
                .execute("DELETE FROM sessions WHERE id = ?1", params!["owner"])
                .is_err()
        );
        assert!(
            store
                .conn
                .execute(
                    "UPDATE plan_runs SET owner_session_id = ?1 WHERE id = ?2",
                    params!["missing", "plan-1"],
                )
                .is_err()
        );
        assert!(
            store
                .conn
                .execute(
                    "UPDATE plan_step_attempts SET plan_run_id = ?1 WHERE id = ?2",
                    params!["missing", attempt_id],
                )
                .is_err()
        );
        assert!(
            store
                .conn
                .execute(
                    "UPDATE plan_runs SET id = ?1 WHERE id = ?2",
                    params!["plan-2", "plan-1"],
                )
                .is_err()
        );
        store
            .conn
            .execute("DELETE FROM plan_runs WHERE id = ?1", params!["plan-1"])
            .unwrap();
        assert!(store.get_plan_run("plan-1").unwrap().is_none());
        assert!(store.get_step_attempts("plan-1", 0).unwrap().is_empty());
        ensure_plan_runs_table(&store.conn).unwrap();
        assert!(has_table(&store.conn, "plan_runs").unwrap());
        assert!(has_table(&store.conn, "plan_step_attempts").unwrap());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_new_cleans_invalid_step_attempt_status_finished_at_pairs() {
        let path = temp_db_path("plan_step_attempt_cleanup");
        {
            let mut store = Store::new(&path).unwrap();
            store.create_session("owner", None).unwrap();
            store
                .create_plan_run(
                    "plan-1",
                    "owner",
                    r#"{"kind":"plan"}"#,
                    Some("topic"),
                    Some("cli"),
                )
                .unwrap();
            store
                .record_step_attempt(StepAttemptRecord {
                    plan_run_id: "plan-1".to_string(),
                    revision: 1,
                    step_index: 0,
                    step_id: "step-valid".to_string(),
                    attempt: 0,
                    status: "running".to_string(),
                    child_session_id: None,
                    summary_json: "{}".to_string(),
                    checks_json: "[]".to_string(),
                })
                .unwrap();
            let valid_finished_at = utc_timestamp();
            store
                .conn
                .execute(
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
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10)",
                    params![
                        "plan-1",
                        1,
                        0,
                        "step-terminal",
                        1,
                        "passed",
                        "{}",
                        "[]",
                        valid_finished_at,
                        valid_finished_at
                    ],
                )
                .unwrap();
            store
                .conn
                .execute(
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
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, NULL)",
                    params![
                        "plan-1",
                        1,
                        0,
                        "step-terminal-missing-finished",
                        2,
                        "failed",
                        "{}",
                        "[]",
                        valid_finished_at,
                    ],
                )
                .unwrap();
            store
                .conn
                .execute(
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
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10)",
                    params![
                        "plan-1",
                        1,
                        0,
                        "step-running-with-finished",
                        3,
                        "running",
                        "{}",
                        "[]",
                        valid_finished_at,
                        "",
                    ],
                )
                .unwrap();
        }

        let store = Store::new(&path).unwrap();
        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].step_id, "step-valid");
        assert_eq!(attempts[0].status, "running");
        assert_eq!(attempts[0].finished_at, None);
        assert_eq!(attempts[1].step_id, "step-terminal");
        assert_eq!(attempts[1].status, "passed");
        assert_eq!(
            attempts[1].finished_at.as_deref(),
            Some(attempts[1].started_at.as_str())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_and_get_plan_run_round_trips() {
        let path = temp_db_path("plan_run_round_trip");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();

        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan","steps":[]}"#,
                Some("topic-a"),
                Some("cron"),
            )
            .unwrap();

        let plan_run = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(plan_run.id, "plan-1");
        assert_eq!(plan_run.owner_session_id, "owner");
        assert_eq!(plan_run.topic.as_deref(), Some("topic-a"));
        assert_eq!(plan_run.trigger_source.as_deref(), Some("cron"));
        assert_eq!(plan_run.status, "pending");
        assert_eq!(plan_run.revision, 1);
        assert_eq!(plan_run.current_step_index, 0);
        assert_eq!(plan_run.active_child_session_id, None);
        assert_eq!(plan_run.definition_json, r#"{"kind":"plan","steps":[]}"#);
        assert_eq!(plan_run.last_failure_json, None);
        assert_eq!(plan_run.claimed_at, None);
        assert!(!plan_run.created_at.is_empty());
        assert_eq!(plan_run.created_at, plan_run.updated_at);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_plan_run_rejects_missing_owner_session() {
        let path = temp_db_path("plan_run_missing_owner");
        let mut store = Store::new(&path).unwrap();

        let err = store
            .create_plan_run("plan-1", "missing", "{}", None, None)
            .expect_err("missing owner session should fail");
        assert!(err.to_string().contains("failed to create plan run"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_plan_run_returns_none_for_missing_id() {
        let path = temp_db_path("plan_run_missing");
        let store = Store::new(&path).unwrap();

        assert_eq!(store.get_plan_run("missing").unwrap(), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn list_plan_runs_by_session_returns_only_that_owner_in_creation_order() {
        let path = temp_db_path("plan_run_list");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner-a", None).unwrap();
        store.create_session("owner-b", None).unwrap();

        store
            .create_plan_run("plan-a1", "owner-a", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-b1", "owner-b", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-a2", "owner-a", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let plan_runs = store.list_plan_runs_by_session("owner-a").unwrap();
        let ids = plan_runs
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["plan-a1", "plan-a2"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_plan_run_status_updates_only_requested_fields() {
        let path = temp_db_path("plan_run_update");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan"}"#,
                Some("topic"),
                Some("cli"),
            )
            .unwrap();

        let original = store.get_plan_run("plan-1").unwrap().unwrap();
        std::thread::sleep(Duration::from_secs(1));
        store
            .update_plan_run_status(
                "plan-1",
                "waiting_t2",
                PlanRunUpdateFields {
                    revision: Some(2),
                    current_step_index: Some(1),
                    definition_json: Some(r#"{"kind":"plan","patched":true}"#.to_string()),
                    active_child_session_id: NullableUpdate::Value("child-1".to_string()),
                    last_failure_json: NullableUpdate::Value(r#"{"reason":"boom"}"#.to_string()),
                },
            )
            .unwrap();

        let updated = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(updated.revision, 2);
        assert_eq!(updated.current_step_index, 1);
        assert_eq!(updated.definition_json, r#"{"kind":"plan","patched":true}"#);
        assert_eq!(updated.active_child_session_id.as_deref(), Some("child-1"));
        assert_eq!(
            updated.last_failure_json.as_deref(),
            Some(r#"{"reason":"boom"}"#)
        );
        assert_eq!(updated.topic, original.topic);
        assert_eq!(updated.trigger_source, original.trigger_source);
        assert_eq!(updated.created_at, original.created_at);
        assert_ne!(updated.updated_at, original.updated_at);

        std::thread::sleep(Duration::from_secs(1));
        store
            .update_plan_run_status(
                "plan-1",
                "completed",
                PlanRunUpdateFields {
                    active_child_session_id: NullableUpdate::Null,
                    last_failure_json: NullableUpdate::Null,
                    ..Default::default()
                },
            )
            .unwrap();

        let cleared = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(cleared.active_child_session_id, None);
        assert_eq!(cleared.last_failure_json, None);
        assert_ne!(cleared.updated_at, updated.updated_at);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn invalid_plan_run_status_rejects_before_sql() {
        let path = temp_db_path("plan_run_invalid_status");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", None, None)
            .unwrap();

        let err = store
            .update_plan_run_status("plan-1", "bogus", PlanRunUpdateFields::default())
            .expect_err("invalid status should fail");
        assert!(err.to_string().contains("invalid plan run status"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_plan_run_status_rejects_negative_numeric_fields() {
        let path = temp_db_path("plan_run_negative_numbers");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", None, None)
            .unwrap();

        let err = store
            .update_plan_run_status(
                "plan-1",
                "waiting_t2",
                PlanRunUpdateFields {
                    revision: Some(0),
                    current_step_index: Some(0),
                    ..Default::default()
                },
            )
            .expect_err("negative numeric fields should fail");
        assert!(err.to_string().contains("invalid plan run revision"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_plan_run_rejects_empty_definition_json() {
        let path = temp_db_path("plan_run_empty_definition");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();

        let err = store
            .create_plan_run("plan-1", "owner", "", None, None)
            .expect_err("empty definition json should fail");
        assert!(err.to_string().contains("invalid plan run definition json"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_plan_run_status_rejects_empty_definition_json() {
        let path = temp_db_path("plan_run_empty_definition_update");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", r#"{"kind":"plan"}"#, None, None)
            .unwrap();

        let err = store
            .update_plan_run_status(
                "plan-1",
                "waiting_t2",
                PlanRunUpdateFields {
                    definition_json: Some(String::new()),
                    ..Default::default()
                },
            )
            .expect_err("empty definition json update should fail");
        assert!(err.to_string().contains("invalid plan run definition json"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_pending_plan_run_marks_row_running_and_claimed() {
        let path = temp_db_path("plan_run_claim");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let claimed = store
            .claim_next_pending_plan_run(300)
            .unwrap()
            .expect("pending plan run should be claimed");
        assert_eq!(claimed.id, "plan-1");
        assert_eq!(claimed.status, "running");
        assert!(claimed.claimed_at.is_some());
        assert_eq!(
            store.get_plan_run("plan-1").unwrap().unwrap().status,
            "running"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_pending_plan_run_is_atomic_across_workers() {
        use std::sync::{Arc, Barrier};

        let path = temp_db_path("plan_run_claim_atomic");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        let plan_run_id = "plan-1".to_string();
        store
            .create_plan_run(&plan_run_id, "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        drop(store);

        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = barrier.clone();
            let plan_run_id = plan_run_id.clone();
            workers.push(std::thread::spawn(move || {
                let mut store = Store::new(&path).unwrap();
                barrier.wait();
                store
                    .claim_next_pending_plan_run(300)
                    .unwrap()
                    .map(|plan_run| plan_run.id == plan_run_id)
                    .unwrap_or(false)
            }));
        }

        let claims = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(claims, 1);

        let store = Store::new(&path).unwrap();
        assert_eq!(
            store.get_plan_run("plan-1").unwrap().unwrap().status,
            "running"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_pending_plan_run_skips_fresh_running_rows() {
        let path = temp_db_path("plan_run_skip_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .update_plan_run_status("plan-1", "running", PlanRunUpdateFields::default())
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp(), "plan-1"],
            )
            .unwrap();

        assert!(store.claim_next_pending_plan_run(300).unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_pending_plan_run_returns_none_when_only_non_pending_rows_exist() {
        let path = temp_db_path("plan_run_claim_none");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-waiting", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-completed", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-failed", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .update_plan_run_status("plan-waiting", "waiting_t2", PlanRunUpdateFields::default())
            .unwrap();
        store
            .update_plan_run_status(
                "plan-completed",
                "completed",
                PlanRunUpdateFields::default(),
            )
            .unwrap();
        store
            .update_plan_run_status("plan-failed", "failed", PlanRunUpdateFields::default())
            .unwrap();

        assert!(store.claim_next_pending_plan_run(300).unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_runnable_plan_run_skips_running_row_after_recovery() {
        let path = temp_db_path("plan_run_claim_runnable");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store.claim_next_pending_plan_run(300).unwrap().unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-1"],
            )
            .unwrap();

        store.recover_stale_plan_runs(300).unwrap();
        let recovered_run = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(recovered_run.status, "running");
        assert!(recovered_run.claimed_at.is_some());

        assert!(store.claim_next_runnable_plan_run(300).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_runnable_plan_run_does_not_claim_stale_running_row() {
        let path = temp_db_path("plan_run_claim_runnable_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-running", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .update_plan_run_status("plan-running", "running", PlanRunUpdateFields::default())
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-running"],
            )
            .unwrap();

        assert!(store.claim_next_runnable_plan_run(300).unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_runnable_plan_run_skips_running_rows() {
        let path = temp_db_path("plan_run_claim_runnable_priority");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-pending", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let claimed = store.claim_next_runnable_plan_run(300).unwrap().unwrap();
        assert_eq!(claimed.id, "plan-pending");
        assert_eq!(claimed.status, "running");
        assert!(claimed.claimed_at.is_some());

        let second = store.claim_next_runnable_plan_run(300).unwrap();
        assert!(second.is_none(), "fresh running rows are not claimable");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn claim_next_runnable_plan_run_prioritizes_pending_rows_over_running_rows() {
        let path = temp_db_path("plan_run_claim_runnable_order");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-pending", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-running", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .update_plan_run_status("plan-running", "running", PlanRunUpdateFields::default())
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id IN (?2, ?3)",
                params![unix_timestamp() - 301, "plan-pending", "plan-running"],
            )
            .unwrap();

        let claimed = store.claim_next_runnable_plan_run(300).unwrap().unwrap();
        assert_eq!(claimed.id, "plan-pending");
        assert_eq!(claimed.status, "running");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn release_plan_run_claim_clears_claim_without_losing_state() {
        let path = temp_db_path("plan_run_release");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store.claim_next_pending_plan_run(300).unwrap().unwrap();
        store
            .update_plan_run_status("plan-1", "waiting_t2", PlanRunUpdateFields::default())
            .unwrap();
        let before_release = store.get_plan_run("plan-1").unwrap().unwrap();
        std::thread::sleep(Duration::from_secs(1));

        store.release_plan_run_claim("plan-1").unwrap();

        let after_release = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(after_release.claimed_at, None);
        assert_eq!(after_release.status, "waiting_t2");
        assert_eq!(after_release.topic, before_release.topic);
        assert_eq!(after_release.trigger_source, before_release.trigger_source);
        assert_ne!(after_release.updated_at, before_release.updated_at);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn release_plan_run_claim_rejects_running_rows() {
        let path = temp_db_path("plan_run_release_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store.claim_next_pending_plan_run(300).unwrap().unwrap();

        let err = store
            .release_plan_run_claim("plan-1")
            .expect_err("running plan run should not be releasable");
        assert!(err.to_string().contains("still running"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_plan_runs_respects_age_threshold() {
        let path = temp_db_path("plan_run_recover_threshold");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-pending", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .create_plan_run("plan-running", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-pending"],
            )
            .unwrap();
        store
            .update_plan_run_status("plan-running", "running", PlanRunUpdateFields::default())
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-running"],
            )
            .unwrap();

        let recovered = store.recover_stale_plan_runs(300).unwrap();
        assert_eq!(recovered, 1);

        let pending = store.get_plan_run("plan-pending").unwrap().unwrap();
        let running = store.get_plan_run("plan-running").unwrap().unwrap();
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.claimed_at, None);
        assert_eq!(running.status, "running");
        assert!(running.claimed_at.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_plan_runs_keeps_stale_running_rows_unclaimable() {
        let path = temp_db_path("plan_run_recover_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store.claim_next_pending_plan_run(300).unwrap().unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-1"],
            )
            .unwrap();

        let recovered = store.recover_stale_plan_runs(300).unwrap();
        assert_eq!(recovered, 0);

        let recovered_run = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(recovered_run.status, "running");
        assert!(recovered_run.claimed_at.is_some());
        assert!(store.claim_next_runnable_plan_run(300).unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn crash_running_step_attempts_for_run_marks_rows_crashed_and_returns_original_metadata() {
        let path = temp_db_path("plan_run_crash_attempts");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan","steps":[]}"#,
                None,
                None,
            )
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: Some("child-a".to_string()),
                summary_json: r#"{"kind":"plan_step_summary","attempt":0}"#.to_string(),
                checks_json: r#"[{"check_id":"check-a"}]"#.to_string(),
            })
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 1,
                status: "running".to_string(),
                child_session_id: Some("child-b".to_string()),
                summary_json: r#"{"kind":"plan_step_summary","attempt":1}"#.to_string(),
                checks_json: r#"[{"check_id":"check-b"}]"#.to_string(),
            })
            .unwrap();

        let crashed = store.crash_running_step_attempts_for_run("plan-1").unwrap();
        assert_eq!(crashed.len(), 2);
        assert_eq!(crashed[0].status, "running");
        assert_eq!(
            crashed[0].summary_json,
            r#"{"kind":"plan_step_summary","attempt":1}"#
        );
        assert_eq!(
            crashed[1].summary_json,
            r#"{"kind":"plan_step_summary","attempt":0}"#
        );

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().all(|attempt| attempt.status == "crashed"));
        assert!(attempts.iter().all(|attempt| attempt.finished_at.is_some()));
        assert_eq!(attempts[0].checks_json, r#"[{"check_id":"check-a"}]"#);
        assert_eq!(attempts[1].checks_json, r#"[{"check_id":"check-b"}]"#);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_plan_runs_clears_stale_claims_on_non_running_rows_without_status_change() {
        let path = temp_db_path("plan_run_recover_non_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        for (id, status) in [
            ("plan-pending", "pending"),
            ("plan-waiting", "waiting_t2"),
            ("plan-completed", "completed"),
            ("plan-failed", "failed"),
        ] {
            store
                .create_plan_run(id, "owner", "{}", Some("topic"), Some("cli"))
                .unwrap();
            store
                .update_plan_run_status(id, status, PlanRunUpdateFields::default())
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                    params![unix_timestamp() - 301, id],
                )
                .unwrap();
        }

        std::thread::sleep(Duration::from_secs(1));
        let recovered = store.recover_stale_plan_runs(300).unwrap();
        assert_eq!(recovered, 4);

        for (id, status) in [
            ("plan-pending", "pending"),
            ("plan-waiting", "waiting_t2"),
            ("plan-completed", "completed"),
            ("plan-failed", "failed"),
        ] {
            let run = store.get_plan_run(id).unwrap().unwrap();
            assert_eq!(run.status, status);
            assert_eq!(run.claimed_at, None);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_plan_runs_clears_stale_pending_claim_without_changing_status() {
        let path = temp_db_path("plan_run_recover_pending");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE plan_runs SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, "plan-1"],
            )
            .unwrap();

        let recovered = store.recover_stale_plan_runs(300).unwrap();
        assert_eq!(recovered, 1);
        let pending = store.get_plan_run("plan-1").unwrap().unwrap();
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.claimed_at, None);

        let claimed = store
            .claim_next_runnable_plan_run(300)
            .unwrap()
            .expect("stale pending row should be claimable");
        assert_eq!(claimed.id, "plan-1");
        assert_eq!(claimed.status, "running");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_and_get_step_attempts_round_trip() {
        let path = temp_db_path("step_attempt_round_trip");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let first_attempt = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: Some("child-1".to_string()),
                summary_json: r#"{"stdout":"one"}"#.to_string(),
                checks_json: r#"[{"id":"check-1"}]"#.to_string(),
            })
            .unwrap();
        let second_attempt = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 1,
                status: "running".to_string(),
                child_session_id: Some("child-2".to_string()),
                summary_json: r#"{"stdout":"two"}"#.to_string(),
                checks_json: r#"[{"id":"check-2"}]"#.to_string(),
            })
            .unwrap();

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].id, first_attempt);
        assert_eq!(attempts[0].attempt, 0);
        assert_eq!(attempts[0].child_session_id.as_deref(), Some("child-1"));
        assert_eq!(attempts[0].summary_json, r#"{"stdout":"one"}"#);
        assert_eq!(attempts[0].checks_json, r#"[{"id":"check-1"}]"#);
        assert_eq!(attempts[0].status, "running");
        assert_eq!(attempts[1].id, second_attempt);
        assert_eq!(attempts[1].attempt, 1);
        assert_eq!(attempts[1].child_session_id.as_deref(), Some("child-2"));
        assert_eq!(attempts[1].summary_json, r#"{"stdout":"two"}"#);
        assert_eq!(attempts[1].checks_json, r#"[{"id":"check-2"}]"#);
        assert_eq!(attempts[1].status, "running");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_step_attempts_orders_by_revision_then_attempt() {
        let path = temp_db_path("step_attempt_revision_order");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 2,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"stdout":"rev2"}"#.to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 1,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"stdout":"rev1"}"#.to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].revision, 1);
        assert_eq!(attempts[0].attempt, 1);
        assert_eq!(attempts[1].revision, 2);
        assert_eq!(attempts[1].attempt, 0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_missing_plan_run() {
        let path = temp_db_path("step_attempt_missing_plan");
        let mut store = Store::new(&path).unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "missing".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("missing plan run should fail");
        assert!(
            err.to_string()
                .contains("failed to record plan step attempt")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_terminal_status_on_insert() {
        let path = temp_db_path("step_attempt_terminal_status");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "passed".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("terminal insert status should fail");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt start status")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_unknown_status_on_insert() {
        let path = temp_db_path("step_attempt_unknown_status");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "bogus".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("unknown insert status should fail");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt start status")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_negative_attempt_index() {
        let path = temp_db_path("step_attempt_negative_attempt");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: -1,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("negative attempt index should fail");
        assert!(err.to_string().contains("invalid plan step attempt index"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_negative_numeric_fields() {
        let path = temp_db_path("step_attempt_negative_numbers");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 0,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("negative numeric fields should fail");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt revision")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_step_attempt_rejects_empty_step_id() {
        let path = temp_db_path("step_attempt_empty_step_id");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan"}"#,
                Some("topic"),
                Some("cli"),
            )
            .unwrap();

        let err = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: String::new(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .expect_err("empty step id should fail");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt step id")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_step_attempt_status_sets_finished_at() {
        let path = temp_db_path("step_attempt_update");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let first_attempt = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"stdout":"one"}"#.to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        let second_attempt = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 1,
                step_id: "step-b".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: r#"{"stdout":"two"}"#.to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let first_finished_at = utc_timestamp();
        store
            .update_step_attempt_status(first_attempt, "passed", &first_finished_at)
            .unwrap();
        let second_finished_at = utc_timestamp();
        store
            .update_step_attempt_status(second_attempt, "failed", &second_finished_at)
            .unwrap();

        let step_zero_attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(step_zero_attempts[0].status, "passed");
        assert_eq!(
            step_zero_attempts[0].finished_at.as_deref(),
            Some(first_finished_at.as_str())
        );
        let step_one_attempts = store.get_step_attempts("plan-1", 1).unwrap();
        assert_eq!(step_one_attempts[0].status, "failed");
        assert_eq!(
            step_one_attempts[0].finished_at.as_deref(),
            Some(second_finished_at.as_str())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn finalize_step_attempt_updates_payloads_and_finished_at_atomically() {
        let path = temp_db_path("step_attempt_finalize");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let finished_at = utc_timestamp();
        store
            .finalize_step_attempt(
                attempt_id,
                "passed",
                &finished_at,
                r#"{"kind":"shell","stdout":"ok"}"#,
                r#"[{"check_id":"c1","verdict":"pass"}]"#,
            )
            .unwrap();

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts[0].status, "passed");
        assert_eq!(
            attempts[0].finished_at.as_deref(),
            Some(finished_at.as_str())
        );
        assert_eq!(
            attempts[0].summary_json,
            r#"{"kind":"shell","stdout":"ok"}"#
        );
        assert_eq!(
            attempts[0].checks_json,
            r#"[{"check_id":"c1","verdict":"pass"}]"#
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn finalize_step_attempt_rejects_second_finalization() {
        let path = temp_db_path("step_attempt_finalize_twice");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let first_finished_at = utc_timestamp();
        store
            .finalize_step_attempt(attempt_id, "passed", &first_finished_at, "{}", "[]")
            .unwrap();
        let err = store
            .finalize_step_attempt(attempt_id, "failed", &utc_timestamp(), "{}", "[]")
            .expect_err("attempt should not finalize twice");
        assert!(err.to_string().contains("already finalized"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn finalize_stale_step_attempts_crashes_running_attempts() {
        let path = temp_db_path("step_attempt_finalize_stale");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let first_attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        let second_attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 1,
                step_id: "step-b".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let finalized = store.finalize_stale_step_attempts("plan-1", 1).unwrap();
        assert_eq!(finalized, 2);

        let first_attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(first_attempts[0].id, first_attempt_id);
        assert_eq!(first_attempts[0].status, "crashed");
        assert!(first_attempts[0].finished_at.is_some());

        let second_attempts = store.get_step_attempts("plan-1", 1).unwrap();
        assert_eq!(second_attempts[0].id, second_attempt_id);
        assert_eq!(second_attempts[0].status, "crashed");
        assert!(second_attempts[0].finished_at.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_step_attempt_child_session_sets_child_id_on_running_attempt() {
        let path = temp_db_path("step_attempt_child_session");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        store
            .update_step_attempt_child_session(attempt_id, Some("child-1"))
            .unwrap();

        let attempts = store.get_step_attempts("plan-1", 0).unwrap();
        assert_eq!(attempts[0].child_session_id.as_deref(), Some("child-1"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_step_attempt_status_rejects_empty_finished_at() {
        let path = temp_db_path("step_attempt_empty_finished_at");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan"}"#,
                Some("topic"),
                Some("cli"),
            )
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let err = store
            .update_step_attempt_status(attempt_id, "passed", "")
            .expect_err("empty finished_at should fail");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt finished_at")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_step_attempt_status_rejects_second_finalization() {
        let path = temp_db_path("step_attempt_update_twice");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        store
            .update_step_attempt_status(attempt_id, "passed", &utc_timestamp())
            .unwrap();
        let err = store
            .update_step_attempt_status(attempt_id, "failed", &utc_timestamp())
            .expect_err("finalized attempt should not be mutable");
        assert!(err.to_string().contains("already finalized"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_step_attempt_status_rejects_running() {
        let path = temp_db_path("step_attempt_update_running");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let err = store
            .update_step_attempt_status(attempt_id, "running", &utc_timestamp())
            .expect_err("running is not a terminal status");
        assert!(
            err.to_string()
                .contains("invalid plan step attempt terminal status")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_step_attempts_returns_empty_for_missing_step() {
        let path = temp_db_path("step_attempt_empty");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();

        assert!(store.get_step_attempts("plan-1", 0).unwrap().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn step_attempts_cascade_when_plan_run_is_deleted() {
        let path = temp_db_path("step_attempt_cascade");
        let mut store = Store::new(&path).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run("plan-1", "owner", "{}", Some("topic"), Some("cli"))
            .unwrap();
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: "plan-1".to_string(),
                revision: 1,
                step_index: 0,
                step_id: "step-a".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        assert!(attempt_id > 0);

        store
            .conn
            .execute("DELETE FROM plan_runs WHERE id = ?1", params!["plan-1"])
            .unwrap();

        assert!(store.get_step_attempts("plan-1", 0).unwrap().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_and_list_sessions() {
        let path = temp_db_path("list");
        let mut store = Store::new(&path).unwrap();

        store
            .create_session("s1", Some(r#"{"source":"cli"}"#))
            .unwrap();
        store
            .create_session("s2", Some(r#"{"source":"api"}"#))
            .unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions, vec!["s1", "s2"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_child_session_records_parent_and_lists_children() {
        let path = temp_db_path("child_session");
        let mut store = Store::new(&path).unwrap();

        store
            .create_session("parent", Some(r#"{"kind":"root"}"#))
            .unwrap();
        store
            .create_child_session("parent", "child-a", Some(r#"{"kind":"child-a"}"#))
            .unwrap();
        store
            .create_child_session("parent", "child-b", Some(r#"{"kind":"child-b"}"#))
            .unwrap();

        assert_eq!(
            store.get_parent_session("child-a").unwrap(),
            Some("parent".to_string())
        );
        assert_eq!(store.get_parent_session("parent").unwrap(), None);
        assert_eq!(
            store.list_child_sessions("parent").unwrap(),
            vec!["child-a".to_string(), "child-b".to_string()]
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn get_session_metadata_reads_persisted_json() {
        let path = temp_db_path("session_metadata");
        let mut store = Store::new(&path).unwrap();

        store
            .create_session("root", Some(r#"{"tier":"t2","model":"gpt-5.4-mini"}"#))
            .unwrap();
        store.create_session("empty", None).unwrap();

        assert_eq!(
            store.get_session_metadata("root").unwrap(),
            Some(r#"{"tier":"t2","model":"gpt-5.4-mini"}"#.to_string())
        );
        assert_eq!(
            store.get_session_metadata("empty").unwrap(),
            Some("{}".to_string())
        );
        assert_eq!(store.get_session_metadata("missing").unwrap(), None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_child_session_rejects_missing_parent() {
        let path = temp_db_path("missing_parent");
        let mut store = Store::new(&path).unwrap();

        let err = store
            .create_child_session("missing", "child", None)
            .expect_err("missing parent should fail");
        assert!(err.to_string().contains("failed to create child session"));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn with_transaction_rolls_back_on_error() {
        let path = temp_db_path("transaction_rollback");
        let mut store = Store::new(&path).unwrap();

        let err = store
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO sessions (id, created_at, metadata, parent_session_id) VALUES (?1, ?2, ?3, ?4)",
                    params!["parent", utc_timestamp(), "{}", Option::<String>::None],
                )?;
                Err::<(), anyhow::Error>(anyhow::anyhow!("boom"))
            })
            .expect_err("transaction should fail");
        assert!(err.to_string().contains("boom"));
        assert!(store.list_sessions().unwrap().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enqueue_dequeue_and_mark_processed() {
        let path = temp_db_path("queue");
        let mut store = Store::new(&path).unwrap();
        store.create_session("worker", None).unwrap();

        let first = store.enqueue_message("worker", "user", "first", "cli");
        let second = store.enqueue_message("worker", "user", "second", "cli");
        assert!(first.is_ok());
        assert!(second.is_ok());

        let first_id = first.unwrap();
        let second_id = second.unwrap();

        let first_msg = store.dequeue_next_message("worker").unwrap().unwrap();
        assert_eq!(first_msg.id, first_id);
        assert_eq!(first_msg.role, "user");
        assert_eq!(first_msg.content, "first");
        assert_eq!(first_msg.status, "processing");
        let (status, claimed_at): (String, Option<i64>) = store
            .conn
            .query_row(
                "SELECT status, claimed_at FROM messages WHERE id = ?1",
                [first_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "processing");
        assert!(claimed_at.is_some());

        store.mark_processed(first_id).unwrap();
        let status: String = store
            .conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [first_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");

        let second_msg = store.dequeue_next_message("worker").unwrap().unwrap();
        assert_eq!(second_msg.id, second_id);
        assert_eq!(second_msg.content, "second");

        store.mark_processed(second_id).unwrap();
        let status: String = store
            .conn
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [second_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");

        let drained = store.dequeue_next_message("worker").unwrap();
        assert!(drained.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn create_session_is_idempotent() {
        let path = temp_db_path("idempotent");
        let mut store = Store::new(&path).unwrap();

        store
            .create_session("shared", Some(r#"{"source":"cli"}"#))
            .unwrap();
        store
            .create_session("shared", Some(r#"{"source":"cli"}"#))
            .unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions, vec!["shared"]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn dequeue_claim_is_atomic_across_concurrent_workers() {
        use std::sync::{Arc, Barrier};

        let path = temp_db_path("atomic_claim");
        let mut store = Store::new(&path).unwrap();
        store.create_session("worker", None).unwrap();
        let message_id = store
            .enqueue_message("worker", "user", "first", "cli")
            .unwrap();
        drop(store);

        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let mut store = Store::new(&path).unwrap();
                barrier.wait();
                store
                    .dequeue_next_message("worker")
                    .unwrap()
                    .map(|row| row.id)
            }));
        }

        let mut claimed = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .flatten()
            .collect::<Vec<_>>();
        claimed.sort_unstable();
        assert_eq!(claimed, vec![message_id]);

        let store = Store::new(&path).unwrap();
        let (status, claimed_at): (String, Option<i64>) = store
            .conn
            .query_row(
                "SELECT status, claimed_at FROM messages WHERE id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "processing");
        assert!(claimed_at.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_messages_respects_age_threshold() {
        let path = temp_db_path("recover_stale");
        let mut store = Store::new(&path).unwrap();
        store.create_session("worker", None).unwrap();
        let message_id = store
            .enqueue_message("worker", "user", "first", "cli")
            .unwrap();
        store.dequeue_next_message("worker").unwrap().unwrap();
        store
            .conn
            .execute(
                "UPDATE messages SET claimed_at = ?1 WHERE id = ?2",
                params![unix_timestamp() - 301, message_id],
            )
            .unwrap();

        let recovered = store.recover_stale_messages(300).unwrap();
        assert_eq!(recovered, 1);

        let (status, claimed_at): (String, Option<i64>) = store
            .conn
            .query_row(
                "SELECT status, claimed_at FROM messages WHERE id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(claimed_at, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recover_stale_messages_leaves_fresh_claims_processing() {
        let path = temp_db_path("recover_fresh");
        let mut store = Store::new(&path).unwrap();
        store.create_session("worker", None).unwrap();
        let message_id = store
            .enqueue_message("worker", "user", "first", "cli")
            .unwrap();
        store.dequeue_next_message("worker").unwrap().unwrap();

        let recovered = store.recover_stale_messages(300).unwrap();
        assert_eq!(recovered, 0);

        let (status, claimed_at): (String, Option<i64>) = store
            .conn
            .query_row(
                "SELECT status, claimed_at FROM messages WHERE id = ?1",
                [message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "processing");
        assert!(claimed_at.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_new_migrates_missing_claimed_at_column() {
        let path = temp_db_path("migrate_claimed_at");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let mut store = Store::new(&path).unwrap();
        let has_claimed_at = has_column(&store.conn, "messages", "claimed_at").unwrap();
        assert!(has_claimed_at);

        store.create_session("worker", None).unwrap();
        let message_id = store
            .enqueue_message("worker", "user", "first", "cli")
            .unwrap();
        let claimed = store.dequeue_next_message("worker").unwrap().unwrap();
        assert_eq!(claimed.id, message_id);

        let claimed_at: Option<i64> = store
            .conn
            .query_row(
                "SELECT claimed_at FROM messages WHERE id = ?1",
                [message_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(claimed_at.is_some());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_new_migrates_missing_parent_session_id_column() {
        let path = temp_db_path("migrate_parent_session_id");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE TABLE messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            "#,
        )
        .unwrap();
        drop(conn);

        let mut store = Store::new(&path).unwrap();
        let has_parent = has_column(&store.conn, "sessions", "parent_session_id").unwrap();
        assert!(has_parent);

        store.create_session("parent", None).unwrap();
        store
            .create_child_session("parent", "child", Some(r#"{"linked":true}"#))
            .unwrap();
        assert_eq!(
            store.get_parent_session("child").unwrap(),
            Some("parent".to_string())
        );

        let _ = std::fs::remove_file(&path);
    }
}
