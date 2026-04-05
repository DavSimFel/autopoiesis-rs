use anyhow::Result;
use tracing::{info, warn};

use crate::app::args::{SubscriptionAddArgs, SubscriptionCommand};
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::subscription::{self, SubscriptionFilter, SubscriptionRecord};

fn default_subscription_topic(topic: Option<String>) -> String {
    topic.unwrap_or_else(|| "_default".to_string())
}

fn subscription_filter(args: &SubscriptionAddArgs) -> Result<SubscriptionFilter> {
    SubscriptionFilter::from_flags(
        args.lines.as_deref(),
        args.regex.as_deref(),
        args.head,
        args.tail,
        args.jq.as_deref(),
    )
}

fn render_subscription_summary(records: &[SubscriptionRecord]) -> Option<usize> {
    let mut total = 0usize;
    for record in records {
        match record.utilization_tokens() {
            Ok(count) => total += count,
            Err(error) => {
                warn!(
                    "warning: failed to estimate subscription utilization for {}: {error}",
                    record.path.display()
                );
                return None;
            }
        }
    }

    Some(total)
}

fn print_subscription_rows(records: &[SubscriptionRecord]) {
    for record in records {
        info!(target: STDOUT_USER_OUTPUT_TARGET, "{}", record.format_listing());
    }
}

pub(crate) async fn handle_subscription_command(command: SubscriptionCommand) -> Result<()> {
    let mut store = autopoiesis::store::Store::new(autopoiesis::paths::default_queue_db_path())?;

    match command {
        SubscriptionCommand::Add(args) => {
            let topic = default_subscription_topic(args.topic.clone());
            let normalized_path = subscription::normalize_path(&args.path)?;
            subscription::ensure_readable_subscription_path(&normalized_path)?;
            let filter = subscription_filter(&args)?;
            store.create_subscription(
                &topic,
                &normalized_path.display().to_string(),
                filter.to_storage().as_deref(),
            )?;
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(None)?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            match render_subscription_summary(&records) {
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
            }
        }
        SubscriptionCommand::Remove(args) => {
            let topic = default_subscription_topic(args.topic.clone());
            let normalized_path = subscription::normalize_path(&args.path)?;
            let deleted =
                store.delete_subscription(&topic, &normalized_path.display().to_string())?;
            info!(
                target: STDOUT_USER_OUTPUT_TARGET,
                "removed {deleted} subscription(s)"
            );
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(None)?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            match render_subscription_summary(&records) {
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
            }
        }
        SubscriptionCommand::List(args) => {
            let _ = store.refresh_subscription_timestamps();
            let rows = store.list_subscriptions(args.topic.as_deref())?;
            let records = rows
                .into_iter()
                .map(SubscriptionRecord::from_row)
                .collect::<Result<Vec<_>>>()?;
            print_subscription_rows(&records);
            match render_subscription_summary(&records) {
                Some(total) => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: {total} tokens"
                ),
                None => info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "subscription utilization: unavailable"
                ),
            }
        }
    }

    Ok(())
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::app::args::{SubscriptionAddArgs, SubscriptionListArgs, SubscriptionRemoveArgs};
    use autopoiesis::logging::PlainMessageFormatter;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("writer lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn handle_subscription_command_covers_add_list_and_remove() {
        let _cwd_guard = crate::app::test_cwd_lock().lock().await;
        let temp_root = std::env::temp_dir().join(format!(
            "autopoiesis_subscription_command_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_root.join("sessions")).unwrap();
        let note_path = temp_root.join("notes.txt");
        std::fs::write(&note_path, "line one\nline two\n").unwrap();
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_root).unwrap();
        struct RestoreDir(std::path::PathBuf);
        impl Drop for RestoreDir {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _restore_dir = RestoreDir(old_dir);

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .event_format(PlainMessageFormatter)
                .with_writer({
                    let stdout = stdout.clone();
                    move || SharedWriter(stdout.clone())
                })
                .with_ansi(false),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        handle_subscription_command(SubscriptionCommand::Add(SubscriptionAddArgs {
            path: note_path.to_string_lossy().into_owned(),
            topic: Some("alpha".to_string()),
            lines: None,
            regex: None,
            head: None,
            tail: None,
            jq: None,
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("subscription utilization:"));
        stdout.lock().unwrap().clear();

        handle_subscription_command(SubscriptionCommand::List(SubscriptionListArgs {
            topic: Some("alpha".to_string()),
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("notes.txt"));
        assert!(output.contains("subscription utilization:"));
        stdout.lock().unwrap().clear();

        handle_subscription_command(SubscriptionCommand::Remove(SubscriptionRemoveArgs {
            path: note_path.to_string_lossy().into_owned(),
            topic: Some("alpha".to_string()),
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("removed 1 subscription(s)"));
    }
}
