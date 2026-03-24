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

pub(crate) fn command_writes_identity_template_path(command: &str) -> bool {
    let command = command.to_lowercase();
    if command.contains("perl -") && identity_template_perl_script_contains_write_api(&command) {
        return true;
    }
    if identity_template_raw_script_writes_path(&command) {
        return true;
    }
    let Ok(tokens) = shell_words::split(&command) else {
        return false;
    };
    if tokens.is_empty() {
        return false;
    }

    let tokens = strip_env_wrapper(&tokens).to_vec();
    if tokens.is_empty() {
        return false;
    }

    identity_template_script_writes_path(&tokens, 0)
}

fn identity_template_script_writes_path(args: &[String], depth: usize) -> bool {
    if depth > 4 {
        return false;
    }

    let args = identity_template_strip_env_wrapper(args);
    if args.is_empty() {
        return false;
    }

    if args.len() == 1
        && let Ok(inner_args) = shell_words::split(&args[0])
        && inner_args.len() > 1
        && identity_template_script_writes_path(&inner_args, depth + 1)
    {
        return true;
    }

    if identity_template_args_write_redirection(args) {
        return true;
    }

    if let Some(script) = identity_template_shell_wrapper_script(args) {
        if identity_template_raw_script_writes_path(script)
            || identity_template_wrapper_script_writes(script)
        {
            return true;
        }

        if let Ok(inner_args) = shell_words::split(script)
            && identity_template_script_writes_path(&inner_args, depth + 1)
        {
            return true;
        }
    }

    identity_template_direct_write_command(args)
}

fn identity_template_strip_env_wrapper(args: &[String]) -> &[String] {
    let Some(first) = args.first() else {
        return args;
    };

    if first.as_str() != "env" {
        return args;
    }

    let mut index = 1;
    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return args.get(index + 1..).unwrap_or(&[]);
        }

        if arg.starts_with('-') || arg.contains('=') {
            index += 1;
            continue;
        }

        return &args[index..];
    }

    &[]
}

fn identity_template_shell_wrapper_script(args: &[String]) -> Option<&str> {
    let shell = args.first()?.as_str();
    let shell = match shell {
        "busybox" => match args.get(1).map(String::as_str) {
            Some("sh") => "sh",
            _ => return None,
        },
        other => other,
    };

    if !matches!(shell, "bash" | "sh" | "zsh" | "dash" | "ksh") {
        return None;
    }

    let mut index = if shell == "sh" && args.first().map(String::as_str) == Some("busybox") {
        2
    } else {
        1
    };

    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "-c" | "-lc" | "-cl" => return args.get(index + 1).map(String::as_str),
            _ if identity_template_shell_inline_script_flag(arg) => {
                return args.get(index + 1).map(String::as_str);
            }
            "--rcfile" | "--init-file" | "-O" | "-o" => {
                index += 2;
            }
            "--" => return None,
            _ if arg.starts_with('-') => {
                index += 1;
            }
            _ => break,
        }
    }

    None
}

fn identity_template_args_write_redirection(args: &[String]) -> bool {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        if identity_template_redirection_token_writes_target(arg, args.get(index + 1)) {
            return true;
        }

        index += 1;
    }

    false
}

fn identity_template_redirection_token_writes_target(token: &str, next: Option<&String>) -> bool {
    let mut stripped = token.trim_start_matches(|ch: char| ch.is_ascii_digit());
    if let Some(rest) = stripped.strip_prefix('&') {
        stripped = rest;
    }

    let Some(rest) = stripped
        .strip_prefix(">>")
        .or_else(|| stripped.strip_prefix('>'))
    else {
        return false;
    };

    let target = if rest.is_empty() {
        next.map(String::as_str)
    } else if rest.starts_with(char::is_whitespace) {
        return false;
    } else {
        Some(rest)
    };
    target.is_some_and(identity_template_mentions_target)
}

fn identity_template_direct_write_command(args: &[String]) -> bool {
    let Some(command) = args.first().map(String::as_str) else {
        return false;
    };
    let command = command_basename(command);

    match command {
        "touch" | "rm" | "rmdir" | "tee" | "chmod" | "chown" => args
            .iter()
            .skip(1)
            .any(|arg| identity_template_mentions_target(arg)),
        "cp" | "install" | "ln" => identity_template_destination_argument(args)
            .is_some_and(identity_template_mentions_target),
        "dd" => args.iter().any(|arg| {
            arg.strip_prefix("of=")
                .is_some_and(identity_template_mentions_target)
        }),
        "mv" => args
            .iter()
            .any(|arg| identity_template_mentions_target(arg)),
        "sed" => {
            args.iter().any(|arg| arg == "-i" || arg.starts_with("-i"))
                && args
                    .iter()
                    .any(|arg| identity_template_mentions_target(arg))
        }
        "git" => {
            matches!(
                identity_template_git_subcommand(args),
                Some("checkout" | "restore")
            ) && args
                .iter()
                .any(|arg| identity_template_mentions_target(arg))
        }
        "perl" => {
            let mut saw_code_flag = false;
            for arg in args.iter().skip(1) {
                if matches!(arg.as_str(), "-e" | "-i" | "-p" | "-pe" | "-pi") {
                    saw_code_flag = true;
                    continue;
                }

                if saw_code_flag && identity_template_perl_script_contains_write_api(arg) {
                    return true;
                }
            }
            false
        }
        "python" | "python3" | "python3.11" => {
            let mut saw_code_flag = false;
            for arg in args.iter().skip(1) {
                if matches!(arg.as_str(), "-c") {
                    saw_code_flag = true;
                    continue;
                }

                if saw_code_flag && identity_template_script_contains_write_api(arg) {
                    return true;
                }
            }
            false
        }
        "ruby" => {
            let mut saw_code_flag = false;
            for arg in args.iter().skip(1) {
                if matches!(arg.as_str(), "-e") {
                    saw_code_flag = true;
                    continue;
                }

                if saw_code_flag && identity_template_script_contains_write_api(arg) {
                    return true;
                }
            }
            false
        }
        "node" | "nodejs" => {
            let mut saw_code_flag = false;
            for arg in args.iter().skip(1) {
                if matches!(arg.as_str(), "-e" | "-p" | "--eval" | "--print") {
                    saw_code_flag = true;
                    continue;
                }

                if saw_code_flag && identity_template_script_contains_write_api(arg) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn command_basename(command: &str) -> &str {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

fn identity_template_mentions_target(value: &str) -> bool {
    value.contains("identity-templates/") || value == "identity-templates"
}

fn identity_template_destination_argument(args: &[String]) -> Option<&str> {
    let mut index = 1;

    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "-t" | "--target-directory" => {
                return args.get(index + 1).map(String::as_str);
            }
            _ if arg.starts_with("--target-directory=") => {
                return arg.split_once('=').map(|(_, value)| value);
            }
            _ => index += 1,
        }
    }

    args.last().map(String::as_str)
}

fn identity_template_git_subcommand(args: &[String]) -> Option<&str> {
    let mut index = 1;

    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--" => return None,
            "-C" | "-c" => {
                index += 2;
            }
            _ if arg.starts_with("-C") && arg.len() > 2 => {
                index += 1;
            }
            _ if arg.starts_with("-c") && arg.len() > 2 => {
                index += 1;
            }
            _ if arg.starts_with("--git-dir=")
                || arg.starts_with("--work-tree=")
                || arg.starts_with("--namespace=")
                || arg.starts_with("--super-prefix=") =>
            {
                index += 1;
            }
            _ if arg.starts_with('-') => {
                index += 1;
            }
            other => return Some(other),
        }
    }

    None
}

fn identity_template_raw_script_writes_path(script: &str) -> bool {
    identity_template_unquoted_redirection_writes_target(script)
}

fn identity_template_script_contains_write_api(script: &str) -> bool {
    if !identity_template_mentions_target(script) {
        return false;
    }

    for segment in script.split([';', '\n']) {
        if !identity_template_mentions_target(segment) {
            continue;
        }

        let sanitized = strip_quoted_literals(segment);
        if sanitized.contains("write_text(")
            || sanitized.contains("write_bytes(")
            || sanitized.contains("writefilesync(")
            || sanitized.contains("appendfilesync(")
            || sanitized.contains("writefile(")
            || sanitized.contains("appendfile(")
            || sanitized.contains(".touch(")
            || sanitized.contains(".rename(")
            || sanitized.contains(".unlink(")
            || sanitized.contains("os.remove(")
            || sanitized.contains("os.unlink(")
            || sanitized.contains("path(") && sanitized.contains(".unlink(")
            || sanitized.contains("path(") && sanitized.contains(".write_text(")
            || sanitized.contains("path(") && sanitized.contains(".write_bytes(")
            || identity_template_open_mode_write(segment)
        {
            return true;
        }
    }

    false
}

fn identity_template_perl_script_contains_write_api(script: &str) -> bool {
    if !identity_template_mentions_target(script) {
        return false;
    }

    let code = identity_template_perl_code_fragment(script).unwrap_or(script);
    let code = strip_outer_shell_quotes(code);
    let sanitized = strip_quoted_literals(code);
    sanitized.contains("unlink")
        || sanitized.contains("rename")
        || identity_template_open_mode_write(code)
}

fn identity_template_perl_code_fragment(script: &str) -> Option<&str> {
    for flag in ["-pe", "-pi", "-e", "-p", "-i"] {
        let Some(index) = find_unquoted_substring(script, flag) else {
            continue;
        };

        let fragment = script[index + flag.len()..].trim_start();
        if !fragment.is_empty() {
            return Some(fragment);
        }
    }

    None
}

fn strip_outer_shell_quotes(value: &str) -> &str {
    let value = value.trim();
    if let Some(rest) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return rest;
    }

    if let Some(rest) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return rest;
    }

    value
}

fn identity_template_open_mode_write(script: &str) -> bool {
    if !identity_template_mentions_target(script) {
        return false;
    }

    let Some(open_index) = find_unquoted_substring(script, "open(") else {
        return false;
    };
    let tail = &script[open_index + "open(".len()..];
    let Some(close_index) = identity_template_matching_paren(tail) else {
        return false;
    };
    let invocation = tail[..close_index].trim();
    let statement_start = script
        .get(..open_index)
        .and_then(|prefix| prefix.rfind([';', '\n']))
        .map_or(0, |index| index + 1);
    let statement = &script[statement_start..];
    if !identity_template_mentions_target(invocation)
        && !identity_template_mentions_target(statement)
    {
        return false;
    }
    let mode_segment = if let Some(comma_index) = invocation.find(',') {
        invocation[comma_index + 1..].trim_start()
    } else {
        invocation
    };
    let mode_segment = mode_segment
        .strip_prefix("mode=")
        .map_or(mode_segment, |rest| rest.trim_start());
    let Some(quote) = mode_segment.chars().next() else {
        return false;
    };
    if !matches!(quote, '\'' | '"') {
        return false;
    }

    let Some(end_quote) = mode_segment[1..].find(quote) else {
        return false;
    };
    let mode = &mode_segment[1..1 + end_quote];
    identity_template_write_mode(mode)
}

fn identity_template_write_mode(mode: &str) -> bool {
    let mode = mode.trim();
    !mode.is_empty()
        && mode
            .chars()
            .all(|ch| matches!(ch, 'w' | 'a' | 'x' | 'r' | 'b' | 't' | '+' | 'U'))
        && mode.chars().any(|ch| matches!(ch, 'w' | 'a' | 'x' | '+'))
}

fn identity_template_matching_paren(script: &str) -> Option<usize> {
    let bytes = script.as_bytes();
    let mut index = 0;
    let mut depth = 1;
    let mut single_quoted = false;
    let mut double_quoted = false;

    while index < bytes.len() {
        let ch = bytes[index] as char;
        if single_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '\'' {
                single_quoted = false;
            }
            index += 1;
            continue;
        }

        if double_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '"' {
                double_quoted = false;
            }
            index += 1;
            continue;
        }

        match ch {
            '\'' => single_quoted = true,
            '"' => double_quoted = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            '\\' => {
                index += 1;
            }
            _ => {}
        }

        index += 1;
    }

    None
}

fn identity_template_shell_inline_script_flag(arg: &str) -> bool {
    if arg == "-c" || arg == "-lc" || arg == "-cl" {
        return true;
    }

    if !arg.starts_with('-') || arg.starts_with("--") {
        return false;
    }

    let mut saw_c = false;
    for ch in arg.chars().skip(1) {
        match ch {
            'c' => saw_c = true,
            'i' | 'l' | 'e' | 'x' | 'u' | 'v' | 'p' | 's' | 'r' | 'o' | 'h' | 'f' | 'n' | 'k'
            | 'a' | 'B' | 'H' | 'P' | 'T' => {}
            _ => return false,
        }
    }

    saw_c
}

fn identity_template_wrapper_script_writes(script: &str) -> bool {
    let mentions_target = identity_template_mentions_target(script);
    let sanitized = strip_quoted_literals(script);
    mentions_target
        && (sanitized.contains("touch identity-templates/")
            || sanitized.contains("perl -i")
            || sanitized.contains("perl -pi")
            || identity_template_perl_shell_payload_writes(script)
            || sanitized.contains("python -c")
                && identity_template_script_contains_write_api(script)
            || sanitized.contains("os.remove(")
            || sanitized.contains("os.unlink(")
            || sanitized.contains("write_text(")
            || sanitized.contains("write_bytes(")
            || sanitized.contains("sed -i")
            || sanitized.contains("git checkout")
            || sanitized.contains("git restore"))
}

fn identity_template_perl_shell_payload_writes(script: &str) -> bool {
    let script = script.trim();
    let Some((_, payload)) = script.split_once("perl ") else {
        return false;
    };

    let payload = payload.trim_start();
    let mut parts = payload.splitn(2, char::is_whitespace);
    let flag = parts.next().unwrap_or("");
    let payload = parts.next().unwrap_or("");

    match flag {
        "-e" | "-p" | "-pe" => {
            let payload = payload.trim_start();
            let payload = payload
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .or_else(|| {
                    payload
                        .strip_prefix('"')
                        .and_then(|rest| rest.strip_suffix('"'))
                })
                .unwrap_or(payload);
            identity_template_perl_script_contains_write_api(payload)
        }
        "-i" | "-pi" => true,
        _ => false,
    }
}

#[cfg(test)]
mod env_wrapper_regression_tests {
    use super::command_writes_identity_template_path;

    #[test]
    fn env_shell_split_payload_is_reparsed() {
        assert!(command_writes_identity_template_path(
            "env -S 'bash -c \"touch identity-templates/context.md\"'"
        ));
    }

    #[test]
    fn open_mode_detection_requires_target_in_invocation() {
        assert!(!command_writes_identity_template_path(
            "python -c \"open('README.md', 'w'); print('identity-templates/context.md')\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').open('w').close()\""
        ));
    }
}

fn identity_template_unquoted_redirection_writes_target(script: &str) -> bool {
    let bytes = script.as_bytes();
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;

    while index < bytes.len() {
        let ch = bytes[index] as char;
        if single_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '\'' {
                single_quoted = false;
            }
            index += 1;
            continue;
        }

        if double_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '"' {
                double_quoted = false;
            }
            index += 1;
            continue;
        }

        match ch {
            '\'' => single_quoted = true,
            '"' => double_quoted = true,
            '\\' => {
                index += 1;
            }
            '>' => {
                let mut target_index = index + 1;
                if target_index < bytes.len() && bytes[target_index] as char == '>' {
                    target_index += 1;
                }
                while target_index < bytes.len() && bytes[target_index].is_ascii_whitespace() {
                    target_index += 1;
                }
                if target_index < bytes.len() && bytes[target_index] as char != '&' {
                    let mut end = target_index;
                    while end < bytes.len() {
                        let end_ch = bytes[end] as char;
                        if end_ch.is_ascii_whitespace() || matches!(end_ch, '|' | '&' | ';' | ')') {
                            break;
                        }
                        end += 1;
                    }
                    if target_index < end
                        && identity_template_mentions_target(&script[target_index..end])
                    {
                        return true;
                    }
                }
            }
            _ => {}
        }

        index += 1;
    }

    false
}

fn find_unquoted_substring(script: &str, needle: &str) -> Option<usize> {
    let bytes = script.as_bytes();
    let needle = needle.as_bytes();
    let mut index = 0;
    let mut single_quoted = false;
    let mut double_quoted = false;

    while index + needle.len() <= bytes.len() {
        let ch = bytes[index] as char;
        if single_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '\'' {
                single_quoted = false;
            }
            index += 1;
            continue;
        }

        if double_quoted {
            if ch == '\\' {
                index += 1;
            } else if ch == '"' {
                double_quoted = false;
            }
            index += 1;
            continue;
        }

        match ch {
            '\'' => single_quoted = true,
            '"' => double_quoted = true,
            '\\' => {
                index += 1;
            }
            _ if bytes[index..].starts_with(needle) => return Some(index),
            _ => {}
        }

        index += 1;
    }

    None
}

fn strip_quoted_literals(script: &str) -> String {
    let mut output = String::with_capacity(script.len());
    let mut chars = script.chars().peekable();
    let mut single_quoted = false;
    let mut double_quoted = false;

    while let Some(ch) = chars.next() {
        if single_quoted {
            if ch == '\\' {
                chars.next();
            } else if ch == '\'' {
                single_quoted = false;
            }
            output.push(' ');
            continue;
        }

        if double_quoted {
            if ch == '\\' {
                chars.next();
            } else if ch == '"' {
                double_quoted = false;
            }
            output.push(' ');
            continue;
        }

        match ch {
            '\'' => {
                single_quoted = true;
                output.push(' ');
            }
            '"' => {
                double_quoted = true;
                output.push(' ');
            }
            _ => output.push(ch),
        }
    }

    output
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

    #[test]
    fn identity_template_write_detection_requires_write_target() {
        assert!(command_writes_identity_template_path(
            "rm -rf identity-templates"
        ));
        assert!(command_writes_identity_template_path(
            "/bin/touch identity-templates/context.md"
        ));
        assert!(command_writes_identity_template_path(
            "/usr/bin/python -c \"from pathlib import Path; Path('identity-templates/context.md').touch()\""
        ));
        assert!(command_writes_identity_template_path(
            "mv identity-templates /tmp/x"
        ));
        assert!(command_writes_identity_template_path(
            "dd if=/tmp/x of=identity-templates/context.md"
        ));
        assert!(command_writes_identity_template_path(
            "env -i touch identity-templates/context.md"
        ));
        assert!(command_writes_identity_template_path(
            "bash -c \"touch identity-templates/context.md\""
        ));
        assert!(command_writes_identity_template_path(
            "bash --rcfile /tmp/rc -c \"touch identity-templates/context.md\""
        ));
        assert!(command_writes_identity_template_path(
            "bash -O nullglob -c \"rm -rf identity-templates\""
        ));
        assert!(command_writes_identity_template_path(
            "sh -c \"rm -rf identity-templates\""
        ));
        assert!(command_writes_identity_template_path(
            "bash -c \"git restore -- identity-templates/context.md\""
        ));
        assert!(command_writes_identity_template_path(
            "bash -c \"mv identity-templates/context.md /tmp/x\""
        ));
        assert!(command_writes_identity_template_path(
            "bash -ec \"touch identity-templates/context.md\""
        ));
        assert!(command_writes_identity_template_path(
            "printf hi > identity-templates/constitution.md"
        ));
        assert!(command_writes_identity_template_path(
            "printf hi >identity-templates/constitution.md"
        ));
        assert!(command_writes_identity_template_path(
            "bash -c \"cat > identity-templates/context.md\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').touch()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').write_text('x')\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"open('identity-templates/context.md', 'w').close()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').open('w').close()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"open('identity-templates/context.md', 'x').close()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').open(mode='a').close()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"open('identity-templates/context.md', 'w+').close()\""
        ));
        assert!(command_writes_identity_template_path(
            "python -c \"open('identity-templates/context.md', 'wb').close()\""
        ));
        assert!(!command_writes_identity_template_path(
            "python -c \"from pathlib import Path; Path('identity-templates/context.md').open('r').read()\""
        ));
        assert!(command_writes_identity_template_path(
            "perl -e \"unlink 'identity-templates/context.md'\""
        ));
        assert!(command_writes_identity_template_path(
            "bash -c \"perl -e 'unlink \\'identity-templates/context.md\\''\""
        ));
        assert!(command_writes_identity_template_path(
            "node -e \"require('fs').writeFileSync('identity-templates/context.md', 'x')\""
        ));
        assert!(command_writes_identity_template_path(
            "node --eval \"require('fs').writeFileSync('identity-templates/context.md', 'x')\""
        ));
        assert!(command_writes_identity_template_path(
            "tee identity-templates/context.md"
        ));
        assert!(command_writes_identity_template_path(
            "cp /tmp/x identity-templates/agents/silas/agent.md"
        ));
        assert!(command_writes_identity_template_path(
            "cp /tmp/x identity-templates"
        ));
        assert!(command_writes_identity_template_path(
            "cp -t identity-templates /tmp/x"
        ));
        assert!(command_writes_identity_template_path(
            "install --target-directory=identity-templates /tmp/x"
        ));
        assert!(command_writes_identity_template_path(
            "ln --target-directory identity-templates /tmp/x"
        ));
        assert!(!command_writes_identity_template_path(
            "cp identity-templates/context.md /tmp/x"
        ));
        assert!(!command_writes_identity_template_path(
            "cat identity-templates/context.md"
        ));
        assert!(!command_writes_identity_template_path(
            "python -c \"print('>identity-templates/context.md')\""
        ));
        assert!(!command_writes_identity_template_path(
            "python -c \"print(\\\"open('identity-templates/context.md', 'w')\\\")\""
        ));
        assert!(!command_writes_identity_template_path(
            "python -c \"import sys; sys.stdout.write('identity-templates/context.md')\""
        ));
        assert!(!command_writes_identity_template_path(
            "bash -c \"printf '> identity-templates/context.md\\n'\""
        ));
        assert!(!command_writes_identity_template_path(
            "bash --norc /tmp/script.sh"
        ));
        assert!(!command_writes_identity_template_path(
            "bash --rcfile /tmp/rc /tmp/script.sh"
        ));
        assert!(!command_writes_identity_template_path(
            "printf '> identity-templates/context.md\\n'"
        ));
        assert!(command_writes_identity_template_path(
            "node -p \"require('fs').writeFileSync('identity-templates/context.md', 'x')\""
        ));
        assert!(command_writes_identity_template_path(
            "git -C . restore -- identity-templates/context.md"
        ));
        assert!(!command_writes_identity_template_path(
            "git -C . restore -- README.md"
        ));
    }
}

#[cfg(test)]
mod identity_template_guard_tests {
    use super::command_writes_identity_template_path;

    #[test]
    fn denies_wrapped_perl_inplace_edit() {
        assert!(command_writes_identity_template_path(
            r#"bash -c "perl -pi -e 's/foo/bar/' identity-templates/context.md""#,
        ));
    }

    #[test]
    fn denies_wrapped_python_remove() {
        assert!(command_writes_identity_template_path(
            r#"sh -c "python -c 'import os; os.remove(\"identity-templates/context.md\")'""#,
        ));
    }

    #[test]
    fn allows_copying_identity_template_outside_the_tree() {
        assert!(!command_writes_identity_template_path(
            r#"bash -c "cp identity-templates/context.md /tmp/x""#,
        ));
    }

    #[test]
    fn allows_printf_that_mentions_identity_template_text() {
        assert!(!command_writes_identity_template_path(
            r#"bash -c "printf '> identity-templates/context.md\n'""#,
        ));
    }
}
