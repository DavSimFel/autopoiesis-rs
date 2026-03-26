use tracing::debug;

use super::Session;
use crate::gate::BudgetSnapshot;
use crate::llm::ChatRole;
use crate::principal::Principal;
use anyhow::Context;
use std::io::BufRead;

impl Session {
    pub(super) fn latest_turn_tokens(&self) -> u64 {
        let mut total = 0;

        for (message, token_delta) in self.messages.iter().zip(self.message_tokens.iter()).rev() {
            match message.role {
                ChatRole::User => break,
                ChatRole::Assistant if message.principal == Principal::Agent => {
                    total += *token_delta;
                }
                _ => {}
            }
        }

        total
    }

    pub(super) fn today_token_total(&self) -> anyhow::Result<u64> {
        let path = self.today_path();
        if !path.exists() {
            return Ok(0);
        }

        let file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open sessions file {}", path.display()))?;
        let reader = std::io::BufReader::new(file);
        let mut total = 0;

        for raw_line in reader.lines() {
            let raw_line = raw_line?;
            if raw_line.trim().is_empty() {
                continue;
            }

            let entry: super::SessionEntry = serde_json::from_str(&raw_line)
                .with_context(|| format!("failed to parse session entry in {}", path.display()))?;
            total += Session::token_total(entry.meta.as_ref());
        }

        Ok(total)
    }

    /// Read the live budget snapshot used by budget guards.
    pub fn budget_snapshot(&self) -> anyhow::Result<BudgetSnapshot> {
        let snapshot = BudgetSnapshot {
            turn_tokens: self.latest_turn_tokens(),
            session_tokens: self.session_total_tokens,
            day_tokens: self.today_token_total()?,
        };
        debug!(
            turn_tokens = snapshot.turn_tokens,
            session_tokens = snapshot.session_tokens,
            day_tokens = snapshot.day_tokens,
            "read budget snapshot"
        );
        Ok(snapshot)
    }

    /// Get total token count from provider metadata.
    pub fn total_tokens(&self) -> u64 {
        self.total_tokens
    }

    /// Get cumulative session token count from provider metadata.
    pub fn session_total_tokens(&self) -> u64 {
        self.session_total_tokens
    }
}
