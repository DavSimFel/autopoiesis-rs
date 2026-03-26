use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(test)]
use std::cell::Cell;

use crate::agent::{ApprovalHandler, SpawnDrainContext, TokenSink, finish_spawned_child_drain};
use crate::config::Config;
use crate::gate::output_cap::safe_call_id_for_filename;
use crate::llm::ToolCall;
use crate::plan::notify::notify_plan_failure;
use crate::plan::recovery::crash_plan_run_to_waiting_t2;
use crate::plan::{PlanAction, PlanStepSpec, ShellCheckSpec, ShellExpectation};
use crate::session::Session;
use crate::spawn::SpawnRequest;
use crate::store::{NullableUpdate, PlanRun, PlanRunUpdateFields, StepAttemptRecord, Store};
use crate::turn::build_t3_turn;
use crate::util::utc_timestamp;

#[cfg(test)]
thread_local! {
    static FORCE_MISSING_SPAWN_METADATA: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
fn set_force_missing_spawn_metadata(value: bool) {
    FORCE_MISSING_SPAWN_METADATA.with(|flag| flag.set(value));
}

#[cfg(test)]
fn should_force_missing_spawn_metadata() -> bool {
    FORCE_MISSING_SPAWN_METADATA.with(|flag| flag.get())
}

#[cfg(test)]
fn maybe_force_missing_spawn_metadata(metadata_json: Option<String>) -> Option<String> {
    if should_force_missing_spawn_metadata() {
        None
    } else {
        metadata_json
    }
}

#[cfg(not(test))]
fn maybe_force_missing_spawn_metadata(metadata_json: Option<String>) -> Option<String> {
    metadata_json
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    Advanced,
    WaitingT2 { failure: PlanFailureDetails },
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckVerdict {
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub artifact_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub check_id: String,
    pub verdict: CheckVerdict,
    pub observed: ObservedOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StepSummaryPayload {
    pub kind: String,
    pub plan_run_id: String,
    pub revision: i64,
    pub step_index: i64,
    pub step_id: String,
    pub attempt: i64,
    pub command: Option<String>,
    pub child_session_id: Option<String>,
    pub resolved_model: Option<String>,
    pub last_assistant_response: Option<String>,
    pub observed: Option<ObservedOutput>,
    pub checks: Vec<CheckOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StepCrashPayload {
    pub kind: String,
    pub plan_run_id: String,
    pub revision: i64,
    pub step_index: i64,
    pub step_id: String,
    pub attempt: i64,
    pub reason: String,
    pub child_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFailureDetails {
    pub step_index: i64,
    pub step_id: String,
    pub attempt: i64,
    pub reason: String,
    pub payload_child_session_id: Option<String>,
    pub active_child_session_update: NullableUpdate<String>,
    pub checks: Vec<CheckOutcome>,
}

pub(crate) fn build_step_call_id(
    plan_run_id: &str,
    revision: i64,
    step_index: i64,
    attempt: i64,
    suffix: &str,
) -> String {
    format!("plan-{plan_run_id}-rev-{revision}-step-{step_index}-attempt-{attempt}-{suffix}")
}

pub(crate) fn artifact_path(session_dir: &Path, call_id: &str) -> PathBuf {
    session_dir
        .join("results")
        .join(format!("{}.txt", safe_call_id_for_filename(call_id)))
}

pub(crate) fn parse_shell_output_text(
    output: &str,
    artifact_path: Option<&Path>,
) -> Result<ObservedOutput> {
    let output = if let Some(path) = artifact_path {
        if path.exists() {
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to read shell artifact {}", path.display()))?
        } else {
            output.to_string()
        }
    } else {
        output.to_string()
    };

    let stdout_prefix = "stdout:\n";
    let stderr_marker = "\nstderr:\n";
    let exit_marker = "\nexit_code=";

    ensure!(
        output.starts_with(stdout_prefix),
        "shell output is missing stdout header"
    );
    let stdout_and_rest = &output[stdout_prefix.len()..];
    let stderr_index = stdout_and_rest
        .find(stderr_marker)
        .context("shell output is missing stderr header")?;
    let stdout = stdout_and_rest[..stderr_index].to_string();
    let stderr_and_rest = &stdout_and_rest[stderr_index + stderr_marker.len()..];
    let exit_index = stderr_and_rest
        .find(exit_marker)
        .context("shell output is missing exit code header")?;
    let stderr = stderr_and_rest[..exit_index].to_string();
    let exit_and_rest = &stderr_and_rest[exit_index + exit_marker.len()..];
    let exit_code_line = exit_and_rest.lines().next().unwrap_or_default();
    let exit_code = if exit_code_line == "signal" {
        None
    } else {
        Some(
            exit_code_line
                .parse::<i32>()
                .context("shell output exit code is not a valid integer")?,
        )
    };

    Ok(ObservedOutput {
        exit_code,
        stdout,
        stderr,
        artifact_path: artifact_path.map(|path| path.display().to_string()),
    })
}

pub(crate) fn evaluate_check(observed: &ObservedOutput, expect: &ShellExpectation) -> CheckVerdict {
    let no_observable_output = observed.exit_code.is_none()
        && observed.stdout.is_empty()
        && observed.stderr.is_empty()
        && observed.artifact_path.is_none();
    if no_observable_output {
        return CheckVerdict::Inconclusive;
    }

    if let Some(expected_exit_code) = expect.exit_code {
        match observed.exit_code {
            Some(actual) if actual == expected_exit_code => {}
            Some(_) => return CheckVerdict::Fail,
            None => return CheckVerdict::Inconclusive,
        }
    }

    if let Some(expected_stdout_contains) = expect.stdout_contains.as_deref()
        && !observed.stdout.contains(expected_stdout_contains)
    {
        return CheckVerdict::Fail;
    }

    if let Some(expected_stderr_contains) = expect.stderr_contains.as_deref()
        && !observed.stderr.contains(expected_stderr_contains)
    {
        return CheckVerdict::Fail;
    }

    if let Some(expected_stdout_equals) = expect.stdout_equals.as_deref()
        && observed.stdout != expected_stdout_equals
    {
        return CheckVerdict::Fail;
    }

    CheckVerdict::Pass
}

fn build_shell_call(command: &str, timeout_ms: Option<u64>, call_id: String) -> ToolCall {
    let arguments = match timeout_ms {
        Some(timeout_ms) => json!({ "command": command, "timeout_ms": timeout_ms }),
        None => json!({ "command": command }),
    };

    ToolCall {
        id: call_id,
        name: "execute".to_string(),
        arguments: arguments.to_string(),
    }
}

fn plan_action_from_run(plan_run: &PlanRun) -> Result<PlanAction> {
    serde_json::from_str(&plan_run.definition_json)
        .context("failed to parse plan run definition_json")
}

fn step_at(plan_action: &PlanAction, index: usize) -> Result<&PlanStepSpec> {
    plan_action
        .steps
        .get(index)
        .ok_or_else(|| anyhow!("plan run current step index is out of range"))
}

fn next_attempt_index(store: &Store, plan_run: &PlanRun, step_index: i64) -> Result<i64> {
    let attempts = store.get_step_attempts(&plan_run.id, step_index)?;
    Ok(attempts
        .into_iter()
        .filter(|attempt| attempt.revision == plan_run.revision)
        .count() as i64)
}

fn observed_output_from_result(
    output: &str,
    session_dir: &Path,
    call_id: &str,
    exit_code: Option<i32>,
) -> ObservedOutput {
    let artifact = artifact_path(session_dir, call_id);
    let artifact_path = artifact.exists().then(|| artifact.display().to_string());
    match parse_shell_output_text(output, None) {
        Ok(mut observed) => {
            observed.artifact_path = artifact_path;
            observed
        }
        Err(_) => ObservedOutput {
            exit_code,
            stdout: String::new(),
            stderr: String::new(),
            artifact_path,
        },
    }
}

async fn run_check(
    turn: &crate::turn::Turn,
    call_id_prefix: &str,
    check: &ShellCheckSpec,
    session: &Session,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> CheckOutcome {
    let call = build_shell_call(
        &check.command,
        None,
        format!("{call_id_prefix}-check-{}", check.id),
    );

    match crate::plan::executor::guarded_shell_execute_call(turn, &call, session, approval_handler)
        .await
    {
        Ok(result) if result.was_denied => CheckOutcome {
            check_id: check.id.clone(),
            verdict: CheckVerdict::Inconclusive,
            observed: ObservedOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                artifact_path: None,
            },
        },
        Ok(result) => {
            let observed = observed_output_from_result(
                &result.output,
                session.sessions_dir(),
                &call.id,
                result.exit_code,
            );
            let evaluation_observed = if let Some(artifact_path) = observed.artifact_path.as_deref()
            {
                let artifact = Path::new(artifact_path);
                parse_shell_output_text(&result.output, Some(artifact))
                    .unwrap_or_else(|_| observed.clone())
            } else {
                observed.clone()
            };
            let verdict = evaluate_check(&evaluation_observed, &check.expect);
            CheckOutcome {
                check_id: check.id.clone(),
                verdict,
                observed,
            }
        }
        Err(_) => CheckOutcome {
            check_id: check.id.clone(),
            verdict: CheckVerdict::Inconclusive,
            observed: ObservedOutput {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                artifact_path: None,
            },
        },
    }
}

pub(crate) async fn run_checks(
    turn: &crate::turn::Turn,
    call_id_prefix: &str,
    checks: &[ShellCheckSpec],
    session: &Session,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Vec<CheckOutcome> {
    let mut outcomes = Vec::with_capacity(checks.len());
    for check in checks {
        outcomes.push(run_check(turn, call_id_prefix, check, session, approval_handler).await);
    }
    outcomes
}

fn serialize_json<T: Serialize>(value: &T, label: &str) -> Result<String> {
    serde_json::to_string(value).with_context(|| format!("failed to serialize {label}"))
}

fn finalize_attempt(
    store: &mut Store,
    attempt_id: i64,
    terminal_status: &str,
    summary_json: &str,
    checks_json: &str,
) -> Result<()> {
    store.finalize_step_attempt(
        attempt_id,
        terminal_status,
        &utc_timestamp(),
        summary_json,
        checks_json,
    )
}

#[allow(clippy::too_many_arguments)]
async fn handle_spawn_missing_metadata(
    store: &mut Store,
    plan_run: &PlanRun,
    step_index: i64,
    step_id: &str,
    attempt: i64,
    spawn_result: &crate::spawn::SpawnResult,
    attempt_id: i64,
) -> Result<StepOutcome> {
    let failure = build_waiting_t2_failure_details(
        step_index,
        step_id,
        attempt,
        "spawn_failed",
        Some(spawn_result.child_session_id.clone()),
        NullableUpdate::Value(spawn_result.child_session_id.clone()),
        Vec::new(),
    );
    let summary = build_step_summary(
        "plan_step_summary",
        plan_run,
        step_index,
        step_id,
        attempt,
        None,
        Some(spawn_result.child_session_id.clone()),
        Some(spawn_result.resolved_model.clone()),
        None,
        None,
        Vec::new(),
    );
    let summary_json = serialize_json(&summary, "step summary")?;
    let checks_json = "[]".to_string();
    store
        .update_step_attempt_child_session(attempt_id, Some(&spawn_result.child_session_id))
        .context("failed to record spawned child session")?;
    finalize_attempt(store, attempt_id, "crashed", &summary_json, &checks_json)
        .context("failed to finalize crashed spawn attempt")?;
    Ok(StepOutcome::WaitingT2 { failure })
}

#[allow(clippy::too_many_arguments)]
fn build_step_summary(
    kind: &str,
    plan_run: &PlanRun,
    step_index: i64,
    step_id: &str,
    attempt: i64,
    command: Option<String>,
    child_session_id: Option<String>,
    resolved_model: Option<String>,
    last_assistant_response: Option<String>,
    observed: Option<ObservedOutput>,
    checks: Vec<CheckOutcome>,
) -> StepSummaryPayload {
    StepSummaryPayload {
        kind: kind.to_string(),
        plan_run_id: plan_run.id.clone(),
        revision: plan_run.revision,
        step_index,
        step_id: step_id.to_string(),
        attempt,
        command,
        child_session_id,
        resolved_model,
        last_assistant_response,
        observed,
        checks,
    }
}

fn build_step_crash(
    plan_run: &PlanRun,
    step_index: i64,
    step_id: &str,
    attempt: i64,
    reason: &str,
    child_session_id: Option<String>,
) -> StepCrashPayload {
    StepCrashPayload {
        kind: "plan_step_crash".to_string(),
        plan_run_id: plan_run.id.clone(),
        revision: plan_run.revision,
        step_index,
        step_id: step_id.to_string(),
        attempt,
        reason: reason.to_string(),
        child_session_id,
    }
}

fn build_waiting_t2_failure_details(
    step_index: i64,
    step_id: &str,
    attempt: i64,
    reason: &str,
    payload_child_session_id: Option<String>,
    active_child_session_update: NullableUpdate<String>,
    checks: Vec<CheckOutcome>,
) -> PlanFailureDetails {
    PlanFailureDetails {
        step_index,
        step_id: step_id.to_string(),
        attempt,
        reason: reason.to_string(),
        payload_child_session_id,
        active_child_session_update,
        checks,
    }
}

fn step_result_to_failure_reason(verdicts: &[CheckOutcome]) -> Option<&'static str> {
    if verdicts
        .iter()
        .any(|check| check.verdict == CheckVerdict::Fail)
    {
        Some("check_failed")
    } else if verdicts
        .iter()
        .any(|check| check.verdict == CheckVerdict::Inconclusive)
    {
        Some("check_inconclusive")
    } else {
        None
    }
}

pub async fn run_plan_step<F, Fut, P, TS>(
    store: &mut Store,
    config: &Config,
    session_dir: &Path,
    plan_run: &PlanRun,
    _make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<StepOutcome>
where
    F: FnMut(&Config) -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: crate::llm::LlmProvider,
    TS: TokenSink + Send,
{
    let plan_action = plan_action_from_run(plan_run)?;
    ensure!(
        plan_run.current_step_index >= 0,
        "plan run current step index must not be negative"
    );

    let step_index = plan_run.current_step_index;
    let current_step_index = step_index as usize;
    if current_step_index > plan_action.steps.len() {
        return Err(anyhow!("plan run current step index is out of range"));
    }
    if current_step_index == plan_action.steps.len() {
        let _ = store
            .update_plan_run_status_preserving_failed(
                &plan_run.id,
                "completed",
                PlanRunUpdateFields {
                    current_step_index: Some(step_index),
                    last_failure_json: NullableUpdate::Null,
                    active_child_session_id: NullableUpdate::Null,
                    ..PlanRunUpdateFields::default()
                },
            )
            .context("failed to mark completed plan run")?;
        return Ok(StepOutcome::Completed);
    }

    let preexisting_running_attempts = store
        .get_step_attempts(&plan_run.id, step_index)?
        .into_iter()
        .any(|attempt| attempt.revision == plan_run.revision && attempt.status == "running");
    if preexisting_running_attempts {
        let failure = crash_plan_run_to_waiting_t2(store, plan_run)
            .context("failed to hand off preexisting running plan step attempts")?;
        return Ok(StepOutcome::WaitingT2 { failure });
    }

    let step = step_at(&plan_action, current_step_index)?;
    let attempt = next_attempt_index(store, plan_run, step_index)?;
    let parent_session_dir = session_dir.join(&plan_run.owner_session_id);
    let mut session = Session::new(&parent_session_dir).context("failed to open plan session")?;
    session
        .load_today()
        .context("failed to load plan session history")?;
    let shell_turn = build_t3_turn(config);

    match step {
        PlanStepSpec::Shell {
            id,
            command,
            timeout_ms,
            checks,
            ..
        } => {
            let initial_checks_json = "[]".to_string();
            let initial_summary = build_step_summary(
                "plan_step_summary",
                plan_run,
                step_index,
                id,
                attempt,
                Some(command.clone()),
                None,
                None,
                None,
                None,
                Vec::new(),
            );
            let initial_summary_json = serialize_json(&initial_summary, "step summary")?;
            let attempt_id = store.record_step_attempt(StepAttemptRecord {
                plan_run_id: plan_run.id.clone(),
                revision: plan_run.revision,
                step_index,
                step_id: id.clone(),
                attempt,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: initial_summary_json.clone(),
                checks_json: initial_checks_json.clone(),
            })?;
            let call_id = build_step_call_id(
                &plan_run.id,
                plan_run.revision,
                step_index,
                attempt,
                "shell",
            );
            let call = build_shell_call(command, *timeout_ms, call_id);
            let result = match crate::plan::executor::guarded_shell_execute_call(
                &shell_turn,
                &call,
                &session,
                approval_handler,
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    let crash = build_step_crash(
                        plan_run,
                        step_index,
                        id,
                        attempt,
                        &error.to_string(),
                        None,
                    );
                    let crash_json = serialize_json(&crash, "step crash")?;
                    let summary = build_step_summary(
                        "plan_step_summary",
                        plan_run,
                        step_index,
                        id,
                        attempt,
                        Some(command.clone()),
                        None,
                        None,
                        None,
                        None,
                        Vec::new(),
                    );
                    let summary_json = serialize_json(&summary, "step summary")?;
                    let checks_json = initial_checks_json.clone();
                    store
                        .update_plan_run_status_preserving_failed(
                            &plan_run.id,
                            "failed",
                            PlanRunUpdateFields {
                                current_step_index: Some(step_index),
                                last_failure_json: NullableUpdate::Value(crash_json),
                                active_child_session_id: NullableUpdate::Null,
                                ..PlanRunUpdateFields::default()
                            },
                        )
                        .context("failed to mark crashed shell step failed")?;
                    finalize_attempt(store, attempt_id, "crashed", &summary_json, &checks_json)
                        .context("failed to finalize crashed shell attempt")?;
                    return Ok(StepOutcome::Failed);
                }
            };

            if result.was_denied {
                let checks = Vec::new();
                let checks_json = serialize_json(&checks, "checks")?;
                let failure = build_waiting_t2_failure_details(
                    step_index,
                    id,
                    attempt,
                    "shell_denied",
                    None,
                    NullableUpdate::Null,
                    checks.clone(),
                );
                let summary = build_step_summary(
                    "plan_step_summary",
                    plan_run,
                    step_index,
                    id,
                    attempt,
                    Some(command.clone()),
                    None,
                    None,
                    None,
                    None,
                    checks,
                );
                let summary_json = serialize_json(&summary, "step summary")?;
                finalize_attempt(store, attempt_id, "failed", &summary_json, &checks_json)
                    .context("failed to finalize denied shell attempt")?;
                return Ok(StepOutcome::WaitingT2 { failure });
            }

            let observed = observed_output_from_result(
                &result.output,
                session.sessions_dir(),
                &call.id,
                result.exit_code,
            );
            let checks_call_id_prefix = build_step_call_id(
                &plan_run.id,
                plan_run.revision,
                step_index,
                attempt,
                "checks",
            );
            let checks = run_checks(
                &shell_turn,
                &checks_call_id_prefix,
                checks,
                &session,
                approval_handler,
            )
            .await;
            let failure_reason = step_result_to_failure_reason(&checks);
            let summary = build_step_summary(
                "plan_step_summary",
                plan_run,
                step_index,
                id,
                attempt,
                Some(command.clone()),
                None,
                None,
                None,
                Some(observed.clone()),
                checks.clone(),
            );
            let summary_json = serialize_json(&summary, "step summary")?;
            let checks_json = serialize_json(&checks, "checks")?;

            if let Some(reason) = failure_reason {
                let failure = build_waiting_t2_failure_details(
                    step_index,
                    id,
                    attempt,
                    reason,
                    None,
                    NullableUpdate::Null,
                    checks.clone(),
                );
                finalize_attempt(store, attempt_id, "failed", &summary_json, &checks_json)
                    .context("failed to finalize failed shell attempt")?;
                return Ok(StepOutcome::WaitingT2 { failure });
            }

            let completed = current_step_index + 1 >= plan_action.steps.len();
            store
                .update_plan_run_status_preserving_failed(
                    &plan_run.id,
                    if completed { "completed" } else { "pending" },
                    PlanRunUpdateFields {
                        current_step_index: Some(step_index + 1),
                        last_failure_json: NullableUpdate::Null,
                        active_child_session_id: NullableUpdate::Null,
                        ..PlanRunUpdateFields::default()
                    },
                )
                .context("failed to advance shell step")?;
            finalize_attempt(store, attempt_id, "passed", &summary_json, &checks_json)
                .context("failed to finalize passed shell attempt")?;
            Ok(if completed {
                StepOutcome::Completed
            } else {
                StepOutcome::Advanced
            })
        }
        PlanStepSpec::Spawn {
            id, spawn, checks, ..
        } => {
            let initial_checks_json = "[]".to_string();
            let initial_summary = build_step_summary(
                "plan_step_summary",
                plan_run,
                step_index,
                id,
                attempt,
                None,
                None,
                None,
                None,
                None,
                Vec::new(),
            );
            let initial_summary_json = serialize_json(&initial_summary, "step summary")?;
            let attempt_id = store.record_step_attempt(StepAttemptRecord {
                plan_run_id: plan_run.id.clone(),
                revision: plan_run.revision,
                step_index,
                step_id: id.clone(),
                attempt,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: initial_summary_json.clone(),
                checks_json: initial_checks_json.clone(),
            })?;
            let spawn_request = SpawnRequest {
                parent_session_id: plan_run.owner_session_id.clone(),
                task: spawn.task.clone(),
                task_kind: spawn.task_kind.clone(),
                tier: Some(spawn.tier.clone()),
                model_override: spawn.model_override.clone(),
                reasoning_override: spawn.reasoning_override.clone(),
                skills: spawn.skills.clone(),
                skill_token_budget: spawn.skill_token_budget,
            };
            let parent_budget = session
                .budget_snapshot()
                .context("failed to read parent budget snapshot")?;
            let spawn_result =
                match crate::spawn::spawn_child(store, config, parent_budget, spawn_request) {
                    Ok(result) => result,
                    Err(_error) => {
                        let failure = build_waiting_t2_failure_details(
                            step_index,
                            id,
                            attempt,
                            "spawn_failed",
                            None,
                            NullableUpdate::Null,
                            Vec::new(),
                        );
                        let summary = build_step_summary(
                            "plan_step_summary",
                            plan_run,
                            step_index,
                            id,
                            attempt,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Vec::new(),
                        );
                        let summary_json = serialize_json(&summary, "step summary")?;
                        let checks_json = initial_checks_json.clone();
                        finalize_attempt(store, attempt_id, "crashed", &summary_json, &checks_json)
                            .context("failed to finalize crashed spawn attempt")?;
                        return Ok(StepOutcome::WaitingT2 { failure });
                    }
                };
            let metadata_json = maybe_force_missing_spawn_metadata(
                store.get_session_metadata(&spawn_result.child_session_id)?,
            );
            let Some(metadata_json) = metadata_json else {
                return handle_spawn_missing_metadata(
                    store,
                    plan_run,
                    step_index,
                    id,
                    attempt,
                    &spawn_result,
                    attempt_id,
                )
                .await;
            };
            let context = SpawnDrainContext {
                store,
                config,
                session_dir,
                spawn_result: spawn_result.clone(),
            };
            let drain_result = match finish_spawned_child_drain(
                context,
                &metadata_json,
                _make_provider,
                token_sink,
                approval_handler,
            )
            .await
            {
                Ok(result) => result,
                Err(_error) => {
                    let failure = build_waiting_t2_failure_details(
                        step_index,
                        id,
                        attempt,
                        "spawn_failed",
                        Some(spawn_result.child_session_id.clone()),
                        NullableUpdate::Value(spawn_result.child_session_id.clone()),
                        Vec::new(),
                    );
                    let summary = build_step_summary(
                        "plan_step_summary",
                        plan_run,
                        step_index,
                        id,
                        attempt,
                        None,
                        Some(spawn_result.child_session_id.clone()),
                        Some(spawn_result.resolved_model.clone()),
                        None,
                        None,
                        Vec::new(),
                    );
                    let summary_json = serialize_json(&summary, "step summary")?;
                    let checks_json = initial_checks_json.clone();
                    store
                        .update_step_attempt_child_session(
                            attempt_id,
                            Some(&spawn_result.child_session_id),
                        )
                        .context("failed to record spawned child session")?;
                    finalize_attempt(store, attempt_id, "crashed", &summary_json, &checks_json)
                        .context("failed to finalize crashed spawn attempt")?;
                    return Ok(StepOutcome::WaitingT2 { failure });
                }
            };
            let checks_call_id_prefix = build_step_call_id(
                &plan_run.id,
                plan_run.revision,
                step_index,
                attempt,
                "checks",
            );
            let checks = run_checks(
                &shell_turn,
                &checks_call_id_prefix,
                checks,
                &session,
                approval_handler,
            )
            .await;
            let failure_reason = step_result_to_failure_reason(&checks);
            let summary = build_step_summary(
                "plan_step_summary",
                plan_run,
                step_index,
                id,
                attempt,
                None,
                Some(drain_result.child_session_id.clone()),
                Some(drain_result.resolved_model.clone()),
                drain_result.last_assistant_response.clone(),
                None,
                checks.clone(),
            );
            let summary_json = serialize_json(&summary, "step summary")?;
            let checks_json = serialize_json(&checks, "checks")?;

            if let Some(reason) = failure_reason {
                let failure = build_waiting_t2_failure_details(
                    step_index,
                    id,
                    attempt,
                    reason,
                    Some(drain_result.child_session_id.clone()),
                    NullableUpdate::Null,
                    checks.clone(),
                );
                store
                    .update_step_attempt_child_session(
                        attempt_id,
                        Some(&drain_result.child_session_id),
                    )
                    .context("failed to record spawned child session")?;
                finalize_attempt(store, attempt_id, "failed", &summary_json, &checks_json)
                    .context("failed to finalize failed spawn attempt")?;
                return Ok(StepOutcome::WaitingT2 { failure });
            }

            let completed = current_step_index + 1 >= plan_action.steps.len();
            store
                .update_plan_run_status_preserving_failed(
                    &plan_run.id,
                    if completed { "completed" } else { "pending" },
                    PlanRunUpdateFields {
                        current_step_index: Some(step_index + 1),
                        last_failure_json: NullableUpdate::Null,
                        active_child_session_id: NullableUpdate::Null,
                        ..PlanRunUpdateFields::default()
                    },
                )
                .context("failed to advance spawn step")?;
            store
                .update_step_attempt_child_session(attempt_id, Some(&drain_result.child_session_id))
                .context("failed to record spawned child session")?;
            finalize_attempt(store, attempt_id, "passed", &summary_json, &checks_json)
                .context("failed to finalize passed spawn attempt")?;
            Ok(if completed {
                StepOutcome::Completed
            } else {
                StepOutcome::Advanced
            })
        }
    }
}

pub async fn tick_plan_runner<F, Fut, P, TS>(
    store: &mut Store,
    config: &Config,
    session_dir: &Path,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut (dyn ApprovalHandler + Send),
) -> Result<Option<StepOutcome>>
where
    F: FnMut(&Config) -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: crate::llm::LlmProvider,
    TS: TokenSink + Send,
{
    let Some(plan_run) =
        store.claim_next_runnable_plan_run(config.queue.stale_processing_timeout_secs)?
    else {
        return Ok(None);
    };

    let outcome = run_plan_step(
        store,
        config,
        session_dir,
        &plan_run,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await;

    match outcome {
        Ok(StepOutcome::WaitingT2 { failure }) => {
            let should_notify = store
                .get_plan_run(&plan_run.id)?
                .map(|current| current.status != "waiting_t2")
                .unwrap_or(true);
            if should_notify {
                notify_plan_failure(store, &plan_run, &failure)
                    .context("failed to notify T2 of plan failure")?;
            }
            Ok(Some(StepOutcome::WaitingT2 { failure }))
        }
        Ok(outcome) => {
            store.release_plan_run_claim(&plan_run.id)?;
            Ok(Some(outcome))
        }
        Err(error) => {
            let failure_json = serde_json::json!({
                "kind": "runner_internal_error",
                "plan_run_id": plan_run.id,
                "message": error.to_string(),
            })
            .to_string();
            if let Err(cleanup_error) = store.update_plan_run_status_preserving_failed(
                &plan_run.id,
                "failed",
                PlanRunUpdateFields {
                    last_failure_json: NullableUpdate::Value(failure_json),
                    active_child_session_id: NullableUpdate::Null,
                    ..PlanRunUpdateFields::default()
                },
            ) {
                return Err(cleanup_error.context(format!(
                    "failed to mark plan run failed after runner error: {error}"
                )));
            }
            if let Err(cleanup_error) = store.release_plan_run_claim(&plan_run.id) {
                return Err(cleanup_error.context(format!(
                    "failed to release plan run claim after runner error: {error}"
                )));
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::common::{
        ChatMessage, MessageContent, Principal, StaticProvider, StopReason, StreamedTurn,
        spawned_t3_test_config, temp_sessions_dir,
    };
    use crate::plan::PlanActionKind;
    use crate::session::Session;
    use crate::skills::SkillCatalog;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aprs_plan_runner_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ))
    }

    #[test]
    fn build_step_call_id_includes_all_parts() {
        let call_id = build_step_call_id("plan-1", 3, 2, 5, "check-1");
        assert_eq!(call_id, "plan-plan-1-rev-3-step-2-attempt-5-check-1");
    }

    #[test]
    fn artifact_path_sanitizes_call_id() {
        let dir = temp_dir("artifact_path");
        let path = artifact_path(&dir, "../../escape");
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert!(file_name.starts_with("call_"));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("txt"));
    }

    #[test]
    fn parse_shell_output_text_parses_stdout_stderr_and_exit_code() {
        let output = "stdout:\nhello\nstderr:\nwarn\nexit_code=7";
        let parsed = parse_shell_output_text(output, None).unwrap();
        assert_eq!(parsed.exit_code, Some(7));
        assert_eq!(parsed.stdout, "hello");
        assert_eq!(parsed.stderr, "warn");
        assert!(parsed.artifact_path.is_none());
    }

    #[test]
    fn parse_shell_output_text_reads_artifact_when_present() {
        let dir = temp_dir("artifact_read");
        let path = artifact_path(&dir, "call-1");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "stdout:\nfrom file\nstderr:\n\nexit_code=0").unwrap();
        let parsed = parse_shell_output_text(
            "stdout:\nignored\nstderr:\nignored\nexit_code=1",
            Some(&path),
        )
        .unwrap();
        assert_eq!(parsed.exit_code, Some(0));
        assert_eq!(parsed.stdout, "from file");
        assert_eq!(parsed.stderr, "");
        assert_eq!(
            parsed.artifact_path.as_deref(),
            Some(path.display().to_string().as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evaluate_check_passes_when_all_expectations_match() {
        let observed = ObservedOutput {
            exit_code: Some(0),
            stdout: "alpha beta".to_string(),
            stderr: "gamma".to_string(),
            artifact_path: None,
        };
        let expect = ShellExpectation {
            exit_code: Some(0),
            stdout_contains: Some("alpha".to_string()),
            stderr_contains: Some("gam".to_string()),
            stdout_equals: None,
        };
        assert_eq!(evaluate_check(&observed, &expect), CheckVerdict::Pass);
    }

    #[test]
    fn evaluate_check_fails_on_mismatch() {
        let observed = ObservedOutput {
            exit_code: Some(1),
            stdout: "alpha beta".to_string(),
            stderr: String::new(),
            artifact_path: None,
        };
        let expect = ShellExpectation {
            exit_code: Some(0),
            stdout_contains: None,
            stderr_contains: None,
            stdout_equals: None,
        };
        assert_eq!(evaluate_check(&observed, &expect), CheckVerdict::Fail);
    }

    #[test]
    fn evaluate_check_is_inconclusive_without_observable_output() {
        let observed = ObservedOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            artifact_path: None,
        };
        let expect = ShellExpectation {
            exit_code: Some(0),
            stdout_contains: Some("hello".to_string()),
            stderr_contains: None,
            stdout_equals: None,
        };
        assert_eq!(
            evaluate_check(&observed, &expect),
            CheckVerdict::Inconclusive
        );
    }

    fn child_turn(text: &str) -> StreamedTurn {
        StreamedTurn {
            assistant_message: ChatMessage {
                role: crate::llm::ChatRole::Assistant,
                principal: Principal::Agent,
                content: vec![MessageContent::text(text.to_string())],
            },
            tool_calls: vec![],
            meta: Some(crate::llm::TurnMeta {
                model: Some("gpt-child".to_string()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                reasoning_tokens: None,
                reasoning_trace: None,
            }),
            stop_reason: StopReason::Stop,
        }
    }

    fn test_config(root: &Path) -> crate::config::Config {
        let skills_dir = root.join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();
        spawned_t3_test_config(skills_dir, SkillCatalog::default())
    }

    fn shell_plan(command: &str, check_command: &str, check_stdout: &str) -> PlanAction {
        PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Shell {
                id: "step-shell".to_string(),
                command: command.to_string(),
                timeout_ms: None,
                checks: vec![ShellCheckSpec {
                    id: "check-shell".to_string(),
                    command: check_command.to_string(),
                    expect: ShellExpectation {
                        exit_code: Some(0),
                        stdout_contains: Some(check_stdout.to_string()),
                        stderr_contains: None,
                        stdout_equals: None,
                    },
                }],
                max_attempts: 1,
            }],
        }
    }

    fn spawn_plan(check_stdout: &str) -> PlanAction {
        PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![PlanStepSpec::Spawn {
                id: "step-spawn".to_string(),
                spawn: crate::plan::SpawnStepSpec {
                    task: "do the child task".to_string(),
                    task_kind: None,
                    tier: "t3".to_string(),
                    model_override: None,
                    reasoning_override: None,
                    skills: vec![],
                    skill_token_budget: None,
                },
                checks: vec![ShellCheckSpec {
                    id: "check-spawn".to_string(),
                    command: "echo child-ok".to_string(),
                    expect: ShellExpectation {
                        exit_code: Some(0),
                        stdout_contains: Some(check_stdout.to_string()),
                        stderr_contains: None,
                        stdout_equals: None,
                    },
                }],
                max_attempts: 1,
            }],
        }
    }

    fn insert_plan_run(
        store: &mut Store,
        owner_session_id: &str,
        plan_run_id: &str,
        action: &PlanAction,
    ) -> PlanRun {
        let definition_json = serde_json::to_string(action).unwrap();
        store.create_session(owner_session_id, Some("{}")).unwrap();
        store
            .create_plan_run(
                plan_run_id,
                owner_session_id,
                &definition_json,
                Some("topic"),
                Some("cli"),
            )
            .unwrap();
        store.get_plan_run(plan_run_id).unwrap().unwrap()
    }

    #[tokio::test]
    async fn tick_plan_runner_advances_shell_step_and_releases_claim() {
        let root = temp_sessions_dir("tick_shell");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![
                PlanStepSpec::Shell {
                    id: "step-shell-1".to_string(),
                    command: "echo shell-ok-1".to_string(),
                    timeout_ms: None,
                    checks: vec![ShellCheckSpec {
                        id: "check-shell-1".to_string(),
                        command: "echo shell-ok-1".to_string(),
                        expect: ShellExpectation {
                            exit_code: Some(0),
                            stdout_contains: Some("shell-ok-1".to_string()),
                            stderr_contains: None,
                            stdout_equals: None,
                        },
                    }],
                    max_attempts: 1,
                },
                PlanStepSpec::Shell {
                    id: "step-shell-2".to_string(),
                    command: "echo shell-ok-2".to_string(),
                    timeout_ms: None,
                    checks: vec![ShellCheckSpec {
                        id: "check-shell-2".to_string(),
                        command: "echo shell-ok-2".to_string(),
                        expect: ShellExpectation {
                            exit_code: Some(0),
                            stdout_contains: Some("shell-ok-2".to_string()),
                            stderr_contains: None,
                            stdout_equals: None,
                        },
                    }],
                    max_attempts: 1,
                },
            ],
        };
        let plan_run = insert_plan_run(&mut store, owner_session_id, "plan-shell", &plan_action);
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = tick_plan_runner(
            &mut store,
            &config,
            &root,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(outcome, StepOutcome::Advanced);
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "pending");
        assert_eq!(updated.current_step_index, 1);
        assert_eq!(updated.claimed_at, None);

        let attempts = store.get_step_attempts(&plan_run.id, 0).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "passed");
    }

    #[tokio::test]
    async fn run_plan_step_spawn_step_records_child_and_advances() {
        let root = temp_sessions_dir("spawn_step");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-spawn",
            &spawn_plan("child-ok"),
        );
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child complete"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert_eq!(outcome, StepOutcome::Completed);
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "completed");
        assert_eq!(updated.current_step_index, 1);
        assert_eq!(updated.active_child_session_id, None);

        let attempts = store.get_step_attempts(&plan_run.id, 0).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "passed");
        let summary: StepSummaryPayload = serde_json::from_str(&attempts[0].summary_json).unwrap();
        assert_eq!(attempts[0].child_session_id, summary.child_session_id);
        assert_eq!(summary.resolved_model.as_deref(), Some("gpt-child"));
        assert_eq!(
            summary.last_assistant_response.as_deref(),
            Some("child complete")
        );
    }

    #[tokio::test]
    async fn run_plan_step_shell_denial_returns_waiting_t2() {
        let root = temp_sessions_dir("shell_denial");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-denied",
            &shell_plan("echo shell-ok", "echo shell-ok", "shell-ok"),
        );
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| false;

        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        crate::plan::notify::notify_plan_failure(&mut store, &plan_run, &failure).unwrap();
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        let failure_json = updated.last_failure_json.as_deref().unwrap();
        let failure_payload: serde_json::Value = serde_json::from_str(failure_json).unwrap();
        assert_eq!(failure.reason, "shell_denied");
        assert_eq!(failure_payload["reason"], "shell_denied");
    }

    #[tokio::test]
    async fn run_plan_step_spawn_drain_failure_returns_waiting_t2() {
        let root = temp_sessions_dir("spawn_drain_failure");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-spawn-drain",
            &spawn_plan("child-ok"),
        );
        let mut make_provider = |_config: &crate::config::Config| async move {
            Err::<StaticProvider, anyhow::Error>(anyhow::Error::msg("provider error"))
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        crate::plan::notify::notify_plan_failure(&mut store, &plan_run, &failure).unwrap();
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        let failure_json = updated.last_failure_json.as_deref().unwrap();
        let failure_payload: serde_json::Value = serde_json::from_str(failure_json).unwrap();
        assert_eq!(failure.reason, "spawn_failed");
        assert_eq!(failure_payload["reason"], "spawn_failed");
    }

    #[tokio::test]
    async fn run_plan_step_spawn_missing_metadata_returns_waiting_t2() {
        let root = temp_sessions_dir("spawn_missing_metadata_route");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-spawn-metadata",
            &spawn_plan("child-ok"),
        );
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child complete"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        struct ResetFlag;
        impl Drop for ResetFlag {
            fn drop(&mut self) {
                set_force_missing_spawn_metadata(false);
            }
        }

        let _reset_flag = ResetFlag;
        set_force_missing_spawn_metadata(true);
        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        crate::plan::notify::notify_plan_failure(&mut store, &plan_run, &failure).unwrap();
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(updated.current_step_index, 0);
        assert_eq!(failure.reason, "spawn_failed");
        assert!(updated.active_child_session_id.is_some());
    }

    #[tokio::test]
    async fn run_plan_step_detects_preexisting_running_attempt_and_returns_waiting_t2() {
        let root = temp_sessions_dir("stale_attempt_resume");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_action = PlanAction {
            kind: PlanActionKind::Plan,
            plan_run_id: None,
            replace_from_step: None,
            note: None,
            steps: vec![
                PlanStepSpec::Shell {
                    id: "step-one".to_string(),
                    command: "echo step-one".to_string(),
                    timeout_ms: None,
                    checks: vec![ShellCheckSpec {
                        id: "check-one".to_string(),
                        command: "echo step-one".to_string(),
                        expect: ShellExpectation {
                            exit_code: Some(0),
                            stdout_contains: Some("step-one".to_string()),
                            stderr_contains: None,
                            stdout_equals: None,
                        },
                    }],
                    max_attempts: 1,
                },
                PlanStepSpec::Shell {
                    id: "step-two".to_string(),
                    command: "echo step-two".to_string(),
                    timeout_ms: None,
                    checks: vec![ShellCheckSpec {
                        id: "check-two".to_string(),
                        command: "echo step-two".to_string(),
                        expect: ShellExpectation {
                            exit_code: Some(0),
                            stdout_contains: Some("step-two".to_string()),
                            stderr_contains: None,
                            stdout_equals: None,
                        },
                    }],
                    max_attempts: 1,
                },
            ],
        };
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-stale-attempt",
            &plan_action,
        );
        let stale_attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: plan_run.id.clone(),
                revision: plan_run.revision,
                step_index: 0,
                step_id: "step-one".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();
        store
            .update_plan_run_status_preserving_failed(
                &plan_run.id,
                "running",
                PlanRunUpdateFields {
                    current_step_index: Some(0),
                    ..Default::default()
                },
            )
            .unwrap();
        let resumed_plan_run = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(resumed_plan_run.current_step_index, 0);

        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child complete"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &resumed_plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        assert!(matches!(outcome, StepOutcome::WaitingT2 { .. }));
        let attempts = store.get_step_attempts(&plan_run.id, 0).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].id, stale_attempt_id);
        assert_eq!(attempts[0].status, "crashed");
        assert!(attempts[0].finished_at.is_some());
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(updated.current_step_index, 0);
    }

    #[tokio::test]
    async fn run_plan_step_check_failure_returns_waiting_t2() {
        let root = temp_sessions_dir("shell_failure");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-failure",
            &shell_plan("echo shell-ok", "echo shell-ok", "not-present"),
        );
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = run_plan_step(
            &mut store,
            &config,
            &root,
            &plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        crate::plan::notify::notify_plan_failure(&mut store, &plan_run, &failure).unwrap();
        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(updated.current_step_index, 0);
        let failure_json = updated.last_failure_json.as_deref().unwrap();
        let failure_payload: serde_json::Value = serde_json::from_str(failure_json).unwrap();
        assert_eq!(failure.reason, "check_failed");
        assert_eq!(failure_payload["kind"], "plan_failure");
        assert_eq!(failure_payload["reason"], "check_failed");
        assert_eq!(failure_payload["step_id"], "step-shell");

        let attempts = store.get_step_attempts(&plan_run.id, 0).unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "failed");
    }

    #[tokio::test]
    async fn tick_plan_runner_notifies_t2_after_waiting_for_failure() {
        let root = temp_sessions_dir("tick_waiting_t2_notify");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-tick-waiting-t2",
            &shell_plan("echo shell-ok", "echo shell-ok", "not-present"),
        );
        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let outcome = tick_plan_runner(
            &mut store,
            &config,
            &root,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .unwrap()
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        assert_eq!(failure.reason, "check_failed");

        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(updated.claimed_at, None);

        let notification = store
            .dequeue_next_message(owner_session_id)
            .unwrap()
            .unwrap();
        assert_eq!(notification.role, "user");
        assert_eq!(notification.source, format!("agent-plan-{}", plan_run.id));
        let payload: serde_json::Value = serde_json::from_str(&notification.content).unwrap();
        assert_eq!(payload["kind"], "plan_failure");
        assert_eq!(payload["plan_run_id"], plan_run.id);
        assert_eq!(payload["reason"], "check_failed");
        assert_eq!(payload["step_id"], "step-shell");
        assert_eq!(payload["step_index"], 0);
        assert_eq!(payload["attempt"], 0);
    }

    #[tokio::test]
    async fn handle_spawn_missing_metadata_records_child_session_on_attempt() {
        let root = temp_sessions_dir("spawn_missing_metadata");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let mut parent_session = Session::new(&owner_session_dir).unwrap();
        parent_session.load_today().unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-spawn-metadata",
            &spawn_plan("child-ok"),
        );
        let parent_budget = parent_session.budget_snapshot().unwrap();
        let spawn_request = crate::spawn::SpawnRequest {
            parent_session_id: owner_session_id.to_string(),
            task: "do the child task".to_string(),
            task_kind: None,
            tier: Some("t3".to_string()),
            model_override: None,
            reasoning_override: None,
            skills: vec![],
            skill_token_budget: None,
        };
        let spawn_result =
            crate::spawn::spawn_child(&mut store, &config, parent_budget, spawn_request)
                .expect("spawn child should succeed");
        let attempt_id = store
            .record_step_attempt(StepAttemptRecord {
                plan_run_id: plan_run.id.clone(),
                revision: plan_run.revision,
                step_index: 0,
                step_id: "step-spawn".to_string(),
                attempt: 0,
                status: "running".to_string(),
                child_session_id: None,
                summary_json: "{}".to_string(),
                checks_json: "[]".to_string(),
            })
            .unwrap();

        let outcome = handle_spawn_missing_metadata(
            &mut store,
            &plan_run,
            0,
            "step-spawn",
            0,
            &spawn_result,
            attempt_id,
        )
        .await
        .unwrap();

        let failure = match outcome {
            StepOutcome::WaitingT2 { failure } => failure,
            other => panic!("expected waiting_t2, got {other:?}"),
        };
        assert_eq!(failure.reason, "spawn_failed");
        crate::plan::notify::notify_plan_failure(&mut store, &plan_run, &failure).unwrap();

        let updated = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(updated.status, "waiting_t2");
        assert_eq!(
            updated.active_child_session_id.as_deref(),
            Some(spawn_result.child_session_id.as_str())
        );

        let attempts = store.get_step_attempts(&plan_run.id, 0).unwrap();
        assert_eq!(
            attempts[0].child_session_id.as_deref(),
            Some(spawn_result.child_session_id.as_str())
        );
    }

    #[tokio::test]
    async fn run_plan_step_rejects_step_index_past_end() {
        let root = temp_sessions_dir("step_index_past_end");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-past-end",
            &shell_plan("echo shell-ok", "echo shell-ok", "shell-ok"),
        );
        let mut invalid_plan_run = plan_run.clone();
        invalid_plan_run.current_step_index = 2;

        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let err = run_plan_step(
            &mut store,
            &config,
            &root,
            &invalid_plan_run,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .expect_err("step index past the end should fail");
        assert!(err.to_string().contains("out of range"));

        let stored = store.get_plan_run(&plan_run.id).unwrap().unwrap();
        assert_eq!(stored.status, "pending");
        assert_eq!(stored.current_step_index, 0);
        assert_eq!(stored.claimed_at, None);
    }

    #[tokio::test]
    async fn tick_plan_runner_marks_internal_error_failed_and_releases_claim() {
        let root = temp_sessions_dir("tick_internal_error");
        let config = test_config(&root);
        let mut store = Store::new(root.join("store.sqlite")).unwrap();
        let owner_session_id = "owner";
        let owner_session_dir = root.join(owner_session_id);
        std::fs::create_dir_all(&owner_session_dir).unwrap();
        let _session = Session::new(&owner_session_dir).unwrap();
        let _plan_run = insert_plan_run(
            &mut store,
            owner_session_id,
            "plan-bad",
            &shell_plan("echo shell-ok", "echo shell-ok", "shell-ok"),
        );
        store
            .update_plan_run_status_preserving_failed(
                "plan-bad",
                "pending",
                PlanRunUpdateFields {
                    definition_json: Some("{".to_string()),
                    ..PlanRunUpdateFields::default()
                },
            )
            .unwrap();

        let mut make_provider = |_config: &crate::config::Config| {
            let provider = StaticProvider {
                turn: child_turn("child ok"),
            };
            async move { Ok::<StaticProvider, anyhow::Error>(provider) }
        };
        let mut token_sink = |_token: String| {};
        let mut approval_handler =
            |_severity: &crate::gate::Severity, _reason: &str, _command: &str| true;

        let err = tick_plan_runner(
            &mut store,
            &config,
            &root,
            &mut make_provider,
            &mut token_sink,
            &mut approval_handler,
        )
        .await
        .expect_err("malformed definition_json should fail the tick");
        assert!(err.to_string().contains("definition_json"));

        let stored = store.get_plan_run("plan-bad").unwrap().unwrap();
        assert_eq!(stored.status, "failed");
        assert_eq!(stored.claimed_at, None);
        assert!(
            stored
                .last_failure_json
                .as_deref()
                .unwrap()
                .contains("runner_internal_error")
        );
    }
}
