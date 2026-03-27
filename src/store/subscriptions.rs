//! Subscription persistence helpers.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::Path;
use std::time::SystemTime;

use super::{SubscriptionRow, format_system_time};

pub(super) fn create_subscription_for_session(
    conn: &Connection,
    session_id: Option<&str>,
    topic: &str,
    path: &str,
    filter: Option<&str>,
) -> Result<i64> {
    let timestamp = format_system_time(SystemTime::now());
    conn.execute(
        "INSERT INTO subscriptions (session_id, topic, path, filter, activated_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session_id, topic, path, filter, timestamp, timestamp],
    )
    .context("failed to insert subscription")?;
    Ok(conn.last_insert_rowid())
}

pub(super) fn delete_subscription_for_session(
    conn: &Connection,
    session_id: Option<&str>,
    topic: &str,
    path: &str,
    filter: Option<&str>,
) -> Result<usize> {
    let count = if filter.is_some() {
        conn.execute(
            "DELETE FROM subscriptions
             WHERE COALESCE(session_id, '') = COALESCE(?1, '')
               AND topic = ?2
               AND path = ?3
               AND COALESCE(filter, '') = COALESCE(?4, '')",
            params![session_id, topic, path, filter],
        )
        .context("failed to delete subscription")?
    } else {
        conn.execute(
            "DELETE FROM subscriptions
             WHERE COALESCE(session_id, '') = COALESCE(?1, '')
               AND topic = ?2
               AND path = ?3",
            params![session_id, topic, path],
        )
        .context("failed to delete subscription")?
    };
    Ok(count)
}

pub(super) fn list_subscriptions(
    conn: &Connection,
    topic: Option<&str>,
) -> Result<Vec<SubscriptionRow>> {
    let mut statement = if topic.is_some() {
        conn.prepare(
            "SELECT id, session_id, topic, path, filter, activated_at, updated_at
             FROM subscriptions
             WHERE topic = ?1
             ORDER BY CASE WHEN updated_at > activated_at THEN updated_at ELSE activated_at END ASC, id ASC",
        )
        .context("failed to prepare list_subscriptions query")?
    } else {
        conn.prepare(
            "SELECT id, session_id, topic, path, filter, activated_at, updated_at
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
                    session_id: row.get(1)?,
                    topic: row.get(2)?,
                    path: row.get(3)?,
                    filter: row.get(4)?,
                    activated_at: row.get(5)?,
                    updated_at: row.get(6)?,
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
                    session_id: row.get(1)?,
                    topic: row.get(2)?,
                    path: row.get(3)?,
                    filter: row.get(4)?,
                    activated_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .context("failed to query subscriptions")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to collect subscriptions")?
    };

    Ok(rows)
}

pub(super) fn list_subscriptions_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<SubscriptionRow>> {
    let mut rows = list_subscriptions(conn, None)?;
    rows.retain(|row| {
        row.session_id
            .as_deref()
            .is_none_or(|value| value == session_id)
    });
    rows.sort_by(|left, right| {
        let left_effective = if left.updated_at >= left.activated_at {
            &left.updated_at
        } else {
            &left.activated_at
        };
        let right_effective = if right.updated_at >= right.activated_at {
            &right.updated_at
        } else {
            &right.activated_at
        };
        left_effective
            .cmp(right_effective)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut chosen: HashMap<(String, Option<String>), SubscriptionRow> = HashMap::new();
    for row in rows {
        let key = (row.path.clone(), row.filter.clone());
        let replace = match chosen.get(&key) {
            None => true,
            Some(existing) => {
                let existing_is_global = existing.session_id.is_none();
                let row_is_session = row.session_id.as_deref() == Some(session_id);
                row_is_session && existing_is_global
            }
        };
        if replace {
            chosen.insert(key, row);
        }
    }

    let mut deduped = chosen.into_values().collect::<Vec<_>>();
    deduped.sort_by(|left, right| {
        let left_effective = if left.updated_at >= left.activated_at {
            &left.updated_at
        } else {
            &left.activated_at
        };
        let right_effective = if right.updated_at >= right.activated_at {
            &right.updated_at
        } else {
            &right.activated_at
        };
        left_effective
            .cmp(right_effective)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(deduped)
}

#[cfg(not(test))]
pub(super) fn refresh_subscription_timestamps(conn: &Connection) -> Result<u64> {
    let rows = list_subscriptions(conn, None)?;
    let mut refreshed = 0u64;

    for row in rows {
        let path = Path::new(&row.path);
        let Ok(metadata) = std::fs::metadata(path) else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let updated_at = format_system_time(modified);
        if updated_at > row.updated_at {
            conn.execute(
                "UPDATE subscriptions SET updated_at = ?1 WHERE id = ?2",
                params![updated_at, row.id],
            )
            .context("failed to update subscription timestamp")?;
            refreshed += 1;
        }
    }

    Ok(refreshed)
}

#[cfg(test)]
pub(super) fn refresh_subscription_timestamps_with<F>(
    conn: &Connection,
    mut modified_for: F,
) -> Result<u64>
where
    F: FnMut(&Path) -> Option<SystemTime>,
{
    let rows = list_subscriptions(conn, None)?;
    let mut refreshed = 0u64;

    for row in rows {
        let path = Path::new(&row.path);
        let Some(modified) = modified_for(path) else {
            continue;
        };
        let updated_at = format_system_time(modified);
        if updated_at > row.updated_at {
            conn.execute(
                "UPDATE subscriptions SET updated_at = ?1 WHERE id = ?2",
                params![updated_at, row.id],
            )
            .context("failed to update subscription timestamp")?;
            refreshed += 1;
        }
    }

    Ok(refreshed)
}

impl super::Store {
    pub fn create_subscription_for_session(
        &mut self,
        session_id: Option<&str>,
        topic: &str,
        path: &str,
        filter: Option<&str>,
    ) -> Result<i64> {
        create_subscription_for_session(&self.conn, session_id, topic, path, filter)
    }

    pub fn create_subscription(
        &mut self,
        topic: &str,
        path: &str,
        filter: Option<&str>,
    ) -> Result<i64> {
        self.create_subscription_for_session(None, topic, path, filter)
    }

    pub fn delete_subscription_for_session(
        &mut self,
        session_id: Option<&str>,
        topic: &str,
        path: &str,
        filter: Option<&str>,
    ) -> Result<usize> {
        delete_subscription_for_session(&self.conn, session_id, topic, path, filter)
    }

    pub fn delete_subscription(&mut self, topic: &str, path: &str) -> Result<usize> {
        self.delete_subscription_for_session(None, topic, path, None)
    }

    pub fn list_subscriptions(&self, topic: Option<&str>) -> Result<Vec<SubscriptionRow>> {
        list_subscriptions(&self.conn, topic)
    }

    pub fn list_subscriptions_for_session(&self, session_id: &str) -> Result<Vec<SubscriptionRow>> {
        list_subscriptions_for_session(&self.conn, session_id)
    }

    pub fn refresh_subscription_timestamps(&mut self) -> Result<u64> {
        #[cfg(test)]
        {
            return self.refresh_subscription_timestamps_with(|path| {
                std::fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
            });
        }

        #[cfg(not(test))]
        {
            refresh_subscription_timestamps(&self.conn)
        }
    }

    #[cfg(test)]
    pub(crate) fn refresh_subscription_timestamps_with<F>(&mut self, modified_for: F) -> Result<u64>
    where
        F: FnMut(&Path) -> Option<SystemTime>,
    {
        refresh_subscription_timestamps_with(&self.conn, modified_for)
    }
}
