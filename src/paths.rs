use std::path::PathBuf;

pub const RUNTIME_ROOT_DIR: &str = ".aprs";
pub const DEFAULT_SESSIONS_DIR: &str = ".aprs/sessions";
pub const DEFAULT_QUEUE_DB_PATH: &str = ".aprs/queue.sqlite";
pub const DEFAULT_WORKSPACE_DIR: &str = ".aprs/workspace";

pub const SHIPPED_ROOT_DIR: &str = "src/shipped";
pub const DEFAULT_IDENTITY_TEMPLATES_DIR: &str = "src/shipped/identity-templates";
pub const DEFAULT_SKILLS_DIR: &str = "src/shipped/skills";

pub fn runtime_root_dir() -> PathBuf {
    PathBuf::from(RUNTIME_ROOT_DIR)
}

pub fn default_sessions_dir() -> PathBuf {
    PathBuf::from(DEFAULT_SESSIONS_DIR)
}

pub fn default_queue_db_path() -> PathBuf {
    PathBuf::from(DEFAULT_QUEUE_DB_PATH)
}

pub fn default_workspace_dir() -> PathBuf {
    PathBuf::from(DEFAULT_WORKSPACE_DIR)
}

pub fn default_identity_templates_dir() -> PathBuf {
    PathBuf::from(DEFAULT_IDENTITY_TEMPLATES_DIR)
}

pub fn default_skills_dir() -> PathBuf {
    PathBuf::from(DEFAULT_SKILLS_DIR)
}

pub fn default_auth_dir() -> PathBuf {
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home_dir).join(RUNTIME_ROOT_DIR)
}

pub fn default_auth_file_path() -> PathBuf {
    default_auth_dir().join("auth.json")
}
