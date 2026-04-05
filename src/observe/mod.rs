//! Structured observability events and observer fan-out.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tracing::warn;

pub mod otel;
pub mod sqlite;

use otel::OtelObserver;
use sqlite::SqliteObserver;

static TEST_DISABLE_OBSERVERS: AtomicBool = AtomicBool::new(false);

/// Disable observer construction for tests without mutating process-global env state.
#[doc(hidden)]
pub fn disable_observers_for_tests() {
    TEST_DISABLE_OBSERVERS.store(true, Ordering::SeqCst);
}

/// Structured event emitted by the runtime observability layer.
///
/// Eval events are part of the schema even though this repository currently has
/// no active eval execution path wired into the runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TraceEvent {
    EvalRunStarted {
        eval_run_id: String,
        session_id: Option<String>,
    },
    EvalRunFinished {
        eval_run_id: String,
        session_id: Option<String>,
        status: String,
        total_turns: Option<i64>,
    },
    TurnStarted {
        session_id: String,
        turn_id: String,
        user_principal: Option<String>,
        model: Option<String>,
        message_count: Option<i64>,
    },
    TurnFinished {
        session_id: String,
        turn_id: String,
        status: String,
        elapsed_ms: Option<i64>,
        prompt_tokens: Option<i64>,
        completion_tokens: Option<i64>,
        total_tokens: Option<i64>,
    },
    CompletionFinished {
        session_id: String,
        turn_id: String,
        stop_reason: Option<String>,
        tool_call_count: Option<i64>,
    },
    GuardDenied {
        session_id: String,
        turn_id: String,
        gate_id: String,
        reason: String,
        severity: Option<String>,
    },
    GuardModified {
        session_id: String,
        turn_id: String,
        gate_id: String,
        reason: Option<String>,
    },
    GuardApprovalRequested {
        session_id: String,
        turn_id: String,
        gate_id: String,
        reason: String,
        severity: String,
    },
    GuardApprovalGranted {
        session_id: String,
        turn_id: String,
        gate_id: String,
        reason: String,
        severity: String,
    },
    GuardApprovalDenied {
        session_id: String,
        turn_id: String,
        gate_id: String,
        reason: String,
        severity: String,
    },
    ToolCallStarted {
        session_id: String,
        turn_id: String,
        call_id: String,
        tool_name: String,
        command: Option<String>,
    },
    ToolCallFinished {
        session_id: String,
        turn_id: String,
        call_id: String,
        tool_name: String,
        status: String,
        exit_code: Option<i64>,
        was_approved: Option<bool>,
        was_denied: Option<bool>,
    },
    PlanRunCreated {
        session_id: String,
        plan_run_id: String,
        caused_by_turn_id: Option<String>,
        owner_session_id: String,
    },
    PlanRunPatched {
        session_id: String,
        plan_run_id: String,
        caused_by_turn_id: Option<String>,
        owner_session_id: String,
    },
    PlanStepAttemptStarted {
        session_id: String,
        plan_run_id: String,
        step_index: i64,
        step_id: String,
        attempt: i64,
        child_session_id: Option<String>,
    },
    PlanStepAttemptFinished {
        session_id: String,
        plan_run_id: String,
        step_index: i64,
        step_id: String,
        attempt: i64,
        status: String,
        child_session_id: Option<String>,
    },
    PlanWaitingT2 {
        session_id: String,
        plan_run_id: String,
        step_index: i64,
        reason: Option<String>,
    },
    PlanRecovered {
        session_id: String,
        plan_run_id: String,
        reason: Option<String>,
    },
    PlanCompleted {
        session_id: String,
        plan_run_id: String,
        total_attempts: i64,
    },
    PlanFailed {
        session_id: String,
        plan_run_id: String,
        total_attempts: i64,
        reason: Option<String>,
    },
    FailureNotifiedToT2 {
        session_id: String,
        plan_run_id: String,
        owner_session_id: String,
    },
}

/// Observer interface for structured runtime events.
pub trait Observer: Send + Sync + 'static {
    fn emit(&self, event: &TraceEvent);
}

/// Observer that intentionally drops every event.
#[derive(Debug, Default)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    fn emit(&self, _event: &TraceEvent) {}
}

/// Observer that forwards one event to a set of child observers.
#[derive(Clone, Default)]
pub struct MultiObserver {
    observers: Vec<Arc<dyn Observer>>,
}

impl MultiObserver {
    pub fn new(observers: Vec<Arc<dyn Observer>>) -> Self {
        Self { observers }
    }
}

impl Observer for MultiObserver {
    fn emit(&self, event: &TraceEvent) {
        for observer in &self.observers {
            observer.emit(event);
        }
    }
}

/// Build the runtime observer stack for a sessions directory.
pub fn build_observer(sessions_dir: &Path) -> Arc<dyn Observer> {
    let mut observers: Vec<Arc<dyn Observer>> = Vec::new();

    match SqliteObserver::new(sessions_dir) {
        Ok(observer) => observers.push(Arc::new(observer)),
        Err(error) => {
            warn!(%error, "failed to initialize sqlite trace observer");
        }
    }

    if tokio::runtime::Handle::try_current().is_ok() {
        match OtelObserver::new() {
            Ok(observer) => observers.push(Arc::new(observer)),
            Err(error) => {
                warn!(%error, "failed to initialize otel trace observer");
            }
        }
    }

    if observers.is_empty() {
        Arc::new(NoopObserver)
    } else if observers.len() == 1 {
        observers.remove(0)
    } else {
        Arc::new(MultiObserver::new(observers))
    }
}

/// Build the runtime observer stack, falling back to `NoopObserver` during tests.
pub fn runtime_observer(sessions_dir: &Path) -> Arc<dyn Observer> {
    if cfg!(test)
        || TEST_DISABLE_OBSERVERS.load(Ordering::SeqCst)
        || std::env::var_os("AUTOPOIESIS_DISABLE_OBSERVERS").is_some()
    {
        Arc::new(NoopObserver)
    } else {
        build_observer(sessions_dir)
    }
}

impl TraceEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::EvalRunStarted { .. } => "EvalRunStarted",
            Self::EvalRunFinished { .. } => "EvalRunFinished",
            Self::TurnStarted { .. } => "TurnStarted",
            Self::TurnFinished { .. } => "TurnFinished",
            Self::CompletionFinished { .. } => "CompletionFinished",
            Self::GuardDenied { .. } => "GuardDenied",
            Self::GuardModified { .. } => "GuardModified",
            Self::GuardApprovalRequested { .. } => "GuardApprovalRequested",
            Self::GuardApprovalGranted { .. } => "GuardApprovalGranted",
            Self::GuardApprovalDenied { .. } => "GuardApprovalDenied",
            Self::ToolCallStarted { .. } => "ToolCallStarted",
            Self::ToolCallFinished { .. } => "ToolCallFinished",
            Self::PlanRunCreated { .. } => "PlanRunCreated",
            Self::PlanRunPatched { .. } => "PlanRunPatched",
            Self::PlanStepAttemptStarted { .. } => "PlanStepAttemptStarted",
            Self::PlanStepAttemptFinished { .. } => "PlanStepAttemptFinished",
            Self::PlanWaitingT2 { .. } => "PlanWaitingT2",
            Self::PlanRecovered { .. } => "PlanRecovered",
            Self::PlanCompleted { .. } => "PlanCompleted",
            Self::PlanFailed { .. } => "PlanFailed",
            Self::FailureNotifiedToT2 { .. } => "FailureNotifiedToT2",
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::EvalRunStarted { session_id, .. } | Self::EvalRunFinished { session_id, .. } => {
                session_id.as_deref()
            }
            Self::TurnStarted { session_id, .. }
            | Self::TurnFinished { session_id, .. }
            | Self::CompletionFinished { session_id, .. }
            | Self::GuardDenied { session_id, .. }
            | Self::GuardModified { session_id, .. }
            | Self::GuardApprovalRequested { session_id, .. }
            | Self::GuardApprovalGranted { session_id, .. }
            | Self::GuardApprovalDenied { session_id, .. }
            | Self::ToolCallStarted { session_id, .. }
            | Self::ToolCallFinished { session_id, .. }
            | Self::PlanRunCreated { session_id, .. }
            | Self::PlanRunPatched { session_id, .. }
            | Self::PlanStepAttemptStarted { session_id, .. }
            | Self::PlanStepAttemptFinished { session_id, .. }
            | Self::PlanWaitingT2 { session_id, .. }
            | Self::PlanRecovered { session_id, .. }
            | Self::PlanCompleted { session_id, .. }
            | Self::PlanFailed { session_id, .. }
            | Self::FailureNotifiedToT2 { session_id, .. } => Some(session_id.as_str()),
        }
    }

    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::TurnStarted { turn_id, .. }
            | Self::TurnFinished { turn_id, .. }
            | Self::CompletionFinished { turn_id, .. }
            | Self::GuardDenied { turn_id, .. }
            | Self::GuardModified { turn_id, .. }
            | Self::GuardApprovalRequested { turn_id, .. }
            | Self::GuardApprovalGranted { turn_id, .. }
            | Self::GuardApprovalDenied { turn_id, .. }
            | Self::ToolCallStarted { turn_id, .. }
            | Self::ToolCallFinished { turn_id, .. } => Some(turn_id.as_str()),
            _ => None,
        }
    }

    pub fn plan_run_id(&self) -> Option<&str> {
        match self {
            Self::PlanRunCreated { plan_run_id, .. }
            | Self::PlanRunPatched { plan_run_id, .. }
            | Self::PlanStepAttemptStarted { plan_run_id, .. }
            | Self::PlanStepAttemptFinished { plan_run_id, .. }
            | Self::PlanWaitingT2 { plan_run_id, .. }
            | Self::PlanRecovered { plan_run_id, .. }
            | Self::PlanCompleted { plan_run_id, .. }
            | Self::PlanFailed { plan_run_id, .. }
            | Self::FailureNotifiedToT2 { plan_run_id, .. } => Some(plan_run_id.as_str()),
            _ => None,
        }
    }

    pub fn eval_run_id(&self) -> Option<&str> {
        match self {
            Self::EvalRunStarted { eval_run_id, .. }
            | Self::EvalRunFinished { eval_run_id, .. } => Some(eval_run_id.as_str()),
            _ => None,
        }
    }
}

#[cfg(all(test, not(clippy)))]
pub(crate) mod test_support {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    pub struct RecordingObserver {
        events: Arc<Mutex<Vec<TraceEvent>>>,
    }

    impl RecordingObserver {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn events(&self) -> Vec<TraceEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl Observer for RecordingObserver {
        fn emit(&self, event: &TraceEvent) {
            self.events.lock().unwrap().push(event.clone());
        }
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct CountingObserver {
        count: AtomicUsize,
    }

    impl CountingObserver {
        fn new() -> Self {
            Self {
                count: AtomicUsize::new(0),
            }
        }
    }

    impl Observer for CountingObserver {
        fn emit(&self, _event: &TraceEvent) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "autopoiesis_observe_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn multi_observer_forwards_events() {
        let first = Arc::new(CountingObserver::new());
        let second = Arc::new(CountingObserver::new());
        let multi = MultiObserver::new(vec![first.clone(), second.clone()]);
        let event = TraceEvent::EvalRunStarted {
            eval_run_id: "eval-1".to_string(),
            session_id: Some("session-1".to_string()),
        };

        multi.emit(&event);

        assert_eq!(first.count.load(Ordering::SeqCst), 1);
        assert_eq!(second.count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn noop_observer_discards_events() {
        let observer = NoopObserver;
        let event = TraceEvent::EvalRunFinished {
            eval_run_id: "eval-1".to_string(),
            session_id: None,
            status: "completed".to_string(),
            total_turns: Some(2),
        };

        observer.emit(&event);
    }

    #[test]
    fn trace_event_accessors_return_expected_ids() {
        let event = TraceEvent::PlanRunCreated {
            session_id: "session-1".to_string(),
            plan_run_id: "plan-1".to_string(),
            caused_by_turn_id: Some("turn-1".to_string()),
            owner_session_id: "owner-1".to_string(),
        };

        assert_eq!(event.session_id(), Some("session-1"));
        assert_eq!(event.plan_run_id(), Some("plan-1"));
        assert_eq!(event.turn_id(), None);
        assert_eq!(event.eval_run_id(), None);
        assert_eq!(event.event_type(), "PlanRunCreated");
    }

    #[test]
    fn build_observer_falls_back_to_noop_when_backends_fail() {
        let dir = temp_dir("fallback");
        let observer = build_observer(&dir);
        observer.emit(&TraceEvent::EvalRunStarted {
            eval_run_id: "eval-1".to_string(),
            session_id: None,
        });
    }
}
