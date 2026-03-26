use anyhow::Result;

use crate::agent::TurnVerdict;
use crate::llm::{ChatMessage, MessageContent, TurnMeta};
use crate::principal::Principal;
use crate::session::Session;
use crate::turn::Turn;

const MAX_DENIALS_PER_TURN: usize = 2;

fn append_audit_note(session: &mut Session, note: String) -> Result<()> {
    let mut message = ChatMessage::with_role_with_principal(
        crate::llm::ChatRole::Assistant,
        Some(Principal::System),
    );
    message.content.push(MessageContent::text(note));
    session.append(message, None)
}

fn append_approval_denied(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(
        session,
        format!("Tool execution rejected after approval by {gate_id}"),
    )
}

fn append_inbound_approval_denied(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(
        session,
        format!("Message rejected after approval by {gate_id}"),
    )
}

fn append_hard_deny(session: &mut Session, by: &str) -> Result<()> {
    append_audit_note(session, format!("Tool execution hard-denied by {by}"))
}

fn append_inbound_deny(session: &mut Session, gate_id: &str) -> Result<()> {
    append_audit_note(session, format!("Message hard-denied by {gate_id}"))
}

pub(crate) fn persist_denied_assistant_text(
    session: &mut Session,
    turn: &Turn,
    mut assistant_message: ChatMessage,
    meta: Option<TurnMeta>,
) -> Result<()> {
    crate::gate::guard_message_output(turn, &mut assistant_message);
    assistant_message
        .content
        .retain(|block| matches!(block, MessageContent::Text { .. }));

    if assistant_message.content.is_empty() {
        assistant_message.content.push(MessageContent::Text {
            text: String::new(),
        });
    }

    session.append(assistant_message, meta)
}

/// Build a denial verdict, stopping after too many denied actions in one turn.
pub(crate) fn make_denial_verdict(
    denial_count: &mut usize,
    gate_id: String,
    reason: String,
) -> TurnVerdict {
    *denial_count += 1;
    if *denial_count >= MAX_DENIALS_PER_TURN {
        TurnVerdict::Denied {
            reason: format!(
                "stopped after {} denied actions this turn; last denial by {gate_id}: {reason}",
                *denial_count
            ),
            gate_id,
        }
    } else {
        TurnVerdict::Denied { reason, gate_id }
    }
}

pub(crate) fn append_denial_note_for_inbound(session: &mut Session, gate_id: &str) -> Result<()> {
    append_inbound_deny(session, gate_id)
}

pub(crate) fn append_denial_note_for_inbound_approval(
    session: &mut Session,
    gate_id: &str,
) -> Result<()> {
    append_inbound_approval_denied(session, gate_id)
}

pub(crate) fn append_denial_note_for_tool_approval(
    session: &mut Session,
    gate_id: &str,
) -> Result<()> {
    append_approval_denied(session, gate_id)
}

pub(crate) fn append_denial_note_for_tool_deny(session: &mut Session, gate_id: &str) -> Result<()> {
    append_hard_deny(session, gate_id)
}

/// Format a denial message shared by CLI and server output paths.
pub fn format_denial_message(reason: &str, gate_id: &str) -> String {
    format!("Command hard-denied by {gate_id}: {reason}")
}
