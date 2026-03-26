//! Session metadata helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::util::utc_timestamp;

pub(super) fn create_session(
    conn: &Connection,
    session_id: &str,
    metadata: Option<&str>,
) -> Result<()> {
    let metadata = metadata.unwrap_or("{}");
    conn.execute(
        "INSERT OR IGNORE INTO sessions (id, created_at, metadata) VALUES (?1, ?2, ?3)",
        params![session_id, utc_timestamp(), metadata],
    )
    .context("failed to create session")?;
    Ok(())
}

pub(super) fn create_child_session(
    conn: &Connection,
    parent_id: &str,
    child_id: &str,
    metadata: Option<&str>,
) -> Result<()> {
    let metadata = metadata.unwrap_or("{}");
    conn.execute(
        "INSERT INTO sessions (id, created_at, metadata, parent_session_id) VALUES (?1, ?2, ?3, ?4)",
        params![child_id, utc_timestamp(), metadata, parent_id],
    )
    .context("failed to create child session")?;
    Ok(())
}

pub(super) fn create_child_session_with_task(
    store: &mut super::Store,
    parent_id: &str,
    child_id: &str,
    metadata: Option<&str>,
    task: &str,
    source: &str,
) -> Result<()> {
    let metadata = metadata.unwrap_or("{}");
    store.with_transaction(|tx| {
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

pub(super) fn list_sessions(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn
        .prepare("SELECT id FROM sessions ORDER BY created_at ASC, id ASC")
        .context("failed to prepare list_sessions query")?;
    let sessions = statement
        .query_map([], |row| row.get::<_, String>(0))
        .context("failed to query sessions")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect sessions")?;

    Ok(sessions)
}

pub(super) fn get_parent_session(conn: &Connection, child_id: &str) -> Result<Option<String>> {
    let mut statement = conn
        .prepare("SELECT parent_session_id FROM sessions WHERE id = ?1")
        .context("failed to prepare get_parent_session query")?;
    match statement.query_row(params![child_id], |row| row.get::<_, Option<String>>(0)) {
        Ok(parent_id) => Ok(parent_id),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context("failed to read parent session"),
    }
}

pub(super) fn get_session_metadata(conn: &Connection, session_id: &str) -> Result<Option<String>> {
    let mut statement = conn
        .prepare("SELECT metadata FROM sessions WHERE id = ?1")
        .context("failed to prepare get_session_metadata query")?;
    match statement.query_row(params![session_id], |row| row.get::<_, Option<String>>(0)) {
        Ok(metadata) => Ok(metadata),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error).context("failed to read session metadata"),
    }
}

pub(super) fn list_child_sessions(conn: &Connection, parent_id: &str) -> Result<Vec<String>> {
    let mut statement = conn
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
