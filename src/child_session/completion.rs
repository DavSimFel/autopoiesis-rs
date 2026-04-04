//! Child completion propagation helpers.

use anyhow::{Context, Result};

use crate::llm::{ChatRole, MessageContent};
use crate::principal::Principal;
use crate::session::Session;
use crate::store::Store;

pub fn enqueue_child_completion(
    store: &mut Store,
    child_session_id: &str,
    session: &Session,
    last_assistant_response: Option<&str>,
) -> Result<bool> {
    let Some(parent_session_id) = store.get_parent_session(child_session_id)? else {
        return Ok(false);
    };

    let completion = build_completion_message(child_session_id, session, last_assistant_response);
    store
        .enqueue_message(
            &parent_session_id,
            "user",
            &completion,
            &format!("agent-{child_session_id}"),
        )
        .context("failed to enqueue child completion message")?;

    Ok(true)
}

pub(crate) fn should_enqueue_child_completion(processed_any: bool) -> bool {
    processed_any
}

fn build_completion_message(
    child_session_id: &str,
    session: &Session,
    last_assistant_response: Option<&str>,
) -> String {
    let response = last_assistant_response
        .filter(|response| !response.trim().is_empty())
        .map(ToString::to_string)
        .or_else(|| latest_assistant_response(session))
        .unwrap_or_else(|| "No assistant response was produced.".to_string());

    format!("Child session {child_session_id} completed.\n\n{response}")
}

pub(crate) fn latest_assistant_response(session: &Session) -> Option<String> {
    for message in session.history().iter().rev() {
        if message.role != ChatRole::Assistant || message.principal != Principal::Agent {
            continue;
        }

        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                MessageContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        return if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
    }

    None
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use std::fs;

    use crate::llm::{ChatMessage, ChatRole, MessageContent};
    use crate::principal::Principal;
    use crate::session::Session;

    use super::latest_assistant_response;

    #[test]
    fn latest_assistant_response_treats_empty_latest_text_as_absent() {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_completion_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();

        let mut session = Session::new(&root).unwrap();
        let mut non_empty =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
        non_empty.content.push(MessageContent::text("older answer"));
        session.append(non_empty, None).unwrap();

        let empty =
            ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
        // Whitespace-only text should not be treated as a real completion payload.
        let mut empty = empty;
        empty.content.push(MessageContent::text("   "));
        session.append(empty, None).unwrap();

        assert_eq!(latest_assistant_response(&session), None);

        let _ = fs::remove_dir_all(&root);
    }
}
