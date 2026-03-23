pub(crate) const SECRET_PATTERN_COUNT: usize = 3;

pub(crate) const OPENAI_SECRET_PREFIX: &str = "sk-";
pub(crate) const OPENAI_SECRET_REGEX: &str = r"sk-[a-zA-Z0-9_-]{20,}";
pub(crate) const OPENAI_SECRET_MIN_SUFFIX_LEN: usize = 20;

pub(crate) const GITHUB_PAT_PREFIX: &str = "ghp_";
pub(crate) const GITHUB_PAT_REGEX: &str = r"ghp_[a-zA-Z0-9]{36}";
pub(crate) const GITHUB_PAT_SUFFIX_LEN: usize = 36;

pub(crate) const AWS_ACCESS_KEY_PREFIX: &str = "AKIA";
pub(crate) const AWS_ACCESS_KEY_REGEX: &str = r"AKIA[0-9A-Z]{16}";
pub(crate) const AWS_ACCESS_KEY_SUFFIX_LEN: usize = 16;

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

const READ_ONLY_GIT_SUBCOMMANDS: [&str; 4] = ["diff", "show", "grep", "cat-file"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretBodyKind {
    OpenAiToken,
    LowercaseAlphanumeric,
    UppercaseAlphanumeric,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretSuffixLen {
    Minimum(usize),
    Exact(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecretPattern {
    pub prefix: &'static str,
    pub regex: &'static str,
    pub body_kind: SecretBodyKind,
    pub suffix_len: SecretSuffixLen,
}

pub(crate) const SECRET_PATTERNS: [SecretPattern; SECRET_PATTERN_COUNT] = [
    SecretPattern {
        prefix: OPENAI_SECRET_PREFIX,
        regex: OPENAI_SECRET_REGEX,
        body_kind: SecretBodyKind::OpenAiToken,
        suffix_len: SecretSuffixLen::Minimum(OPENAI_SECRET_MIN_SUFFIX_LEN),
    },
    SecretPattern {
        prefix: GITHUB_PAT_PREFIX,
        regex: GITHUB_PAT_REGEX,
        body_kind: SecretBodyKind::LowercaseAlphanumeric,
        suffix_len: SecretSuffixLen::Exact(GITHUB_PAT_SUFFIX_LEN),
    },
    SecretPattern {
        prefix: AWS_ACCESS_KEY_PREFIX,
        regex: AWS_ACCESS_KEY_REGEX,
        body_kind: SecretBodyKind::UppercaseAlphanumeric,
        suffix_len: SecretSuffixLen::Exact(AWS_ACCESS_KEY_SUFFIX_LEN),
    },
];

pub(crate) fn protected_path_fragments() -> &'static [&'static str] {
    &PROTECTED_PATH_FRAGMENTS
}

pub(crate) fn command_references_protected_path(command: &str) -> bool {
    let command = command.to_lowercase();
    protected_path_fragments()
        .iter()
        .any(|fragment| command.contains(fragment))
}

fn home_prefix() -> Option<String> {
    std::env::var_os("HOME")
        .and_then(|home| home.into_string().ok())
        .filter(|home| !home.is_empty())
}

fn strip_prefix_with_boundary<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    path.strip_prefix(prefix)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn expand_home_prefix(path: &str) -> String {
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

fn is_env_assignment_token(token: &str) -> bool {
    let Some((key, _value)) = token.split_once('=') else {
        return false;
    };

    !key.is_empty()
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn strip_leading_env_assignments(argv: &[String]) -> &[String] {
    let mut start = 0usize;

    while let Some(token) = argv.get(start) {
        if is_env_assignment_token(token) {
            start += 1;
            continue;
        }
        break;
    }

    &argv[start..]
}

fn strip_env_wrapper(argv: &[String]) -> &[String] {
    let argv = strip_leading_env_assignments(argv);
    let Some(first) = argv.first() else {
        return argv;
    };

    if first != "env" {
        return argv;
    }

    let mut start = 1usize;
    while let Some(token) = argv.get(start) {
        if is_env_assignment_token(token) {
            start += 1;
            continue;
        }
        if token == "--" {
            start += 1;
            break;
        }
        if token == "-S"
            || token == "--split-string"
            || token == "-u"
            || token == "--unset"
            || token == "-C"
            || token == "--chdir"
        {
            start += 2;
            continue;
        }
        if token.starts_with("-u") && token.len() > 2 {
            start += 1;
            continue;
        }
        if token.starts_with("-C") && token.len() > 2 {
            start += 1;
            continue;
        }
        if token.starts_with("--unset=") || token.starts_with("--chdir=") {
            start += 1;
            continue;
        }
        if token.starts_with('-') {
            start += 1;
            continue;
        }
        break;
    }

    &argv[start..]
}

fn normalize_lexical_path(path: &str) -> String {
    let path = expand_home_prefix(path.trim());
    let mut segments = Vec::new();
    let is_absolute = path.starts_with('/');

    for component in std::path::Path::new(&path).components() {
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

fn env_split_string_command(argv: &[String]) -> Option<Vec<String>> {
    let argv = strip_leading_env_assignments(argv);
    let first = argv.first()?;

    if first != "env" {
        return None;
    }

    let mut index = 1usize;
    while let Some(token) = argv.get(index) {
        match token.as_str() {
            "--" => return None,
            "-S" | "--split-string" => {
                let value = argv.get(index + 1)?;
                return shell_words::split(value).ok();
            }
            "-u" | "--unset" | "-C" | "--chdir" => {
                index += 2;
            }
            _ if token.starts_with("--unset=") || token.starts_with("--chdir=") => {
                index += 1;
            }
            _ if token.starts_with("-u") && token.len() > 2 => {
                index += 1;
            }
            _ if token.starts_with("-C") && token.len() > 2 => {
                index += 1;
            }
            _ if token.starts_with('-') => {
                index += 1;
            }
            _ => return None,
        }
    }

    None
}

fn is_protected_env_filename(filename: &str) -> bool {
    PROTECTED_ENV_FILENAMES.contains(&filename)
}

fn is_protected_path_value(path: &str) -> bool {
    let normalized = normalize_lexical_path(path);
    let basename = std::path::Path::new(&normalized)
        .file_name()
        .and_then(|file_name| file_name.to_str());

    PROTECTED_HOME_PATHS
        .iter()
        .any(|protected| normalized == *protected)
        || basename.is_some_and(is_protected_env_filename)
}

fn is_protected_git_pathspec_value(path: &str) -> bool {
    let normalized = normalize_lexical_path(path);
    let basename = std::path::Path::new(&normalized)
        .file_name()
        .and_then(|file_name| file_name.to_str());

    PROTECTED_GIT_PATHS.contains(&normalized.as_str())
        || basename.is_some_and(is_protected_env_filename)
}

fn git_path_spec_argument(token: &str) -> Option<&str> {
    token.split_once(':').map(|(_, path)| path)
}

fn git_option_value_references_protected_path(argv: &[String]) -> bool {
    let mut index = 1usize;

    while index < argv.len() {
        let token = &argv[index];
        if !token.starts_with('-') {
            break;
        }

        if matches!(
            token.as_str(),
            "-c" | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--exec-path"
                | "--config-env"
                | "-C"
        ) {
            if let Some(value) = argv.get(index + 1)
                && git_config_value_references_protected_path(value)
            {
                return true;
            }
            index += 1;
        }

        index += 1;
    }

    false
}

fn git_config_value_references_protected_path(value: &str) -> bool {
    let Some((key, command)) = value.split_once('=') else {
        return false;
    };

    if !git_config_key_executes_shell_command(key) {
        return false;
    }

    let command = command.trim_start_matches('!').trim();
    if let Ok(argv) = shell_words::split(command) {
        simple_command_reads_protected_path(&argv)
    } else {
        command_references_protected_path(command)
    }
}

fn git_config_key_executes_shell_command(key: &str) -> bool {
    matches!(key, k if k.starts_with("alias."))
        || matches!(
            key,
            "core.pager" | "core.sshCommand" | "diff.external" | "pager"
        )
        || key.starts_with("pager.")
        || (key.starts_with("difftool.") && key.ends_with(".cmd"))
        || (key.starts_with("mergetool.") && key.ends_with(".cmd"))
        || (key.starts_with("filter.") && (key.ends_with(".clean") || key.ends_with(".smudge")))
}

fn git_subcommand_and_args(argv: &[String]) -> Option<(&str, &[String])> {
    let mut index = 1usize;
    while index < argv.len() {
        let token = argv.get(index)?;
        if !token.starts_with('-') {
            return Some((token.as_str(), &argv[index + 1..]));
        }

        if matches!(
            token.as_str(),
            "-c" | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--exec-path"
                | "--config-env"
                | "-C"
        ) {
            index += 1;
        }
        index += 1;
    }

    None
}

fn command_argument_references_protected_path(argument: &str) -> bool {
    if let Some(candidate) = git_path_spec_argument(argument) {
        is_protected_path_value(candidate) || is_protected_git_pathspec_value(candidate)
    } else {
        is_protected_path_value(argument)
    }
}

fn grep_file_operands_refer_protected_path(args: &[String]) -> bool {
    let mut options_done = false;
    let mut pattern_specified = false;
    let mut index = 0usize;

    while let Some(argument) = args.get(index) {
        if argument == "--" {
            options_done = true;
            index += 1;
            continue;
        }

        if !options_done && argument.starts_with('-') && argument != "-" {
            if argument == "-e" {
                pattern_specified = true;
                if args.get(index + 1).is_some() {
                    index += 2;
                    continue;
                }
            } else if let Some(value) = argument.strip_prefix("-e")
                && !value.is_empty()
            {
                pattern_specified = true;
                index += 1;
                continue;
            }

            if argument == "-f" {
                pattern_specified = true;
                if let Some(value) = args.get(index + 1) {
                    if command_argument_references_protected_path(value) {
                        return true;
                    }
                    index += 2;
                    continue;
                }
            } else if let Some(value) = argument.strip_prefix("-f") {
                if !value.is_empty() {
                    pattern_specified = true;
                    if command_argument_references_protected_path(value) {
                        return true;
                    }
                    index += 1;
                    continue;
                }
            } else if let Some(value) = argument.strip_prefix("--file=") {
                pattern_specified = true;
                if command_argument_references_protected_path(value) {
                    return true;
                }
                index += 1;
                continue;
            } else if argument == "--file" {
                pattern_specified = true;
                if let Some(value) = args.get(index + 1) {
                    if command_argument_references_protected_path(value) {
                        return true;
                    }
                    index += 2;
                    continue;
                }
            }

            index += 1;
            continue;
        }

        if !pattern_specified {
            pattern_specified = true;
            index += 1;
            continue;
        }

        if command_argument_references_protected_path(argument) {
            return true;
        }

        index += 1;
    }

    false
}

pub(crate) fn simple_command_reads_protected_path(argv: &[String]) -> bool {
    if let Some(expanded) = env_split_string_command(argv)
        && simple_command_reads_protected_path(&expanded)
    {
        return true;
    }

    let argv = strip_env_wrapper(argv);
    let Some((program, args)) = argv.split_first() else {
        return false;
    };

    let program = program.to_lowercase();

    match program.as_str() {
        "cat" | "head" | "tail" | "sed" | "awk" => args
            .iter()
            .any(|argument| command_argument_references_protected_path(argument)),
        "grep" => grep_file_operands_refer_protected_path(args),
        "git" => {
            if git_option_value_references_protected_path(argv) {
                return true;
            }

            let Some((subcommand, sub_args)) = git_subcommand_and_args(argv) else {
                return false;
            };

            if !READ_ONLY_GIT_SUBCOMMANDS.contains(&subcommand) {
                return false;
            }

            if subcommand == "grep" {
                return grep_file_operands_refer_protected_path(sub_args);
            }

            sub_args
                .iter()
                .filter(|argument| !argument.starts_with('-'))
                .any(|argument| command_argument_references_protected_path(argument))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_references_protected_path_matches_known_fragments() {
        assert!(command_references_protected_path(
            "cat ~/.autopoiesis/auth.json"
        ));
        assert!(command_references_protected_path("echo .env.production"));
        assert!(command_references_protected_path("cat ~/.ssh/id_rsa"));
        assert!(!command_references_protected_path("printf '%s' hello"));
    }

    #[test]
    fn simple_command_reads_protected_path_matches_readers() {
        assert!(simple_command_reads_protected_path(&[
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        if let Ok(home) = std::env::var("HOME") {
            assert!(simple_command_reads_protected_path(&[
                "cat".to_string(),
                format!("{home}/.autopoiesis/auth.json"),
            ]));
        }
        assert!(simple_command_reads_protected_path(&[
            "sed".to_string(),
            "-n".to_string(),
            "1,5p".to_string(),
            "~/.ssh/id_rsa".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "diff".to_string(),
            "--no-index".to_string(),
            "/dev/null".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "--no-pager".to_string(),
            "show".to_string(),
            "HEAD:.env.production.local".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "show".to_string(),
            "HEAD:.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "show".to_string(),
            "HEAD:.ssh/id_rsa".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "show".to_string(),
            "HEAD:.aws/credentials".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "color.ui=always".to_string(),
            "grep".to_string(),
            ".".to_string(),
            "main:path/to/.env.local".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "alias.show=!cat ~/.autopoiesis/auth.json".to_string(),
            "show".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "FOO=1".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "FOO=1".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "-u".to_string(),
            "HOME".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "-C".to_string(),
            "/tmp".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "--unset".to_string(),
            "HOME".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "--chdir".to_string(),
            "/tmp".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "-S".to_string(),
            "cat ~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "--split-string".to_string(),
            "cat ~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "-i".to_string(),
            "FOO=1".to_string(),
            "cat".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "GIT_PAGER=cat".to_string(),
            "git".to_string(),
            "show".to_string(),
            "HEAD:.env.production.local".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "GIT_PAGER=cat".to_string(),
            "git".to_string(),
            "show".to_string(),
            "HEAD:.env.production.local".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "env".to_string(),
            "-i".to_string(),
            "GIT_PAGER=cat".to_string(),
            "git".to_string(),
            "show".to_string(),
            "HEAD:.env.production.local".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "grep".to_string(),
            "-f".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
            "README.md".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "grep".to_string(),
            "-f~/.autopoiesis/auth.json".to_string(),
            "README.md".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "grep".to_string(),
            "-f".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
            "README.md".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "grep".to_string(),
            "-f~/.autopoiesis/auth.json".to_string(),
            "README.md".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "printf".to_string(),
            "%s".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "status".to_string(),
            "~/.autopoiesis/auth.json".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "cat".to_string(),
            "config/auth.json".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "cat".to_string(),
            ".env.example".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "show".to_string(),
            "HEAD:.env.example".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "show".to_string(),
            "HEAD:config/auth.json".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "grep".to_string(),
            ".env".to_string(),
            "README.md".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "grep".to_string(),
            ".env".to_string(),
            "README.md".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "include.path=config/auth.json".to_string(),
            "show".to_string(),
            "HEAD:README.md".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "core.attributesfile=.env.example".to_string(),
            "show".to_string(),
            "HEAD:README.md".to_string(),
        ]));
        assert!(simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "core.pager=cat ~/.autopoiesis/auth.json".to_string(),
            "show".to_string(),
            "HEAD:README.md".to_string(),
        ]));
        assert!(!simple_command_reads_protected_path(&[
            "git".to_string(),
            "-c".to_string(),
            "core.pager=cat config/auth.json".to_string(),
            "show".to_string(),
            "HEAD:README.md".to_string(),
        ]));
    }
}
