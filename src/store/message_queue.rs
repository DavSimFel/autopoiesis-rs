//! Message queue persistence helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use super::{QueuedMessage, unix_timestamp};
use crate::util::utc_timestamp;

pub(super) fn enqueue_message(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &str,
    source: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO messages (session_id, role, content, source, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, role, content, source, utc_timestamp()],
    )
    .context("failed to enqueue message")?;
    Ok(conn.last_insert_rowid())
}

pub(super) fn enqueue_message_in_transaction(
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

pub(super) fn dequeue_next_message(
    conn: &Connection,
    session_id: &str,
) -> Result<Option<QueuedMessage>> {
    // Queue claim atomicity boundary: this single UPDATE ... RETURNING statement must claim
    // exactly one pending row or none, so concurrent drains never double-process a message.
    let claimed_at = unix_timestamp();
    let mut statement = conn
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

pub(super) fn mark_processed(conn: &Connection, message_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET status = 'processed' WHERE id = ?1",
        params![message_id],
    )
    .context("failed to mark message processed")?;
    Ok(())
}

pub(super) fn mark_failed(conn: &Connection, message_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE messages SET status = 'failed' WHERE id = ?1",
        params![message_id],
    )
    .context("failed to mark message failed")?;
    Ok(())
}

pub(super) fn recover_stale_messages(conn: &Connection, stale_after_secs: u64) -> Result<u64> {
    let stale_after_secs = stale_after_secs.min(i64::MAX as u64) as i64;
    let stale_before = unix_timestamp().saturating_sub(stale_after_secs);
    let count = conn
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
