use anyhow::{Context, Result};

use crate::gate::{BudgetSnapshot, GuardContext, Verdict};
use crate::llm::{ChatMessage, TurnMeta};
use crate::session::Session;
use crate::turn::Turn;

/// Drain any buffered tokens through the sink after a budget-sensitive turn.
pub(crate) fn flush_buffered_tokens<TS: crate::agent::TokenSink + ?Sized>(
    token_sink: &mut TS,
    buffered_tokens: &mut Vec<String>,
) {
    for token in buffered_tokens.drain(..) {
        token_sink.on_token(token);
    }
}

pub(crate) fn token_total(meta: Option<&TurnMeta>, assistant_message: &ChatMessage) -> u64 {
    let estimated_tokens = Session::estimate_message_tokens(assistant_message);
    match meta {
        Some(meta) => match (meta.input_tokens, meta.output_tokens) {
            (Some(input), Some(output)) => input.saturating_add(output),
            (Some(input), None) => input.max(estimated_tokens),
            (None, Some(output)) => output.max(estimated_tokens),
            (None, None) => estimated_tokens,
        },
        None => estimated_tokens,
    }
}

pub(crate) fn charged_turn_meta(
    meta: Option<TurnMeta>,
    assistant_message: &ChatMessage,
) -> TurnMeta {
    let estimated_tokens = Session::estimate_message_tokens(assistant_message);
    match meta {
        Some(mut meta) => {
            match (meta.input_tokens, meta.output_tokens) {
                (Some(_), Some(_)) => {}
                (Some(input), None) => {
                    meta.output_tokens = Some(estimated_tokens.saturating_sub(input))
                }
                (None, Some(output)) => {
                    meta.input_tokens = Some(estimated_tokens.saturating_sub(output))
                }
                (None, None) => meta.output_tokens = Some(estimated_tokens),
            }
            meta
        }
        None => TurnMeta {
            model: None,
            input_tokens: None,
            output_tokens: Some(estimated_tokens),
            reasoning_tokens: None,
            reasoning_trace: None,
        },
    }
}

pub(crate) fn post_turn_budget_denial(
    turn: &Turn,
    session: &Session,
    assistant_message: &ChatMessage,
    turn_meta: Option<&TurnMeta>,
) -> Result<Option<(String, String)>> {
    if !turn.needs_budget_context() {
        return Ok(None);
    }

    let turn_tokens = token_total(turn_meta, assistant_message);
    let mut budget: BudgetSnapshot = session
        .budget_snapshot()
        .context("failed to read live budget snapshot")?;
    budget.turn_tokens += turn_tokens;
    budget.session_tokens += turn_tokens;
    budget.day_tokens += turn_tokens;
    let context = GuardContext {
        budget,
        ..Default::default()
    };

    if let Some(Verdict::Deny { reason, gate_id }) = turn.check_budget(context) {
        return Ok(Some((gate_id, reason)));
    }

    Ok(None)
}
