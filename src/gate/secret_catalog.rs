/// Static secret prefixes, regexes, and streaming-redaction metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretBodyKind {
    OpenAiToken,
    LowercaseAlphanumeric,
    UppercaseAlphanumeric,
}

/// Secret suffix length expectations used by redaction heuristics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SecretSuffixLen {
    Minimum(usize),
    Exact(usize),
}

/// Catalog entry for a secret pattern family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SecretPattern {
    pub prefix: &'static str,
    pub regex: &'static str,
    pub body_kind: SecretBodyKind,
    pub suffix_len: SecretSuffixLen,
}

pub(crate) const OPENAI_SECRET_PREFIX: &str = "sk-";
pub(crate) const OPENAI_SECRET_REGEX: &str = r"sk-[a-zA-Z0-9_-]{20,}";
pub(crate) const OPENAI_SECRET_MIN_SUFFIX_LEN: usize = 20;

pub(crate) const GITHUB_PAT_PREFIX: &str = "ghp_";
pub(crate) const GITHUB_PAT_REGEX: &str = r"ghp_[a-zA-Z0-9]{36}";
pub(crate) const GITHUB_PAT_SUFFIX_LEN: usize = 36;

pub(crate) const AWS_ACCESS_KEY_PREFIX: &str = "AKIA";
pub(crate) const AWS_ACCESS_KEY_REGEX: &str = r"AKIA[0-9A-Z]{16}";
pub(crate) const AWS_ACCESS_KEY_SUFFIX_LEN: usize = 16;

pub(crate) const SECRET_PATTERNS: [SecretPattern; 3] = [
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

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn secret_catalog_has_expected_entries() {
        assert_eq!(SECRET_PATTERNS.len(), 3);
        assert_eq!(
            SECRET_PATTERNS[0],
            SecretPattern {
                prefix: "sk-",
                regex: r"sk-[a-zA-Z0-9_-]{20,}",
                body_kind: SecretBodyKind::OpenAiToken,
                suffix_len: SecretSuffixLen::Minimum(20),
            }
        );
        assert_eq!(
            SECRET_PATTERNS[1],
            SecretPattern {
                prefix: "ghp_",
                regex: r"ghp_[a-zA-Z0-9]{36}",
                body_kind: SecretBodyKind::LowercaseAlphanumeric,
                suffix_len: SecretSuffixLen::Exact(36),
            }
        );
        assert_eq!(
            SECRET_PATTERNS[2],
            SecretPattern {
                prefix: "AKIA",
                regex: r"AKIA[0-9A-Z]{16}",
                body_kind: SecretBodyKind::UppercaseAlphanumeric,
                suffix_len: SecretSuffixLen::Exact(16),
            }
        );

        for pattern in SECRET_PATTERNS {
            assert!(Regex::new(pattern.regex).is_ok());
        }
    }
}
