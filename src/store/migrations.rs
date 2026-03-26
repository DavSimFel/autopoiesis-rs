//! SQLite migration helpers.

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::has_column;

pub(super) fn ensure_messages_claimed_at_column(conn: &Connection) -> Result<()> {
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

pub(super) fn ensure_sessions_parent_session_id_column(conn: &Connection) -> Result<()> {
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

pub(super) fn ensure_plan_runs_table(conn: &Connection) -> Result<()> {
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
    cleanup_legacy_plan_rows(conn)?;
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_plan_runs_owner_session_created_at
            ON plan_runs(owner_session_id, created_at, id);
        CREATE INDEX IF NOT EXISTS idx_plan_runs_status_claimed_at
            ON plan_runs(status, claimed_at, id);
        DROP INDEX IF EXISTS idx_plan_step_attempts_run_step_attempt;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_plan_step_attempts_run_step_attempt
            ON plan_step_attempts(plan_run_id, revision, step_index, attempt);
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
    Ok(())
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
        DELETE FROM plan_step_attempts
         WHERE rowid NOT IN (
             SELECT rowid
             FROM (
                 SELECT
                     rowid,
                     ROW_NUMBER() OVER (
                         PARTITION BY plan_run_id, revision, step_index, attempt
                         ORDER BY
                             CASE
                                 WHEN status IN ('passed', 'failed', 'crashed') THEN 1
                                 ELSE 0
                             END DESC,
                             CASE
                                 WHEN finished_at IS NOT NULL AND finished_at != '' THEN 1
                                 ELSE 0
                             END DESC,
                             id DESC
                     ) AS rn
                 FROM plan_step_attempts
             )
             WHERE rn = 1
         );
        "#,
    )
    .context("failed to clean up legacy plan rows")?;
    Ok(())
}
