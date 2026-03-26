use crate::gate::{Guard, GuardContext, GuardEvent, Severity, Verdict};
use tracing::{debug, warn};

/// Guard precedence is deny > approve > allow. A deny returns immediately.
/// Approvals are collected and the highest-severity approval wins unless a later
/// guard denies the event.
pub fn resolve_verdict(
    guards: &[Box<dyn Guard>],
    mut event: GuardEvent,
    modified: bool,
    context: GuardContext,
) -> Verdict {
    let mut approved: Option<(String, String, Severity)> = None;
    let mut verdict = if modified {
        Verdict::Modify
    } else {
        Verdict::Allow
    };

    for guard in guards {
        debug!(guard = guard.name(), "evaluating guard");
        match guard.check(&mut event, &context) {
            Verdict::Allow => {}
            Verdict::Modify => verdict = Verdict::Modify,
            Verdict::Deny { reason, gate_id } => {
                warn!(gate_id = %gate_id, "guard denied event");
                return Verdict::Deny { reason, gate_id };
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
                    approved = Some((reason, gate_id, severity));
                }
            }
        }
    }

    if let Some((reason, gate_id, severity)) = approved {
        debug!(gate_id = %gate_id, severity = ?severity, "guard approval selected");
        Verdict::Approve {
            reason,
            gate_id,
            severity,
        }
    } else {
        verdict
    }
}
