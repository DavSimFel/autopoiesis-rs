//! Deny-first credential path policy.
//!
//! These lists intentionally bias toward blocking high-value secret material
//! rather than silently allowing a read or write to proceed.
//!
//! The shell and read-path heuristics both consume these catalogs, so the
//! module owns the shared deny list and normalization logic.
use std::path::Path;

const PROTECTED_PATH_FRAGMENTS: [&str; 8] = [
    "~/.autopoiesis/auth.json",
    "auth.json",
    ".env.",
    ".env",
    ".ssh/",
    "id_rsa",
    "id_ed25519",
    ".aws/credentials",
];

const PROTECTED_HOME_PATHS: [&str; 4] = [
    "HOME/.autopoiesis/auth.json",
    "HOME/.ssh/id_rsa",
    "HOME/.ssh/id_ed25519",
    "HOME/.aws/credentials",
];

const PROTECTED_ENV_FILENAMES: [&str; 7] = [
    ".env",
    ".env.local",
    ".env.production",
    ".env.production.local",
    ".env.development",
    ".env.development.local",
    ".env.test",
];

const PROTECTED_GIT_PATHS: [&str; 7] = [
    "auth.json",
    ".autopoiesis/auth.json",
    "id_rsa",
    "id_ed25519",
    ".ssh/id_rsa",
    ".ssh/id_ed25519",
    ".aws/credentials",
];

pub(crate) fn protected_path_fragments() -> &'static [&'static str] {
    &PROTECTED_PATH_FRAGMENTS
}

pub(crate) fn path_is_protected(path: impl AsRef<std::path::Path>) -> bool {
    let path = path.as_ref().to_string_lossy();
    is_protected_path_value(path.as_ref())
}

pub(crate) fn home_prefix() -> Option<String> {
    std::env::var_os("HOME")
        .and_then(|home| home.into_string().ok())
        .filter(|home| !home.is_empty())
}

pub(crate) fn strip_prefix_with_boundary<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

pub(crate) fn expand_home_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("${HOME}") {
        format!("HOME{rest}")
    } else if let Some(rest) = path.strip_prefix("$HOME") {
        format!("HOME{rest}")
    } else if let Some(home) = home_prefix() {
        if let Some(rest) = strip_prefix_with_boundary(path, &home) {
            format!("HOME{rest}")
        } else if let Some(rest) = path.strip_prefix('~') {
            format!("HOME{rest}")
        } else {
            path.to_string()
        }
    } else if let Some(rest) = path.strip_prefix('~') {
        format!("HOME{rest}")
    } else {
        path.to_string()
    }
}

pub(crate) fn normalize_lexical_path(path: &str) -> String {
    let path = expand_home_prefix(path.trim());
    let mut segments = Vec::new();
    let is_absolute = path.starts_with('/');

    for component in Path::new(&path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = segments.pop();
            }
            std::path::Component::RootDir => {}
            std::path::Component::Normal(part) => {
                segments.push(part.to_string_lossy().to_string());
            }
            std::path::Component::Prefix(_) => {}
        }
    }

    let mut normalized = String::new();
    if is_absolute {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    normalized
}

pub(crate) fn is_protected_env_filename(filename: &str) -> bool {
    PROTECTED_ENV_FILENAMES.contains(&filename)
}

pub(crate) fn is_protected_path_value(path: &str) -> bool {
    let normalized = normalize_lexical_path(path);
    let basename = Path::new(&normalized)
        .file_name()
        .and_then(|file_name| file_name.to_str());

    PROTECTED_HOME_PATHS
        .iter()
        .any(|protected| normalized == *protected)
        || basename.is_some_and(is_protected_env_filename)
}

pub(crate) fn is_protected_git_pathspec_value(path: &str) -> bool {
    let normalized = normalize_lexical_path(path);
    let basename = Path::new(&normalized)
        .file_name()
        .and_then(|file_name| file_name.to_str());

    PROTECTED_GIT_PATHS.contains(&normalized.as_str())
        || basename.is_some_and(is_protected_env_filename)
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;

    #[test]
    fn path_is_protected_matches_known_fragments() {
        assert!(path_is_protected("~/.autopoiesis/auth.json"));
        assert!(path_is_protected(".env"));
        assert!(path_is_protected(".env.local"));
        assert!(path_is_protected(".env.production.local"));
        assert!(path_is_protected("~/.ssh/id_rsa"));
        assert!(path_is_protected("~/.ssh/id_ed25519"));
        assert!(path_is_protected("~/.aws/credentials"));
        assert!(!path_is_protected("config/auth.json"));
        assert!(!path_is_protected(".env.example"));
    }

    #[test]
    fn normalize_lexical_path_handles_home_and_dot_segments() {
        assert_eq!(normalize_lexical_path("./a/../b"), "b");
        assert_eq!(
            normalize_lexical_path("$HOME/.ssh/../.ssh/id_rsa"),
            "HOME/.ssh/id_rsa"
        );
        assert_eq!(
            normalize_lexical_path("${HOME}/.aws/./credentials"),
            "HOME/.aws/credentials"
        );
    }

    #[test]
    fn path_is_protected_handles_home_variable_expansion() {
        assert!(path_is_protected("$HOME/.ssh/id_rsa"));
        assert!(path_is_protected("${HOME}/.aws/credentials"));
    }
}
