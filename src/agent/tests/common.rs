#![cfg(not(clippy))]

pub(crate) use crate::agent::*;
pub(crate) use rusqlite::Connection;
pub(crate) use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) use crate::context::History;
pub(crate) use crate::gate::{Guard, GuardEvent, SecretRedactor, ShellSafety, Verdict};
pub(crate) use crate::llm::{
    ChatMessage, FunctionTool, MessageContent, StopReason, StreamedTurn, ToolCall,
};
pub(crate) use crate::principal::Principal;
pub(crate) use crate::store::Store;
pub(crate) use crate::tool::{Shell, Tool, ToolFuture};
pub(crate) use crate::turn::Turn;
pub(crate) use serde_json::Value;

#[derive(Clone)]
pub(crate) struct InspectingProvider {
    pub(crate) observed_message_counts: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
}

impl InspectingProvider {
    pub(crate) fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<usize>>>) {
        let observed_message_counts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                observed_message_counts: observed_message_counts.clone(),
            },
            observed_message_counts,
        )
    }
}

impl crate::llm::LlmProvider for InspectingProvider {
    fn stream_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        _tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move {
            self.observed_message_counts
                .lock()
                .expect("observed message count mutex poisoned")
                .push(messages.len());

            Ok(StreamedTurn {
                assistant_message: ChatMessage::system("ok"),
                tool_calls: vec![],
                meta: None,
                stop_reason: StopReason::Stop,
            })
        })
    }
}

#[derive(Clone)]
pub(crate) struct SequenceProvider {
    turns: std::sync::Arc<std::sync::Mutex<Vec<StreamedTurn>>>,
}

impl SequenceProvider {
    pub(crate) fn new(turns: Vec<StreamedTurn>) -> Self {
        Self {
            turns: std::sync::Arc::new(std::sync::Mutex::new(turns.into_iter().rev().collect())),
        }
    }
}

impl crate::llm::LlmProvider for SequenceProvider {
    fn stream_completion<'a>(
        &'a self,
        _messages: &'a [ChatMessage],
        _tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move {
            self.turns
                .lock()
                .expect("sequence provider mutex poisoned")
                .pop()
                .ok_or_else(|| anyhow::anyhow!("no more turns"))
        })
    }
}

#[derive(Clone)]
pub(crate) struct StaticProvider {
    pub(crate) turn: StreamedTurn,
}

impl crate::llm::LlmProvider for StaticProvider {
    fn stream_completion<'a>(
        &'a self,
        _messages: &'a [ChatMessage],
        _tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move { Ok(self.turn.clone()) })
    }
}

#[derive(Clone)]
pub(crate) struct FailingProvider;

impl crate::llm::LlmProvider for FailingProvider {
    fn stream_completion<'a>(
        &'a self,
        _messages: &'a [ChatMessage],
        _tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move { Err(anyhow::anyhow!("provider failure")) })
    }
}

pub(crate) struct InboundDenyGuard;

impl Guard for InboundDenyGuard {
    fn name(&self) -> &str {
        "inbound-deny"
    }

    fn check(&self, event: &mut GuardEvent, _context: &crate::gate::GuardContext) -> Verdict {
        match event {
            GuardEvent::Inbound(_) => Verdict::Deny {
                reason: "blocked by test".to_string(),
                gate_id: "inbound-deny".to_string(),
            },
            _ => Verdict::Allow,
        }
    }
}

pub(crate) struct LeakyTool;

impl Tool for LeakyTool {
    fn name(&self) -> &str {
        "leak"
    }

    fn definition(&self) -> FunctionTool {
        FunctionTool {
            name: "leak".to_string(),
            description: "Return a secret".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            }),
        }
    }

    fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
        Box::pin(async { Ok("stdout:\nsk-proj-abcdefghijklmnopqrstuvwxyz012345".to_string()) })
    }
}

pub(crate) struct RecordingTool {
    pub(crate) executions: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) output: String,
}

impl RecordingTool {
    pub(crate) fn new(
        output: impl Into<String>,
    ) -> (Self, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let executions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        (
            Self {
                executions: executions.clone(),
                output: output.into(),
            },
            executions,
        )
    }
}

impl Tool for RecordingTool {
    fn name(&self) -> &str {
        "execute"
    }

    fn definition(&self) -> FunctionTool {
        FunctionTool {
            name: "execute".to_string(),
            description: "Record execution attempts".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                    },
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
        }
    }

    fn execute(&self, _arguments: &str) -> ToolFuture<'_> {
        self.executions
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let output = self.output.clone();
        Box::pin(async move { Ok(output) })
    }
}

pub(crate) fn temp_sessions_dir(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aprs_agent_test_{prefix}_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

pub(crate) fn temp_queue_root(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "aprs_agent_queue_test_{prefix}_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[derive(Clone)]
pub(crate) struct RecordingProvider {
    pub(crate) assistant_text: String,
    pub(crate) observed_tools: std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>,
}

impl crate::llm::LlmProvider for RecordingProvider {
    fn stream_completion<'a>(
        &'a self,
        _messages: &'a [ChatMessage],
        tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move {
            self.observed_tools
                .lock()
                .expect("tools mutex poisoned")
                .push(tools.iter().map(|tool| tool.name.clone()).collect());

            Ok(StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text(self.assistant_text.clone())],
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
            })
        })
    }
}

#[derive(Clone)]
pub(crate) struct MessageRecordingProvider {
    pub(crate) assistant_text: String,
    pub(crate) observed_system_texts: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl crate::llm::LlmProvider for MessageRecordingProvider {
    fn stream_completion<'a>(
        &'a self,
        messages: &'a [ChatMessage],
        tools: &'a [FunctionTool],
        _on_token: &'a mut (dyn FnMut(String) + Send),
    ) -> crate::llm::BoxFutureLlm<'a, Result<StreamedTurn>> {
        Box::pin(async move {
            self.observed_system_texts
                .lock()
                .expect("system text mutex poisoned")
                .push(
                    messages
                        .iter()
                        .find(|message| message.role == crate::llm::ChatRole::System)
                        .map(|message| {
                            message
                                .content
                                .iter()
                                .filter_map(|block| match block {
                                    MessageContent::Text { text } => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default(),
                );

            let _ = tools;
            Ok(StreamedTurn {
                assistant_message: ChatMessage {
                    role: crate::llm::ChatRole::Assistant,
                    principal: Principal::Agent,
                    content: vec![MessageContent::text(self.assistant_text.clone())],
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
            })
        })
    }
}

pub(crate) fn spawned_t3_test_config(
    skills_dir: std::path::PathBuf,
    skills: crate::skills::SkillCatalog,
) -> crate::config::Config {
    crate::config::Config {
        model: "gpt-test".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: None,
        operator_key: None,
        shell_policy: crate::config::ShellPolicy::default(),
        budget: None,
        read: crate::config::ReadToolConfig::default(),
        subscriptions: crate::config::SubscriptionsConfig::default(),
        queue: crate::config::QueueConfig::default(),
        identity_files: crate::identity::t1_identity_files(
            "src/shipped/identity-templates",
            "silas",
        ),
        skills_dir: skills_dir.clone(),
        skills_dir_resolved: skills_dir,
        skills,
        agents: {
            let mut agents = crate::config::AgentsConfig::default();
            agents.entries.insert(
                "silas".to_string(),
                crate::config::AgentDefinition {
                    identity: Some("silas".to_string()),
                    tier: None,
                    model: None,
                    base_url: None,
                    system_prompt: None,
                    session_name: None,
                    reasoning_effort: None,
                    t1: crate::config::AgentTierConfig::default(),
                    t2: crate::config::AgentTierConfig::default(),
                },
            );
            agents
        },
        models: {
            let mut models = crate::config::ModelsConfig::default();
            models.default = Some("gpt-child".to_string());
            models.catalog.insert(
                "gpt-child".to_string(),
                crate::config::ModelDefinition {
                    provider: "openai".to_string(),
                    model: "gpt-child".to_string(),
                    caps: vec!["code_review".to_string()],
                    context_window: Some(128_000),
                    cost_tier: Some("medium".to_string()),
                    cost_unit: Some(2),
                    enabled: Some(true),
                },
            );
            models.routes.insert(
                "code_review".to_string(),
                crate::config::ModelRoute {
                    requires: vec!["code_review".to_string()],
                    prefer: vec!["gpt-child".to_string()],
                },
            );
            models
        },
        domains: Default::default(),
        active_agent: Some("silas".to_string()),
    }
}

pub(crate) fn message_text(message: &ChatMessage) -> Option<&str> {
    message.content.iter().find_map(|block| match block {
        MessageContent::Text { text } => Some(text.as_str()),
        _ => None,
    })
}

pub(crate) fn shell_policy(
    default: &str,
    allow_patterns: &[&str],
    deny_patterns: &[&str],
    standing_approvals: &[&str],
    default_severity: &str,
) -> crate::config::ShellPolicy {
    crate::config::ShellPolicy {
        default: match default {
            "allow" => crate::config::ShellDefaultAction::Allow,
            "approve" => crate::config::ShellDefaultAction::Approve,
            other => panic!("unexpected shell default in test helper: {other}"),
        },
        allow_patterns: allow_patterns
            .iter()
            .map(|pattern| pattern.to_string())
            .collect(),
        deny_patterns: deny_patterns
            .iter()
            .map(|pattern| pattern.to_string())
            .collect(),
        standing_approvals: standing_approvals
            .iter()
            .map(|pattern| pattern.to_string())
            .collect(),
        default_severity: match default_severity {
            "low" => crate::config::ShellDefaultSeverity::Low,
            "medium" => crate::config::ShellDefaultSeverity::Medium,
            "high" => crate::config::ShellDefaultSeverity::High,
            other => panic!("unexpected shell severity in test helper: {other}"),
        },
        max_output_bytes: crate::config::DEFAULT_SHELL_MAX_OUTPUT_BYTES,
        max_timeout_ms: crate::config::DEFAULT_SHELL_MAX_TIMEOUT_MS,
    }
}

pub(crate) fn streamed_turn_with_tool_call(
    text: Option<&str>,
    command: &str,
    call_id: &str,
) -> StreamedTurn {
    let mut content = Vec::new();
    if let Some(text) = text {
        content.push(MessageContent::text(text));
    }

    let call = ToolCall {
        id: call_id.to_string(),
        name: "execute".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    };
    content.push(MessageContent::ToolCall { call: call.clone() });

    StreamedTurn {
        assistant_message: ChatMessage {
            role: crate::llm::ChatRole::Assistant,
            principal: Principal::Agent,
            content,
        },
        tool_calls: vec![call],
        meta: None,
        stop_reason: StopReason::ToolCalls,
    }
}
