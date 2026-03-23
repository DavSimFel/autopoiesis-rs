//! SQLite-backed session registry and per-session message queue.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
        conn.busy_timeout(Duration::from_secs(5))
            .context("failed to configure sqlite busy timeout")?;
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
        ensure_messages_claimed_at_column(&conn)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_status_claimed_at
             ON messages(status, claimed_at, id)",
            [],
        )
        .context("failed to initialize claimed_at index")?;

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
}
