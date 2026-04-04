use std::fs;
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use tracing::warn;

use crate::time::utc_timestamp;

use super::{Observer, TraceEvent};

pub struct SqliteObserver {
    conn: Mutex<Connection>,
}

impl SqliteObserver {
    pub fn new(sessions_dir: &Path) -> Result<Self> {
        fs::create_dir_all(sessions_dir)
            .with_context(|| format!("failed to create {}", sessions_dir.display()))?;
        let path = sessions_dir.join("traces.sqlite");
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("failed to configure sqlite trace busy timeout")?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS trace_events (
                id INTEGER PRIMARY KEY,
                event_type TEXT NOT NULL,
                session_id TEXT,
                turn_id TEXT,
                plan_run_id TEXT,
                eval_run_id TEXT,
                timestamp TEXT NOT NULL,
                payload_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_trace_events_eval_run_timestamp
                ON trace_events(eval_run_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_trace_events_session_timestamp
                ON trace_events(session_id, timestamp);
            CREATE INDEX IF NOT EXISTS idx_trace_events_plan_run_timestamp
                ON trace_events(plan_run_id, timestamp);
            "#,
        )
        .context("failed to initialize trace sqlite schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn insert_event(&self, event: &TraceEvent) -> Result<()> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "INSERT INTO trace_events (
                event_type,
                session_id,
                turn_id,
                plan_run_id,
                eval_run_id,
                timestamp,
                payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.event_type(),
                event.session_id(),
                event.turn_id(),
                event.plan_run_id(),
                event.eval_run_id(),
                utc_timestamp(),
                serde_json::to_string(event).context("failed to serialize trace event")?,
            ],
        )
        .context("failed to insert trace event")?;
        Ok(())
    }
}

impl Observer for SqliteObserver {
    fn emit(&self, event: &TraceEvent) {
        if let Err(error) = self.insert_event(event) {
            warn!(%error, event_type = event.event_type(), "failed to persist trace event");
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::observe::TraceEvent;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autopoiesis_sqlite_observe_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn inserts_rows_and_indexes_trace_events() {
        let dir = temp_dir("insert");
        let observer = SqliteObserver::new(&dir).unwrap();
        let event = TraceEvent::PlanCompleted {
            session_id: "session-1".to_string(),
            plan_run_id: "plan-1".to_string(),
            total_attempts: 3,
        };
        observer.emit(&event);

        let conn = Connection::open(dir.join("traces.sqlite")).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM trace_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let event_type: String = conn
            .query_row("SELECT event_type FROM trace_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(event_type, "PlanCompleted");

        let payload: String = conn
            .query_row("SELECT payload_json FROM trace_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        let decoded: TraceEvent = serde_json::from_str(&payload).unwrap();
        assert!(matches!(decoded, TraceEvent::PlanCompleted { .. }));

        let indexes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name IN (
                    'idx_trace_events_eval_run_timestamp',
                    'idx_trace_events_session_timestamp',
                    'idx_trace_events_plan_run_timestamp'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexes, 3);
    }
}
