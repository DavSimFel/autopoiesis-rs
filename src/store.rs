//! SQLite-backed session registry and per-session message queue.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

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

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let conn = Connection::open(path).context("failed to open sqlite store")?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                metadata TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                source TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session_status_created_at
                ON messages(session_id, status, created_at, id);",
        )
        .context("failed to initialize sqlite schema")?;

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

    pub fn dequeue_next_message(&mut self, session_id: &str) -> Result<Option<QueuedMessage>> {
        let tx = self.conn.transaction().context("failed to start dequeue transaction")?;
        let result = {
            let mut statement = tx
                .prepare(
                    "SELECT id, session_id, role, content, source, status, created_at
                     FROM messages
                     WHERE session_id = ?1 AND status = 'pending'
                     ORDER BY created_at ASC, id ASC
                     LIMIT 1",
                )
                .context("failed to prepare dequeue query")?;

            statement
                .query_row(params![session_id], |row| {
                    Ok(QueuedMessage {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        role: row.get(2)?,
                        content: row.get(3)?,
                        source: row.get(4)?,
                        status: row.get(5)?,
                        created_at: row.get(6)?,
                    })
                })
        };

        match result {
            Ok(message) => {
                tx.execute(
                    "UPDATE messages SET status = 'processing' WHERE id = ?1",
                    params![message.id],
                )
                .context("failed to claim message")?;
                tx.commit().context("failed to commit dequeue transaction")?;
                Ok(Some(message))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                tx.rollback().context("failed to rollback dequeue transaction")?;
                Ok(None)
            }
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
    /// Resets them to 'pending' so they can be retried.
    pub fn recover_stale_messages(&mut self) -> Result<u64> {
        let count = self
            .conn
            .execute(
                "UPDATE messages SET status = 'pending' WHERE status = 'processing'",
                [],
            )
            .context("failed to recover stale messages")?;
        Ok(count as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autopoiesis_store_test_{prefix}_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        path
    }

    #[test]
    fn create_and_list_sessions() {
        let path = temp_db_path("list");
        let mut store = Store::new(&path).unwrap();

        store.create_session("s1", Some(r#"{"source":"cli"}"#)).unwrap();
        store.create_session("s2", Some(r#"{"source":"api"}"#)).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions, vec!["s1", "s2"]);

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
        assert_eq!(first_msg.status, "pending");
        let status: String = store
            .conn
            .query_row("SELECT status FROM messages WHERE id = ?1", [first_id], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "processing");

        store.mark_processed(first_id).unwrap();
        let status: String = store
            .conn
            .query_row("SELECT status FROM messages WHERE id = ?1", [first_id], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "processed");

        let second_msg = store.dequeue_next_message("worker").unwrap().unwrap();
        assert_eq!(second_msg.id, second_id);
        assert_eq!(second_msg.content, "second");

        store.mark_processed(second_id).unwrap();
        let status: String = store
            .conn
            .query_row("SELECT status FROM messages WHERE id = ?1", [second_id], |row| row.get(0))
            .unwrap();
        assert_eq!(status, "processed");

        let drained = store.dequeue_next_message("worker").unwrap();
        assert!(drained.is_none());

        let _ = std::fs::remove_file(&path);
    }
}
