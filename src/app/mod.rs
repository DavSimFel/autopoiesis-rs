pub mod args;
pub mod enqueue_command;
pub mod plan_commands;
pub mod session_run;
pub mod subscription_commands;
pub mod tracing;

#[cfg(test)]
pub(crate) fn test_cwd_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}
