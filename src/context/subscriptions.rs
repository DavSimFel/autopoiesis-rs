use std::fs;
use std::time::UNIX_EPOCH;

use crate::llm::{ChatMessage, ChatRole};
use crate::subscription::{SubscriptionRecord, estimate_tokens};
use tracing::warn;

use super::ContextSource;

/// Session-scoped file subscriptions materialized into the model context.
pub struct SubscriptionContext {
    subscriptions: Vec<SubscriptionRecord>,
    token_budget: usize,
}

impl SubscriptionContext {
    pub fn new(subscriptions: Vec<SubscriptionRecord>, token_budget: usize) -> Self {
        Self {
            subscriptions,
            token_budget,
        }
    }

    fn effective_timestamp(record: &SubscriptionRecord) -> &str {
        record.effective_at()
    }

    fn provenance_tag(record: &SubscriptionRecord, mtime_unix: Option<u64>) -> String {
        let filter = record.filter.label();
        let mtime = mtime_unix
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "[subscription path={} filter={} mtime={}]",
            record.path.display(),
            filter,
            mtime
        )
    }

    fn build_body(prefix: &str, rendered: &str, truncated: bool, rendered_len: usize) -> String {
        let mut body = String::with_capacity(prefix.len() + rendered_len + 32);
        body.push_str(prefix);
        if rendered_len > 0 {
            body.push('\n');
            body.push_str(&rendered[..rendered_len]);
        }
        if truncated {
            body.push('\n');
            body.push_str("[truncated]");
        }
        body
    }

    fn fit_body(prefix: &str, rendered: &str, budget: usize) -> Option<(String, usize)> {
        let full = Self::build_body(prefix, rendered, false, rendered.len());
        let full_tokens = estimate_tokens(&full);
        if full_tokens <= budget {
            return Some((full, full_tokens));
        }

        let truncated_marker = Self::build_body(prefix, rendered, true, 0);
        if estimate_tokens(&truncated_marker) > budget {
            return None;
        }

        let boundaries = rendered
            .char_indices()
            .map(|(boundary, _)| boundary)
            .chain(std::iter::once(rendered.len()))
            .collect::<Vec<_>>();
        let mut low = 0usize;
        let mut high = boundaries.len().saturating_sub(1);
        let mut best = truncated_marker;

        while low <= high {
            let mid = low + (high - low).div_ceil(2);
            let boundary = boundaries[mid];
            let candidate = Self::build_body(prefix, rendered, true, boundary);
            let candidate_tokens = estimate_tokens(&candidate);
            if candidate_tokens <= budget {
                best = candidate;
                low = mid;
                if low == high {
                    break;
                }
            } else if mid == 0 {
                break;
            } else {
                high = mid - 1;
            }
        }

        let tokens = estimate_tokens(&best);
        Some((best, tokens))
    }

    fn materialize_record(
        &self,
        record: &SubscriptionRecord,
        remaining_budget: usize,
    ) -> Option<(ChatMessage, usize)> {
        let raw = match fs::read_to_string(&record.path) {
            Ok(raw) => raw,
            Err(error) => {
                warn!(
                    path = %record.path.display(),
                    error = %error,
                    "failed to read subscription file; skipping"
                );
                return None;
            }
        };

        let rendered = match record.filter.render(&raw) {
            Ok(rendered) => rendered,
            Err(error) => {
                warn!(
                    path = %record.path.display(),
                    error = %error,
                    "failed to render subscription file; skipping"
                );
                return None;
            }
        };

        let mtime_unix = fs::metadata(&record.path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        let prefix = Self::provenance_tag(record, mtime_unix);
        let (body, tokens) = Self::fit_body(&prefix, &rendered, remaining_budget)?;
        Some((
            ChatMessage::system_with_principal(body, Some(crate::principal::Principal::System)),
            tokens,
        ))
    }
}

impl ContextSource for SubscriptionContext {
    fn name(&self) -> &str {
        "subscriptions"
    }

    fn assemble(&self, messages: &mut Vec<ChatMessage>) {
        if self.subscriptions.is_empty() {
            return;
        }

        let mut subscriptions = self.subscriptions.clone();
        subscriptions.sort_by(|left, right| {
            Self::effective_timestamp(left)
                .cmp(Self::effective_timestamp(right))
                .then_with(|| left.id.cmp(&right.id))
        });

        let insert_at = if messages
            .first()
            .is_some_and(|message| message.role == ChatRole::System)
        {
            1
        } else {
            0
        };

        let mut remaining = self.token_budget;
        let mut offset = 0usize;
        for record in subscriptions {
            if remaining == 0 {
                break;
            }

            let Some((message, tokens)) = self.materialize_record(&record, remaining) else {
                continue;
            };
            messages.insert(insert_at + offset, message);
            offset += 1;
            remaining = remaining.saturating_sub(tokens);
        }
    }
}
