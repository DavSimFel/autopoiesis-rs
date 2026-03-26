const REDACTION_MARKER: &str = "[REDACTED]";
use super::secret_catalog::{SECRET_PATTERNS, SecretBodyKind, SecretPattern, SecretSuffixLen};

enum StreamingSecretDecision {
    NeedMore,
    EmitLiteral {
        consumed_bytes: usize,
    },
    EmitRedacted {
        consumed_bytes: usize,
        continue_redacting: bool,
    },
}

fn is_body_byte(kind: SecretBodyKind, byte: u8) -> bool {
    match kind {
        SecretBodyKind::OpenAiToken => byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_',
        SecretBodyKind::LowercaseAlphanumeric => byte.is_ascii_alphanumeric(),
        SecretBodyKind::UppercaseAlphanumeric => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
    }
}

fn prefix_holdback() -> usize {
    // Policy: keep enough trailing bytes buffered to avoid leaking a secret prefix before matching can finish.
    SECRET_PATTERNS
        .iter()
        .map(|pattern| pattern.prefix.len())
        .max()
        .unwrap_or(0)
        .saturating_sub(1)
}

fn find_earliest_secret_prefix(text: &str) -> Option<(usize, usize)> {
    SECRET_PATTERNS
        .iter()
        .enumerate()
        .filter_map(|(index, pattern)| text.find(pattern.prefix).map(|position| (position, index)))
        .min_by_key(|(position, _)| *position)
}

fn take_prefix(buffer: &mut String, byte_len: usize) -> String {
    buffer.drain(..byte_len).collect()
}

fn analyze_secret_candidate(
    pattern: &SecretPattern,
    rest: &str,
    final_flush: bool,
) -> StreamingSecretDecision {
    let bytes = rest.as_bytes();
    let mut allowed_len = 0usize;

    match pattern.suffix_len {
        SecretSuffixLen::Minimum(min_len) => {
            while allowed_len < bytes.len() && is_body_byte(pattern.body_kind, bytes[allowed_len]) {
                allowed_len += 1;
            }

            if allowed_len < min_len {
                if allowed_len == bytes.len() {
                    if final_flush {
                        StreamingSecretDecision::EmitRedacted {
                            consumed_bytes: pattern.prefix.len() + allowed_len,
                            continue_redacting: false,
                        }
                    } else {
                        StreamingSecretDecision::NeedMore
                    }
                } else {
                    StreamingSecretDecision::EmitLiteral {
                        consumed_bytes: pattern.prefix.len() + allowed_len,
                    }
                }
            } else if allowed_len == bytes.len() {
                StreamingSecretDecision::EmitRedacted {
                    consumed_bytes: pattern.prefix.len() + allowed_len,
                    continue_redacting: !final_flush,
                }
            } else {
                StreamingSecretDecision::EmitRedacted {
                    consumed_bytes: pattern.prefix.len() + allowed_len,
                    continue_redacting: false,
                }
            }
        }
        SecretSuffixLen::Exact(required_len) => {
            while allowed_len < bytes.len()
                && allowed_len < required_len
                && is_body_byte(pattern.body_kind, bytes[allowed_len])
            {
                allowed_len += 1;
            }

            if allowed_len < required_len {
                if allowed_len == bytes.len() && !final_flush {
                    StreamingSecretDecision::NeedMore
                } else {
                    StreamingSecretDecision::EmitLiteral {
                        consumed_bytes: pattern.prefix.len() + allowed_len,
                    }
                }
            } else {
                StreamingSecretDecision::EmitRedacted {
                    consumed_bytes: pattern.prefix.len() + required_len,
                    continue_redacting: false,
                }
            }
        }
    }
}

pub(crate) struct StreamingTextBuffer {
    pending: String,
    active_secret: Option<usize>,
}

impl StreamingTextBuffer {
    pub(crate) fn new() -> Self {
        Self {
            pending: String::new(),
            active_secret: None,
        }
    }

    fn emit_segment<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E, segment: String)
    where
        R: FnMut(String) -> String + ?Sized,
        E: FnMut(String) + ?Sized,
    {
        let redacted_output = redact_text(segment);
        if !redacted_output.is_empty() {
            emit_token(redacted_output);
        }
    }

    fn flush<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E, final_flush: bool)
    where
        R: FnMut(String) -> String + ?Sized,
        E: FnMut(String) + ?Sized,
    {
        loop {
            if self.pending.is_empty() {
                if final_flush {
                    self.active_secret = None;
                }
                break;
            }

            if let Some(secret_index) = self.active_secret {
                let pattern = &SECRET_PATTERNS[secret_index];
                let bytes = self.pending.as_bytes();
                let mut allowed_len = 0usize;

                while allowed_len < bytes.len()
                    && is_body_byte(pattern.body_kind, bytes[allowed_len])
                {
                    allowed_len += 1;
                }

                if allowed_len > 0 {
                    self.pending.drain(..allowed_len);
                }

                if self.pending.is_empty() {
                    if final_flush {
                        self.active_secret = None;
                    }
                    break;
                }

                self.active_secret = None;
                continue;
            }

            if let Some((prefix_index, pattern_index)) = find_earliest_secret_prefix(&self.pending)
            {
                if prefix_index > 0 {
                    let safe_prefix = take_prefix(&mut self.pending, prefix_index);
                    self.emit_segment(redact_text, emit_token, safe_prefix);
                    continue;
                }

                let pattern = &SECRET_PATTERNS[pattern_index];
                let rest = &self.pending[pattern.prefix.len()..];

                match analyze_secret_candidate(pattern, rest, final_flush) {
                    StreamingSecretDecision::NeedMore => break,
                    StreamingSecretDecision::EmitLiteral { consumed_bytes } => {
                        let segment = take_prefix(&mut self.pending, consumed_bytes);
                        self.emit_segment(redact_text, emit_token, segment);
                    }
                    StreamingSecretDecision::EmitRedacted {
                        consumed_bytes,
                        continue_redacting,
                    } => {
                        let _ = take_prefix(&mut self.pending, consumed_bytes);
                        // Policy: once a secret match is confirmed, keep redacting across token boundaries until the candidate ends.
                        self.emit_segment(redact_text, emit_token, REDACTION_MARKER.to_string());
                        self.active_secret = continue_redacting.then_some(pattern_index);
                    }
                }

                continue;
            }

            let char_count = self.pending.chars().count();
            let holdback = prefix_holdback();
            if final_flush {
                let segment = std::mem::take(&mut self.pending);
                self.emit_segment(redact_text, emit_token, segment);
                break;
            }

            // Policy: buffer any prefix-sized tail so a secret candidate cannot leak before the next token arrives.
            if char_count <= holdback {
                break;
            }

            let emit_char_count = char_count - holdback;
            let split_byte = self
                .pending
                .char_indices()
                .nth(emit_char_count)
                .map(|(index, _)| index)
                .unwrap_or(self.pending.len());
            let segment = take_prefix(&mut self.pending, split_byte);
            self.emit_segment(redact_text, emit_token, segment);
        }
    }

    pub(crate) fn push<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E, token: String)
    where
        R: FnMut(String) -> String + ?Sized,
        E: FnMut(String) + ?Sized,
    {
        self.pending.push_str(&token);
        self.flush(redact_text, emit_token, false);
    }

    pub(crate) fn finish<R, E>(&mut self, redact_text: &mut R, emit_token: &mut E)
    where
        R: FnMut(String) -> String + ?Sized,
        E: FnMut(String) + ?Sized,
    {
        self.flush(redact_text, emit_token, true);
    }
}

#[cfg(test)]
mod tests {
    use super::super::secret_catalog::SECRET_PATTERNS;
    use super::*;
    use std::cell::RefCell;

    fn identity(segment: String) -> String {
        segment
    }

    #[test]
    fn buffer_works_with_identity_redactor_and_vec_sink() {
        let mut buffer = StreamingTextBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let mut redact_text = identity;
        let mut emit_token = |token: String| emitted.borrow_mut().push(token);

        buffer.push(&mut redact_text, &mut emit_token, "hello".to_string());
        buffer.finish(&mut redact_text, &mut emit_token);

        assert_eq!(emitted.borrow().concat(), "hello");
    }

    #[test]
    fn outbound_text_is_streamed_incrementally_to_sink() {
        let mut buffer = StreamingTextBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let mut redact_text = identity;
        let mut emit_token = |token: String| emitted.borrow_mut().push(token);

        buffer.push(&mut redact_text, &mut emit_token, "hello ".to_string());
        assert_eq!(emitted.borrow().concat(), "hel");

        buffer.push(&mut redact_text, &mut emit_token, "world".to_string());
        assert!(emitted.borrow().concat().starts_with("hel"));
        assert!(emitted.borrow().concat().len() > 3);

        buffer.finish(&mut redact_text, &mut emit_token);
        assert_eq!(emitted.borrow().concat(), "hello world");
    }

    #[test]
    fn outbound_secret_split_across_tokens_is_redacted_before_sink() {
        let mut buffer = StreamingTextBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let mut redact_text = identity;
        let mut emit_token = |token: String| emitted.borrow_mut().push(token);
        let secret = format!(
            "{}proj-abcdefghijklmnopqrstuvwxyz012345",
            SECRET_PATTERNS[0].prefix
        );
        let first_chunk = format!("hello {}", &secret[..16]);
        let second_chunk = format!("{} world", &secret[16..]);

        buffer.push(&mut redact_text, &mut emit_token, first_chunk);
        buffer.push(&mut redact_text, &mut emit_token, second_chunk);
        buffer.finish(&mut redact_text, &mut emit_token);

        let rendered = emitted.borrow().concat();
        assert_eq!(rendered, "hello [REDACTED] world");
        assert!(!rendered.contains(&secret));
    }

    #[test]
    fn outbound_fixed_length_secret_prefixes_are_redacted_before_sink() {
        let mut buffer = StreamingTextBuffer::new();
        let emitted = RefCell::new(Vec::new());
        let mut redact_text = identity;
        let mut emit_token = |token: String| emitted.borrow_mut().push(token);
        let github = format!(
            "{}0123456789abcdefghijklmnopqrstuvwxyz",
            SECRET_PATTERNS[1].prefix
        );
        let aws = format!("{}1234567890ABCDEF", SECRET_PATTERNS[2].prefix);

        buffer.push(
            &mut redact_text,
            &mut emit_token,
            format!("lead {}", &github[..7]),
        );
        buffer.push(
            &mut redact_text,
            &mut emit_token,
            format!("{} mid {}", &github[7..], &aws[..10]),
        );
        buffer.push(
            &mut redact_text,
            &mut emit_token,
            format!("{} tail", &aws[10..]),
        );
        buffer.finish(&mut redact_text, &mut emit_token);

        let rendered = emitted.borrow().concat();
        assert_eq!(rendered, "lead [REDACTED] mid [REDACTED] tail");
        assert!(!rendered.contains("ghp_"));
        assert!(!rendered.contains("AKIA"));
    }
}
