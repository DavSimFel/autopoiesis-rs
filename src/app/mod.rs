pub mod args;
pub mod enqueue_command;
pub mod plan_commands;
pub mod session_run;
pub mod subscription_commands;
pub mod tracing;

#[cfg(all(test, not(clippy)))]
pub(crate) fn test_cwd_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::const_new(()))
}
