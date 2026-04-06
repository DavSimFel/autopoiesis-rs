#![cfg(not(clippy))]

use super::*;
use crate::Principal;
use crate::config::{
    AgentDefinition, AgentTierConfig, BudgetConfig, Config, DomainsConfig, ModelsConfig,
    QueueConfig, ReadToolConfig, ShellPolicy, SubscriptionsConfig,
};
use crate::identity;
use crate::llm::history_groups::{estimate_message_tokens, estimate_messages_tokens};
use crate::llm::{ChatRole, MessageContent};
use crate::session_registry::SessionRegistry;
use crate::skills::SkillDefinition;
use crate::skills::SkillSummary;
use crate::subscription::{SubscriptionFilter, SubscriptionRecord};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl TempDirGuard {
    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

fn temp_dir(prefix: &str) -> TempDirGuard {
    let path = env::temp_dir().join(format!(
        "autopoiesis_context_test_{prefix}_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("failed to create temp directory");
    TempDirGuard { path }
}

fn make_identity_files(dir: &TempDirGuard, files: &[(&str, &str)]) {
    for (name, contents) in files {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create identity file parent");
        }
        fs::write(path, contents).expect("failed to write identity file");
    }
}

fn message_text(message: &ChatMessage) -> &str {
    match message.content.first() {
        Some(MessageContent::Text { text }) => text,
        _ => panic!("expected text message content"),
    }
}

fn write_temp_file(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create temp file parent");
    }
    fs::write(&path, contents).expect("failed to write temp file");
    path
}

fn subscription_record(
    id: i64,
    session_id: Option<&str>,
    path: std::path::PathBuf,
    filter: SubscriptionFilter,
    activated_at: &str,
    updated_at: &str,
) -> SubscriptionRecord {
    SubscriptionRecord {
        id,
        session_id: session_id.map(ToString::to_string),
        topic: "_default".to_string(),
        path,
        filter,
        activated_at: activated_at.to_string(),
        updated_at: updated_at.to_string(),
    }
}

fn manifest_config() -> Config {
    let mut agents = crate::config::AgentsConfig::default();
    agents.entries.insert(
        "silas".to_string(),
        AgentDefinition {
            identity: Some("silas".to_string()),
            tier: None,
            model: Some("gpt-5.4-mini".to_string()),
            base_url: Some("https://example.test/api".to_string()),
            system_prompt: Some("legacy defaults".to_string()),
            session_name: Some("legacy-session".to_string()),
            reasoning_effort: Some("medium".to_string()),
            t1: AgentTierConfig {
                model: Some("gpt-5.4-mini".to_string()),
                base_url: None,
                system_prompt: Some("t1 prompt".to_string()),
                session_name: Some("silas-t1".to_string()),
                reasoning: None,
                reasoning_effort: None,
                delegation_token_threshold: None,
                delegation_tool_depth: None,
            },
            t2: AgentTierConfig {
                model: Some("o3".to_string()),
                base_url: None,
                system_prompt: Some("t2 prompt".to_string()),
                session_name: Some("silas-t2".to_string()),
                reasoning: Some("high".to_string()),
                reasoning_effort: None,
                delegation_token_threshold: None,
                delegation_tool_depth: None,
            },
        },
    );

    Config {
        model: "gpt-5.4-mini".to_string(),
        system_prompt: "system".to_string(),
        base_url: "https://example.test/api".to_string(),
        reasoning_effort: Some("medium".to_string()),
        session_name: Some("legacy-session".to_string()),
        operator_key: None,
        shell_policy: ShellPolicy::default(),
        budget: Some(BudgetConfig {
            max_tokens_per_turn: None,
            max_tokens_per_session: None,
            max_tokens_per_day: None,
        }),
        read: ReadToolConfig::default(),
        subscriptions: SubscriptionsConfig::default(),
        queue: QueueConfig::default(),
        identity_files: identity::t1_identity_files("src/shipped/identity-templates", "silas"),
        agents,
        models: ModelsConfig::default(),
        domains: DomainsConfig::default(),
        skills_dir: std::path::PathBuf::from("skills"),
        skills_dir_resolved: std::path::PathBuf::from("skills"),
        skills: crate::skills::SkillCatalog::default(),
        active_agent: Some("silas".to_string()),
    }
}

fn capture_warnings<F>(f: F) -> String
where
    F: FnOnce(),
{
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("warning buffer mutex poisoned")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let output = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let output = output.clone();
            move || SharedWriter(output.clone())
        })
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    f();

    String::from_utf8(
        output
            .lock()
            .expect("warning buffer mutex poisoned")
            .clone(),
    )
    .expect("warning output should be valid utf-8")
}

#[test]
fn identity_prompt_rewrites_existing_agent_system_message() {
    let dir = temp_dir("replaces");
    make_identity_files(
        &dir,
        &[
            ("constitution.md", "constitution"),
            ("agents/silas/agent.md", "identity"),
            ("context.md", "context"),
        ],
    );

    let mut vars = HashMap::new();
    vars.insert("model".to_string(), "gpt-5.4".to_string());
    let source = Identity::new(
        identity::t1_identity_files(dir.path(), "silas"),
        vars,
        "fallback",
    );
    let mut messages = vec![
        ChatMessage::system_with_principal("old", Some(crate::principal::Principal::Agent)),
        ChatMessage::user("ask"),
    ];
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].principal, crate::principal::Principal::Agent);
    assert_eq!(
        message_text(&messages[0]),
        "constitution\n\nidentity\n\ncontext"
    );
    assert_eq!(message_text(&messages[1]), "ask");
}

#[test]
fn session_manifest_renders_deterministically_into_system_context() {
    let registry = SessionRegistry::from_config(&manifest_config()).unwrap();
    let manifest = SessionManifest::from_registry(&registry);
    let mut messages = vec![ChatMessage::system_with_principal(
        "base prompt",
        Some(crate::principal::Principal::Agent),
    )];

    manifest.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, ChatRole::System);
    let rendered = messages[0]
        .content
        .iter()
        .filter_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("base prompt"));
    assert!(rendered.contains("## Available Sessions"));
    assert!(rendered.contains("silas-t1"));
    assert!(rendered.contains("silas-t2"));
    assert_eq!(rendered.matches("## Available Sessions").count(), 1);
    assert!(rendered.find("silas-t1").unwrap() < rendered.find("silas-t2").unwrap());
}

#[test]
fn identity_uses_fallback_on_missing_dir() {
    let mut vars = HashMap::new();
    vars.insert("model".to_string(), "gpt-5.4".to_string());
    let source = Identity::new(
        identity::t1_identity_files("/does/not/exist", "silas"),
        vars,
        "fallback prompt",
    );
    let mut messages = vec![ChatMessage::system("old prompt")];
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(message_text(&messages[0]), "fallback prompt");
    assert_eq!(message_text(&messages[1]), "old prompt");
}

#[test]
#[should_panic(expected = "failed to load identity prompt")]
fn identity_strict_mode_panics_on_missing_dir() {
    let source = Identity::new(
        identity::t1_identity_files("/does/not/exist", "silas"),
        HashMap::new(),
        "fallback prompt",
    )
    .strict();
    let mut messages = vec![ChatMessage::system("old prompt")];
    source.assemble(&mut messages);
}

#[test]
fn identity_applies_template_vars() {
    let dir = temp_dir("template_vars");
    make_identity_files(
        &dir,
        &[
            ("constitution.md", "model: {{model}}"),
            ("agents/silas/agent.md", "cwd: {{cwd}}"),
            ("context.md", "tool: {{tool}}"),
        ],
    );

    let mut vars = HashMap::new();
    vars.insert("model".to_string(), "gpt-4".to_string());
    vars.insert("cwd".to_string(), "/tmp/proj".to_string());
    vars.insert("tool".to_string(), "execute".to_string());
    let source = Identity::new(
        identity::t1_identity_files(dir.path(), "silas"),
        vars,
        "fallback",
    );
    let mut messages = vec![ChatMessage::system("old")];
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(
        message_text(&messages[0]),
        "model: gpt-4\n\ncwd: /tmp/proj\n\ntool: execute"
    );
    assert_eq!(message_text(&messages[1]), "old");
}

#[test]
fn skill_context_appends_summary_to_existing_system_message() {
    let source = SkillContext::new(vec![SkillSummary {
        name: "code-review".to_string(),
        description: "Reviews code changes".to_string(),
    }]);
    let mut messages = vec![ChatMessage::system("base prompt")];

    source.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, ChatRole::System);
    assert!(
        messages[0]
            .content
            .iter()
            .any(|block| matches!(block, MessageContent::Text { text } if text == "base prompt"))
    );
    assert!(messages[0].content.iter().any(|block| matches!(block, MessageContent::Text { text } if text == "Available skills: code-review (Reviews code changes)")));
}

#[test]
fn skill_loader_renders_single_skill_fragment() {
    let source = SkillLoader::new(vec![SkillDefinition {
        name: "code-review".to_string(),
        description: "Reviews code changes".to_string(),
        instructions: "Review carefully.".to_string(),
        required_caps: vec!["code".to_string()],
        token_estimate: 100,
    }]);

    assert_eq!(
        source.render_fragment(),
        "Skill: code-review\nReview carefully."
    );

    let mut messages = Vec::new();
    source.assemble(&mut messages);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, ChatRole::System);
    assert!(matches!(
        &messages[0].content[..],
        [MessageContent::Text { text }] if text == "Skill: code-review\nReview carefully."
    ));
}

#[test]
fn skill_loader_renders_multiple_skills_in_request_order() {
    let source = SkillLoader::new(vec![
        SkillDefinition {
            name: "planning".to_string(),
            description: "Plans work".to_string(),
            instructions: "Plan carefully.".to_string(),
            required_caps: vec!["reasoning".to_string()],
            token_estimate: 10,
        },
        SkillDefinition {
            name: "code-review".to_string(),
            description: "Reviews code changes".to_string(),
            instructions: "Review carefully.".to_string(),
            required_caps: vec!["code".to_string()],
            token_estimate: 20,
        },
    ]);

    assert_eq!(
        source.render_fragment(),
        "Skill: planning\nPlan carefully.\n\nSkill: code-review\nReview carefully."
    );
}

#[test]
fn skill_loader_merges_into_existing_system_message() {
    let source = SkillLoader::new(vec![SkillDefinition {
        name: "code-review".to_string(),
        description: "Reviews code changes".to_string(),
        instructions: "Review carefully.".to_string(),
        required_caps: vec!["code".to_string()],
        token_estimate: 100,
    }]);

    let mut messages = vec![ChatMessage::system("base".to_string())];
    source.assemble(&mut messages);
    assert_eq!(messages.len(), 1);
    let text = messages[0]
        .content
        .iter()
        .filter_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, "base\nSkill: code-review\nReview carefully.");
}

#[test]
fn history_adds_history_to_messages() {
    let mut source = History::new(1000);
    let history = vec![
        ChatMessage::user("first"),
        ChatMessage::user("middle"),
        ChatMessage::user("last"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 3);
    assert_eq!(message_text(&messages[0]), "first");
    assert_eq!(message_text(&messages[1]), "middle");
    assert_eq!(message_text(&messages[2]), "last");
}

#[test]
fn history_respects_token_budget() {
    let mut source = History::new(8);
    let history = vec![
        ChatMessage::user("alpha beta gamma delta epsilon"),
        ChatMessage::user("one two three four five six"),
        ChatMessage::user("the quick brown fox jumps"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(message_text(&messages[0]), "the quick brown fox jumps");
}

#[test]
fn history_keeps_assistant_tool_roundtrip_when_budget_allows_group() {
    let mut source = History::new(64);
    let mut assistant =
        ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
    assistant.content.push(MessageContent::Text {
        text: "alpha beta".to_string(),
    });
    assistant.content.push(MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: "{\"command\":\"echo hi\"}".to_string(),
        },
    });

    let history = vec![
        ChatMessage::user("very long old message that should be trimmed"),
        assistant,
        ChatMessage::tool_result_with_principal("call-1", "execute", "ok", Some(Principal::System)),
        ChatMessage::user("new"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    let assistant_index = messages
        .iter()
        .position(|message| matches!(message.role, ChatRole::Assistant))
        .expect("assistant round-trip should be retained");
    assert!(matches!(
        messages[assistant_index].role,
        ChatRole::Assistant
    ));
    assert!(matches!(
        &messages[assistant_index].content[..],
        [
            MessageContent::Text { text },
            MessageContent::ToolCall { call },
        ] if text == "alpha beta" && call.id == "call-1"
    ));
    assert!(matches!(
        messages
            .get(assistant_index + 1)
            .map(|message| &message.role),
        Some(ChatRole::Tool)
    ));
    assert!(matches!(
        &messages[assistant_index + 1].content[..],
        [MessageContent::ToolResult { result }] if result.tool_call_id == "call-1"
    ));
    assert!(matches!(
        messages.last().map(|message| &message.role),
        Some(ChatRole::User)
    ));
    assert!(matches!(
        &messages.last().expect("newest user message should remain").content[..],
        [MessageContent::Text { text }] if text == "new"
    ));
}

#[test]
fn history_skips_system_messages() {
    let mut source = History::new(1000);
    let history = vec![
        ChatMessage::system("system message should skip"),
        ChatMessage::user("first"),
        ChatMessage::system("another skip"),
        ChatMessage::user("last"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    for message in &messages {
        assert_ne!(message.role, ChatRole::System);
    }
    assert_eq!(message_text(&messages[0]), "first");
    assert_eq!(message_text(&messages[1]), "last");
}

#[test]
fn history_newest_first() {
    let mut source = History::new(6);
    let history = vec![
        ChatMessage::user("one two three"),
        ChatMessage::user("four five six"),
        ChatMessage::user("seven eight nine"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(message_text(&messages[0]), "four five six");
    assert_eq!(message_text(&messages[1]), "seven eight nine");
}

#[test]
fn history_keeps_assistant_tool_roundtrip_intact() {
    let mut source = History::new(2);
    let mut assistant =
        ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
    assistant.content.push(MessageContent::Text {
        text: "alpha beta".to_string(),
    });
    assistant.content.push(MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: "{\"command\":\"echo hi\"}".to_string(),
        },
    });
    let assistant_tool_call_count = assistant
        .content
        .iter()
        .filter(|block| matches!(**block, MessageContent::ToolCall { .. }))
        .count();

    let history = vec![
        ChatMessage::user("old"),
        assistant,
        ChatMessage::tool_result_with_principal("call-1", "execute", "ok", Some(Principal::System)),
        ChatMessage::user("new"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0].role, ChatRole::User));
    assert!(matches!(
        &messages[0].content[..],
        [MessageContent::Text { text }] if text == "new"
    ));
    assert!(
        messages
            .iter()
            .all(|message| !matches!(message.role, ChatRole::Assistant | ChatRole::Tool))
    );
    assert_eq!(assistant_tool_call_count, 1);
}

#[test]
fn history_keeps_multi_call_tool_roundtrip_intact() {
    let mut source = History::new(3);
    let mut assistant =
        ChatMessage::with_role_with_principal(ChatRole::Assistant, Some(Principal::Agent));
    assistant.content.push(MessageContent::Text {
        text: "alpha beta".to_string(),
    });
    assistant.content.push(MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "first".to_string(),
            arguments: "{}".to_string(),
        },
    });
    assistant.content.push(MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-2".to_string(),
            name: "second".to_string(),
            arguments: "{}".to_string(),
        },
    });
    let assistant_tool_call_count = assistant
        .content
        .iter()
        .filter(|block| matches!(**block, MessageContent::ToolCall { .. }))
        .count();

    let history = vec![
        ChatMessage::user("old"),
        assistant,
        ChatMessage::tool_result_with_principal("call-1", "first", "ok-1", Some(Principal::System)),
        ChatMessage::tool_result_with_principal(
            "call-2",
            "second",
            "ok-2",
            Some(Principal::System),
        ),
        ChatMessage::user("new"),
    ];
    source.set_history(&history);

    let mut messages = Vec::new();
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0].role, ChatRole::User));
    assert!(matches!(
        &messages[0].content[..],
        [MessageContent::Text { text }] if text == "new"
    ));
    assert_eq!(assistant_tool_call_count, 2);
}

#[test]
fn subscription_context_with_no_subscriptions_adds_no_messages() {
    let mut messages = vec![ChatMessage::system("base")];
    SubscriptionContext::new(Vec::new(), 100).assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(message_text(&messages[0]), "base");
}

#[test]
fn subscription_context_materializes_file_content() {
    let dir = temp_dir("materializes");
    let path = write_temp_file(dir.path(), "notes.txt", "line one\nline two");
    let record = subscription_record(
        1,
        Some("session-a"),
        path.clone(),
        SubscriptionFilter::Full,
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );
    let mut messages = vec![ChatMessage::system("identity")];
    SubscriptionContext::new(vec![record], 200).assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    let rendered = message_text(&messages[1]);
    assert!(rendered.contains("line one"));
    assert!(rendered.contains("line two"));
    assert!(rendered.contains(&path.display().to_string()));
    assert!(rendered.contains("filter=full"));
}

#[test]
fn subscription_context_applies_lines_regex_head_and_tail_filters() {
    let dir = temp_dir("filters");
    let path = write_temp_file(dir.path(), "data.txt", "one\ntwo\nthree\nfour\nfive");
    let base = vec![ChatMessage::system("identity")];

    let cases = vec![
        (
            SubscriptionFilter::Lines { start: 2, end: 4 },
            "two\nthree\nfour",
        ),
        (
            SubscriptionFilter::Regex {
                pattern: "^t".to_string(),
            },
            "two\nthree",
        ),
        (SubscriptionFilter::Head { count: 2 }, "one\ntwo"),
        (SubscriptionFilter::Tail { count: 2 }, "four\nfive"),
    ];

    for (index, (filter, expected)) in cases.into_iter().enumerate() {
        let record = subscription_record(
            index as i64 + 1,
            Some("session-a"),
            path.clone(),
            filter,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        );
        let mut messages = base.clone();
        SubscriptionContext::new(vec![record], 200).assemble(&mut messages);
        let rendered = message_text(&messages[1]);
        assert!(
            rendered.contains(expected),
            "case {index} rendered {rendered:?}"
        );
    }
}

#[test]
fn subscription_context_respects_token_budget_and_truncates_last_message() {
    let dir = temp_dir("budget");
    let path = write_temp_file(
        dir.path(),
        "big.txt",
        &"alpha beta gamma delta epsilon ".repeat(5000),
    );
    let record = subscription_record(
        1,
        Some("session-a"),
        path,
        SubscriptionFilter::Full,
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );
    let mut messages = vec![ChatMessage::system("identity")];
    SubscriptionContext::new(vec![record], 100).assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    let rendered = message_text(&messages[1]);
    assert!(rendered.contains("[truncated]"));
}

#[test]
fn subscription_context_truncates_multibyte_utf8_without_hanging() {
    let dir = temp_dir("utf8_budget");
    let path = write_temp_file(dir.path(), "utf8.txt", &"é".repeat(5000));
    let record = subscription_record(
        1,
        Some("session-a"),
        path,
        SubscriptionFilter::Full,
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );
    let mut messages = vec![ChatMessage::system("identity")];
    SubscriptionContext::new(vec![record], 1000).assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    let rendered = message_text(&messages[1]);
    assert!(rendered.contains("[truncated]"));
    assert!(rendered.contains("subscription path="));
}

#[test]
fn subscription_context_missing_file_warns_and_skips() {
    let dir = temp_dir("missing");
    let missing = dir.path().join("missing.txt");
    let record = subscription_record(
        1,
        Some("session-a"),
        missing.clone(),
        SubscriptionFilter::Full,
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );
    let warnings = capture_warnings(|| {
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(vec![record], 100).assemble(&mut messages);
        assert_eq!(messages.len(), 1);
    });

    assert!(warnings.contains("failed to read subscription file"));
    assert!(warnings.contains(&missing.display().to_string()));
}

#[test]
fn subscription_context_skips_bad_subscription_and_keeps_later_valid_ones() {
    let dir = temp_dir("mixed");
    let missing = dir.path().join("missing.txt");
    let valid = write_temp_file(dir.path(), "valid.txt", "kept");
    let records = vec![
        subscription_record(
            1,
            Some("session-a"),
            missing.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        ),
        subscription_record(
            2,
            Some("session-a"),
            valid.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:02Z",
            "2026-03-25T00:00:03Z",
        ),
    ];

    let warnings = capture_warnings(|| {
        let mut messages = vec![ChatMessage::system("identity")];
        SubscriptionContext::new(records, 100).assemble(&mut messages);
        assert_eq!(messages.len(), 2);
        assert!(message_text(&messages[1]).contains("kept"));
    });

    assert!(warnings.contains("failed to read subscription file"));
    assert!(warnings.contains(&missing.display().to_string()));
}

#[test]
fn subscription_context_includes_provenance_tags() {
    let dir = temp_dir("provenance");
    let path = write_temp_file(dir.path(), "prov.txt", "hello provenance");
    let record = subscription_record(
        1,
        Some("session-a"),
        path.clone(),
        SubscriptionFilter::Tail { count: 1 },
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );
    let mut messages = vec![ChatMessage::system("identity")];
    SubscriptionContext::new(vec![record], 1000).assemble(&mut messages);

    let rendered = message_text(&messages[1]);
    assert!(rendered.contains(&path.display().to_string()));
    assert!(rendered.contains("filter=tail"));
    assert!(rendered.contains("mtime="));
}

#[test]
fn subscription_context_orders_messages_by_effective_timestamp() {
    let dir = temp_dir("ordering");
    let first = write_temp_file(dir.path(), "first.txt", "first");
    let second = write_temp_file(dir.path(), "second.txt", "second");
    let records = vec![
        subscription_record(
            2,
            Some("session-a"),
            second.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:10Z",
        ),
        subscription_record(
            1,
            Some("session-a"),
            first.clone(),
            SubscriptionFilter::Full,
            "2026-03-25T00:00:00Z",
            "2026-03-25T00:00:01Z",
        ),
    ];
    let mut messages = vec![ChatMessage::system("identity")];
    SubscriptionContext::new(records, 200).assemble(&mut messages);

    assert!(message_text(&messages[1]).contains("first"));
    assert!(message_text(&messages[2]).contains("second"));
}

#[test]
fn history_handles_empty_messages() {
    let mut source = History::new(10);
    let mut messages = vec![ChatMessage::system("existing")];
    source.set_history(&[]);
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(message_text(&messages[0]), "existing");
}

#[test]
fn history_respects_budget_after_existing_context_is_counted() {
    let history = vec![ChatMessage::user("older"), ChatMessage::user("newest")];
    let existing_tokens = estimate_message_tokens(&ChatMessage::system("prompt"));
    let newest_tokens = estimate_message_tokens(&history[1]);
    let mut source = History::new(existing_tokens + newest_tokens);
    source.set_history(&history);

    let mut messages = vec![ChatMessage::system("prompt")];
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(message_text(&messages[0]), "prompt");
    assert_eq!(message_text(&messages[1]), "newest");
}
#[test]
fn history_respects_budget_with_existing_context_and_roundtrip_groups() {
    let existing = ChatMessage::system("prompt");
    let mut assistant = ChatMessage::with_role_with_principal(
        ChatRole::Assistant,
        Some(crate::principal::Principal::Agent),
    );
    assistant.content.push(MessageContent::Text {
        text: "alpha beta".to_string(),
    });
    assistant.content.push(MessageContent::ToolCall {
        call: crate::llm::ToolCall {
            id: "call-1".to_string(),
            name: "execute".to_string(),
            arguments: "{\"command\":\"echo hi\"}".to_string(),
        },
    });

    let selected_history = vec![
        assistant,
        ChatMessage::tool_result_with_principal(
            "call-1",
            "execute",
            "ok",
            Some(crate::principal::Principal::System),
        ),
        ChatMessage::user("newest"),
    ];
    let mut source = History::new(
        estimate_message_tokens(&existing) + estimate_messages_tokens(&selected_history),
    );
    source.set_history(&[
        ChatMessage::user("older"),
        selected_history[0].clone(),
        selected_history[1].clone(),
        selected_history[2].clone(),
    ]);

    let mut messages = vec![existing];
    source.assemble(&mut messages);

    assert_eq!(messages.len(), 4);
    assert_eq!(message_text(&messages[0]), "prompt");
    assert_eq!(messages[1].role, ChatRole::Assistant);
    assert_eq!(messages[2].role, ChatRole::Tool);
    assert_eq!(message_text(&messages[3]), "newest");
}
#[test]
fn subscription_context_truncates_multibyte_content_without_hanging() {
    let dir = temp_dir("multibyte_truncation");
    let path = write_temp_file(&dir.path().to_path_buf(), "notes.txt", &"é".repeat(4096));
    let record = subscription_record(
        1,
        Some("session-a"),
        path,
        SubscriptionFilter::Full,
        "2026-03-25T00:00:00Z",
        "2026-03-25T00:00:01Z",
    );

    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut messages = vec![ChatMessage::system("base")];
        SubscriptionContext::new(vec![record], 256).assemble(&mut messages);
        tx.send(messages).expect("should send rendered messages");
    });

    let messages = rx
        .recv_timeout(Duration::from_secs(1))
        .expect("subscription truncation should complete promptly");
    handle.join().expect("worker thread should finish cleanly");

    assert_eq!(messages.len(), 2);
    let rendered = message_text(&messages[1]);
    assert!(rendered.contains("[truncated]"));
}
