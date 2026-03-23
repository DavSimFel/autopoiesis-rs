//! SQLite-backed session registry and per-session message queue.

use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

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
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
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
        let tx = self
            .conn
            .transaction()
            .context("failed to start dequeue transaction")?;
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

            statement.query_row(params![session_id], |row| {
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
                tx.commit()
                    .context("failed to commit dequeue transaction")?;
                Ok(Some(message))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                tx.rollback()
                    .context("failed to rollback dequeue transaction")?;
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
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_store_test_{prefix}_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
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
            .query_row(
                "SELECT status FROM messages WHERE id = ?1",
                [first_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processing");

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
}
