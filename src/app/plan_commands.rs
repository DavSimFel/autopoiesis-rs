use anyhow::Result;
use tracing::info;

use crate::app::args::PlanCommand;
use autopoiesis::logging::STDOUT_USER_OUTPUT_TARGET;
use autopoiesis::store;

fn plan_run_retries(store: &store::Store, plan_run: &store::PlanRun) -> Result<i64> {
    let next_attempt = store.next_step_attempt_index_for_run(plan_run)?;
    Ok(if next_attempt > 0 {
        next_attempt - 1
    } else {
        0
    })
}

fn resolve_plan_run_for_status(
    store: &store::Store,
    plan_run_id: Option<&str>,
) -> Result<Option<store::PlanRun>> {
    match plan_run_id {
        Some(plan_run_id) => store.get_plan_run(plan_run_id),
        None => Ok(store.list_recent_active_plan_runs(1)?.into_iter().next()),
    }
}

fn format_plan_run_summary(plan_run: &store::PlanRun, retries: i64) -> String {
    let current_step = if plan_run.current_step_index >= 0 {
        plan_run.current_step_index.to_string()
    } else {
        "n/a".to_string()
    };
    format!(
        "{} status={} step={} retries={} claimed_at={} updated_at={}",
        plan_run.id,
        plan_run.status,
        current_step,
        retries,
        plan_run
            .claimed_at
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string()),
        plan_run.updated_at,
    )
}

pub(crate) async fn handle_plan_command(command: PlanCommand) -> Result<()> {
    let mut store = store::Store::new("sessions/queue.sqlite")?;

    match command {
        PlanCommand::Status(args) => {
            match resolve_plan_run_for_status(&store, args.plan_run_id.as_deref())? {
                Some(plan_run) => {
                    let retries = plan_run_retries(&store, &plan_run)?;
                    info!(
                        target: STDOUT_USER_OUTPUT_TARGET,
                        "{}",
                        format_plan_run_summary(&plan_run, retries)
                    );
                    if let Some(failure_json) = &plan_run.last_failure_json {
                        info!(
                            target: STDOUT_USER_OUTPUT_TARGET,
                            "last failure: {failure_json}"
                        );
                    }
                }
                None => match args.plan_run_id {
                    Some(plan_run_id) => info!(
                        target: STDOUT_USER_OUTPUT_TARGET,
                        "plan run {} not found",
                        plan_run_id
                    ),
                    None => info!(
                        target: STDOUT_USER_OUTPUT_TARGET,
                        "no active plan runs"
                    ),
                },
            }
        }
        PlanCommand::List(args) => {
            let plan_runs = store.list_recent_plan_runs(args.limit)?;
            if plan_runs.is_empty() {
                info!(target: STDOUT_USER_OUTPUT_TARGET, "no recent plan runs");
            } else {
                for plan_run in plan_runs {
                    let retries = plan_run_retries(&store, &plan_run)?;
                    info!(
                        target: STDOUT_USER_OUTPUT_TARGET,
                        "{}",
                        format_plan_run_summary(&plan_run, retries)
                    );
                }
            }
        }
        PlanCommand::Resume(args) => {
            if store.resume_waiting_plan_run(&args.plan_run_id)? {
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "resumed plan run {}",
                    args.plan_run_id
                );
            } else {
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "plan run {} is not waiting_t2",
                    args.plan_run_id
                );
            }
        }
        PlanCommand::Cancel(args) => {
            if store.cancel_plan_run(&args.plan_run_id)? {
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "cancelled plan run {}",
                    args.plan_run_id
                );
            } else {
                info!(
                    target: STDOUT_USER_OUTPUT_TARGET,
                    "plan run {} could not be cancelled",
                    args.plan_run_id
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::args::PlanStatusArgs;
    use autopoiesis::logging::{PlainMessageFormatter, STDOUT_USER_OUTPUT_TARGET};
    use autopoiesis::store::{PlanRunUpdateFields, Store};
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::prelude::*;

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

    #[test]
    fn plan_summary_includes_status_step_and_retries() {
        let temp_root = std::env::temp_dir().join(format!(
            "autopoiesis_plan_cli_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();
        let mut store = Store::new(temp_root.join("queue.sqlite")).unwrap();
        store.create_session("owner", None).unwrap();
        store
            .create_plan_run(
                "plan-1",
                "owner",
                r#"{"kind":"plan","steps":[]}"#,
                Some("step-2"),
                Some("{\"error\":\"boom\"}"),
            )
            .unwrap();
        store
            .update_plan_run_status(
                "plan-1",
                "running",
                PlanRunUpdateFields {
                    current_step_index: Some(2),
                    ..Default::default()
                },
            )
            .unwrap();

        let plan_run = resolve_plan_run_for_status(&store, Some("plan-1"))
            .unwrap()
            .expect("plan run should exist");
        let summary =
            format_plan_run_summary(&plan_run, plan_run_retries(&store, &plan_run).unwrap());
        assert!(summary.contains("plan-1"));
        assert!(summary.contains("status=running"));
        assert!(summary.contains("step=2"));
        assert!(summary.contains("retries=0"));
        assert!(summary.contains("claimed_at="));
        assert!(summary.contains("updated_at="));
    }

    #[test]
    fn status_lookup_returns_missing_plan_as_none() {
        let temp_root = std::env::temp_dir().join(format!(
            "autopoiesis_plan_lookup_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_root).unwrap();
        let store = Store::new(temp_root.join("queue.sqlite")).unwrap();

        assert!(
            resolve_plan_run_for_status(&store, Some("missing"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn status_args_support_optional_id() {
        let args = PlanStatusArgs { plan_run_id: None };
        assert!(args.plan_run_id.is_none());
    }

    #[tokio::test]
    async fn handle_plan_command_emits_expected_output_branches() {
        let _cwd_guard = crate::app::test_cwd_lock().lock().unwrap();
        let temp_root = std::env::temp_dir().join(format!(
            "autopoiesis_plan_command_output_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_root.join("sessions")).unwrap();
        let old_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_root).unwrap();
        struct RestoreDir(std::path::PathBuf);
        impl Drop for RestoreDir {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let _restore_dir = RestoreDir(old_dir);

        {
            let mut store = Store::new("sessions/queue.sqlite").unwrap();
            store.create_session("owner", None).unwrap();
            store
                .create_plan_run(
                    "plan-waiting",
                    "owner",
                    r#"{"kind":"plan","steps":[]}"#,
                    None,
                    None,
                )
                .unwrap();
            store
                .update_plan_run_status("plan-waiting", "waiting_t2", Default::default())
                .unwrap();
        }

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .event_format(PlainMessageFormatter)
                .with_writer({
                    let stdout = stdout.clone();
                    move || SharedWriter(stdout.clone())
                })
                .with_ansi(false)
                .with_filter(EnvFilter::new(format!("{STDOUT_USER_OUTPUT_TARGET}=trace"))),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        handle_plan_command(PlanCommand::Status(PlanStatusArgs {
            plan_run_id: Some("missing-plan".to_string()),
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("plan run missing-plan not found"));
        stdout.lock().unwrap().clear();

        handle_plan_command(PlanCommand::List(crate::app::args::PlanListArgs {
            limit: 10,
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("plan-waiting"));
        assert!(output.contains("status=waiting_t2"));
        stdout.lock().unwrap().clear();

        handle_plan_command(PlanCommand::Resume(crate::app::args::PlanRunIdArgs {
            plan_run_id: "plan-waiting".to_string(),
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("resumed plan run plan-waiting"));
        stdout.lock().unwrap().clear();

        handle_plan_command(PlanCommand::Cancel(crate::app::args::PlanRunIdArgs {
            plan_run_id: "plan-waiting".to_string(),
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("cancelled plan run plan-waiting"));
        stdout.lock().unwrap().clear();

        handle_plan_command(PlanCommand::List(crate::app::args::PlanListArgs {
            limit: 10,
        }))
        .await
        .unwrap();
        let output = String::from_utf8(stdout.lock().unwrap().clone()).unwrap();
        assert!(output.contains("plan-waiting"));
        assert!(output.contains("status=failed"));
    }
}
