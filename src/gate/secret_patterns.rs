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
