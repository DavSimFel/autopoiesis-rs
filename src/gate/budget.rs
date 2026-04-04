use crate::config::BudgetConfig;
use crate::gate::{BudgetSnapshot, Guard, GuardContext, GuardEvent, Verdict};

pub const BUDGET_GUARD_ID: &str = "budget";

/// Guard that enforces preflight token ceilings from the live session snapshot.
pub struct BudgetGuard {
    id: String,
    limits: BudgetConfig,
}

impl BudgetGuard {
    pub fn new(limits: BudgetConfig) -> Self {
        Self {
            id: BUDGET_GUARD_ID.to_string(),
            limits,
        }
    }

    fn violation_reason(label: &str, observed: u64, limit: u64) -> String {
        format!("{label} token ceiling exceeded: observed {observed}, limit {limit}")
    }

    fn violations(&self, budget: &BudgetSnapshot) -> Vec<String> {
        // Invariant: violation order is stable so operator output and tests stay deterministic.
        let mut violations = Vec::new();

        if let Some(limit) = self.limits.max_tokens_per_turn
            && budget.turn_tokens > limit
        {
            violations.push(Self::violation_reason("turn", budget.turn_tokens, limit));
        }

        if let Some(limit) = self.limits.max_tokens_per_session
            && budget.session_tokens > limit
        {
            violations.push(Self::violation_reason(
                "session",
                budget.session_tokens,
                limit,
            ));
        }

        if let Some(limit) = self.limits.max_tokens_per_day
            && budget.day_tokens > limit
        {
            violations.push(Self::violation_reason("day", budget.day_tokens, limit));
        }

        violations
    }
}

impl Guard for BudgetGuard {
    fn name(&self) -> &str {
        &self.id
    }

    fn check(&self, event: &mut GuardEvent, context: &GuardContext) -> Verdict {
        // Policy: budget checks are inbound preflight hard denies; they do not mutate or redact content.
        match event {
            GuardEvent::Inbound(_) => {
                let violations = self.violations(&context.budget);
                if violations.is_empty() {
                    Verdict::Allow
                } else {
                    Verdict::Deny {
                        reason: format!("budget exceeded: {}", violations.join("; ")),
                        gate_id: self.id.clone(),
                    }
                }
            }
            _ => Verdict::Allow,
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;

    fn limits(turn: Option<u64>, session: Option<u64>, day: Option<u64>) -> BudgetConfig {
        BudgetConfig {
            max_tokens_per_turn: turn,
            max_tokens_per_session: session,
            max_tokens_per_day: day,
        }
    }

    fn context(turn: u64, session: u64, day: u64) -> GuardContext {
        GuardContext {
            budget: BudgetSnapshot {
                turn_tokens: turn,
                session_tokens: session,
                day_tokens: day,
            },
            ..Default::default()
        }
    }

    fn inbound_event<'a>(messages: &'a mut Vec<ChatMessage>) -> GuardEvent<'a> {
        GuardEvent::Inbound(messages)
    }

    #[test]
    fn per_turn_exceeded_denies() {
        let guard = BudgetGuard::new(limits(Some(100), None, None));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        match guard.check(&mut event, &context(101, 10, 10)) {
            Verdict::Deny { reason, gate_id } => {
                assert_eq!(gate_id, BUDGET_GUARD_ID);
                assert_eq!(
                    reason,
                    "budget exceeded: turn token ceiling exceeded: observed 101, limit 100"
                );
            }
            verdict => panic!("expected deny, got {verdict:?}"),
        }
    }

    #[test]
    fn per_session_exceeded_denies() {
        let guard = BudgetGuard::new(limits(None, Some(200), None));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        match guard.check(&mut event, &context(10, 250, 10)) {
            Verdict::Deny { reason, gate_id } => {
                assert_eq!(gate_id, BUDGET_GUARD_ID);
                assert_eq!(
                    reason,
                    "budget exceeded: session token ceiling exceeded: observed 250, limit 200"
                );
            }
            verdict => panic!("expected deny, got {verdict:?}"),
        }
    }

    #[test]
    fn per_day_exceeded_denies() {
        let guard = BudgetGuard::new(limits(None, None, Some(300)));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        match guard.check(&mut event, &context(10, 10, 301)) {
            Verdict::Deny { reason, gate_id } => {
                assert_eq!(gate_id, BUDGET_GUARD_ID);
                assert_eq!(
                    reason,
                    "budget exceeded: day token ceiling exceeded: observed 301, limit 300"
                );
            }
            verdict => panic!("expected deny, got {verdict:?}"),
        }
    }

    #[test]
    fn multiple_violations_are_reported_in_deterministic_order() {
        let guard = BudgetGuard::new(limits(Some(10), Some(20), Some(30)));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        match guard.check(&mut event, &context(11, 21, 31)) {
            Verdict::Deny { reason, gate_id } => {
                assert_eq!(gate_id, BUDGET_GUARD_ID);
                assert_eq!(
                    reason,
                    "budget exceeded: turn token ceiling exceeded: observed 11, limit 10; session token ceiling exceeded: observed 21, limit 20; day token ceiling exceeded: observed 31, limit 30"
                );
            }
            verdict => panic!("expected deny, got {verdict:?}"),
        }
    }

    #[test]
    fn under_budget_allows() {
        let guard = BudgetGuard::new(limits(Some(100), Some(200), Some(300)));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        assert!(matches!(
            guard.check(&mut event, &context(10, 20, 30)),
            Verdict::Allow
        ));
    }

    #[test]
    fn no_effective_limits_set_allows() {
        let guard = BudgetGuard::new(limits(None, None, None));
        let mut messages = vec![ChatMessage::user("hello")];
        let mut event = inbound_event(&mut messages);

        assert!(matches!(
            guard.check(&mut event, &context(10, 20, 30)),
            Verdict::Allow
        ));
    }
}
