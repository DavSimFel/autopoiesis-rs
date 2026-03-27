use crate::gate::{Guard, GuardContext, GuardEvent, Severity, Verdict};
use tracing::{debug, warn};

/// Structured outcome for a single guard evaluation.
#[derive(Debug, Clone)]
pub struct GuardTraceOutcome {
    pub gate_id: Option<String>,
    pub denied: bool,
    pub requested_approval: bool,
    pub modified: bool,
    pub reason: Option<String>,
    pub severity: Option<Severity>,
}

/// Verdict plus the per-guard trace details that produced it.
#[derive(Debug, Clone)]
pub struct TracedVerdict {
    pub verdict: Verdict,
    pub guard_outcomes: Vec<GuardTraceOutcome>,
}

fn guard_event_signature(event: &GuardEvent<'_>) -> String {
    match event {
        GuardEvent::Inbound(messages) => format!("{messages:?}"),
        GuardEvent::ToolCall(call) => format!("{call:?}"),
        GuardEvent::ToolBatch(calls) => format!("{calls:?}"),
        GuardEvent::TextDelta(text) => text.to_string(),
    }
}

fn traced_outcome(guard_name: &str, verdict: &Verdict, modified: bool) -> GuardTraceOutcome {
    match verdict {
        Verdict::Allow => GuardTraceOutcome {
            gate_id: Some(guard_name.to_string()),
            denied: false,
            requested_approval: false,
            modified,
            reason: None,
            severity: None,
        },
        Verdict::Modify => GuardTraceOutcome {
            gate_id: Some(guard_name.to_string()),
            denied: false,
            requested_approval: false,
            modified: true,
            reason: None,
            severity: None,
        },
        Verdict::Deny { reason, gate_id } => GuardTraceOutcome {
            gate_id: Some(if gate_id.is_empty() {
                guard_name.to_string()
            } else {
                gate_id.clone()
            }),
            denied: true,
            requested_approval: false,
            modified,
            reason: Some(reason.clone()),
            severity: None,
        },
        Verdict::Approve {
            reason,
            gate_id,
            severity,
        } => GuardTraceOutcome {
            gate_id: Some(if gate_id.is_empty() {
                guard_name.to_string()
            } else {
                gate_id.clone()
            }),
            denied: false,
            requested_approval: true,
            modified,
            reason: Some(reason.clone()),
            severity: Some(*severity),
        },
    }
}

/// Resolve a guard verdict while preserving per-guard trace data.
pub fn resolve_traced_verdict(
    guards: &[Box<dyn Guard>],
    mut event: GuardEvent<'_>,
    modified: bool,
    context: GuardContext,
) -> TracedVerdict {
    let mut approved: Option<(String, String, Severity)> = None;
    let mut verdict = if modified {
        Verdict::Modify
    } else {
        Verdict::Allow
    };
    let mut guard_outcomes = Vec::new();

    for guard in guards {
        debug!(guard = guard.name(), "evaluating guard");
        let before = guard_event_signature(&event);
        let guard_verdict = guard.check(&mut event, &context);
        let after = guard_event_signature(&event);
        let guard_modified = before != after || matches!(guard_verdict, Verdict::Modify);
        let outcome = traced_outcome(guard.name(), &guard_verdict, guard_modified);

        match guard_verdict {
            Verdict::Allow => {}
            Verdict::Modify => verdict = Verdict::Modify,
            Verdict::Deny { reason, gate_id } => {
                warn!(gate_id = %gate_id, "guard denied event");
                guard_outcomes.push(outcome);
                return TracedVerdict {
                    verdict: Verdict::Deny { reason, gate_id },
                    guard_outcomes,
                };
            }
            Verdict::Approve {
                reason,
                gate_id,
                severity,
            } => {
                debug!(gate_id = %gate_id, severity = ?severity, "guard requested approval");
                if approved
                    .as_ref()
                    .is_none_or(|(_, _, current)| severity > *current)
                {
                    approved = Some((reason.clone(), gate_id.clone(), severity));
                }
            }
        }

        guard_outcomes.push(outcome);
    }

    let verdict = if let Some((reason, gate_id, severity)) = approved {
        debug!(gate_id = %gate_id, severity = ?severity, "guard approval selected");
        Verdict::Approve {
            reason,
            gate_id,
            severity,
        }
    } else {
        verdict
    };

    TracedVerdict {
        verdict,
        guard_outcomes,
    }
}

/// Guard precedence is deny > approve > allow. A deny returns immediately.
/// Approvals are collected and the highest-severity approval wins unless a later
/// guard denies the event.
pub fn resolve_verdict(
    guards: &[Box<dyn Guard>],
    event: GuardEvent,
    modified: bool,
    context: GuardContext,
) -> Verdict {
    resolve_traced_verdict(guards, event, modified, context).verdict
}
