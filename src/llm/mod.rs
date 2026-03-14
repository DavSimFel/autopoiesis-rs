use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod openai;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessageContent {
    Text {
        text: String,
    },
    ToolCall {
        #[serde(flatten)]
        call: ToolCall,
    },
    ToolResult {
        #[serde(flatten)]
        result: ToolResult,
    },
}

impl MessageContent {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text {
            text: value.into(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            result: ToolResult {
                tool_call_id: tool_call_id.into(),
                name: name.into(),
                content: content.into(),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: Vec<MessageContent>,
}

impl ChatMessage {
    pub fn with_role(role: ChatRole) -> Self {
        Self {
            role,
            content: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: vec![MessageContent::text(content)],
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: vec![MessageContent::text(content)],
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: vec![MessageContent::text(content)],
        }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        let mut message = Self::with_role(ChatRole::Assistant);
        message
            .content
            .extend(calls.into_iter().map(|call| MessageContent::ToolCall { call }));
        message
    }

    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: vec![MessageContent::tool_result(tool_call_id, name, content)],
        }
    }

}

#[derive(Debug, Clone)]
pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone)]
pub struct StreamedTurn {
    pub assistant_message: ChatMessage,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other(String),
}

#[async_trait]
pub trait LlmProvider {
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        on_token: &mut (dyn FnMut(String) + Send),
    ) -> Result<StreamedTurn>;
}
