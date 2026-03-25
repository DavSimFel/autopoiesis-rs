use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use autopoiesis::Principal;
use autopoiesis::agent::TurnVerdict;
use autopoiesis::config::Config;
use autopoiesis::gate::BudgetSnapshot;
use autopoiesis::llm::{
    ChatMessage, ChatRole, FunctionTool, LlmProvider, MessageContent, StopReason, StreamedTurn,
    ToolCall, TurnMeta,
};
use autopoiesis::spawn::{SpawnRequest, SpawnResult};
use autopoiesis::store::Store;
use rusqlite::Connection;
use serde::Deserialize;

struct TierTestFixtures {
    _tempdir_path: PathBuf,
    root: PathBuf,
    queue_db_path: PathBuf,
    workspace_dir: PathBuf,
    skills_dir: PathBuf,
    identity_dir: PathBuf,
}

impl TierTestFixtures {
    fn new() -> Result<Self> {
        let mut root = std::env::temp_dir();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("compute unique timestamp")?
            .as_nanos();
        root.push(format!("autopoiesis-tier-test-{unique}"));
        let queue_db_path = root.join("queue.sqlite");
        let workspace_dir = root.join("workspace");
        let skills_dir = root.join("skills");
        let identity_dir = root.join("identity-templates");

        fs::create_dir_all(&root).context("create temp root")?;
        fs::create_dir_all(&workspace_dir).context("create workspace dir")?;
        fs::create_dir_all(&skills_dir).context("create skills dir")?;
        fs::create_dir_all(&identity_dir).context("create identity dir")?;

        Ok(Self {
            _tempdir_path: root.clone(),
            root,
            queue_db_path,
            workspace_dir,
            skills_dir,
            identity_dir,
        })
    }

    fn write_workspace_file(&self, rel: &str, contents: &str) -> Result<PathBuf> {
        let path = self.workspace_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create workspace parent")?;
        }
        fs::write(&path, contents).context("write workspace file")?;
        Ok(path)
    }

    fn write_skill_file(&self, rel: &str, contents: &str) -> Result<PathBuf> {
        let path = self.skills_dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create skill parent")?;
        }
        fs::write(&path, contents).context("write skill file")?;
        Ok(path)
    }

    fn copy_identity_templates_from_repo(&self) -> Result<()> {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("identity-templates");
        copy_dir_recursive(&source, &self.identity_dir)
    }

    fn queue_db_path(&self) -> &PathBuf {
        &self.queue_db_path
    }

    fn build_config(&self) -> Result<Config> {
        let config_path = self.root.join("agents.toml");
        let config_contents = format!(
            "skills_dir='skills'\nread.allowed_paths=['{}']\nread.max_read_bytes=65536\n[agents.silas]\nidentity='silas'\n[agents.silas.t1]\nbase_url='https://example.test/api'\nsystem_prompt='system'\nsession_name='tier-test'\nmodel='t1-model'\n[agents.silas.t2]\nmodel='t2-model'\nreasoning='high'\n[models]\ndefault='t1-model'\n[models.catalog.t1-model]\nprovider='openai'\nmodel='t1-model'\ncaps=['code_review']\ncontext_window=32000\ncost_tier='low'\ncost_unit=1\nenabled=true\n[models.catalog.t2-model]\nprovider='openai'\nmodel='t2-model'\ncaps=['code_review']\ncontext_window=32000\ncost_tier='medium'\ncost_unit=1\nenabled=true\n[models.catalog.gpt-child]\nprovider='openai'\nmodel='gpt-child'\ncaps=['reasoning']\ncontext_window=32000\ncost_tier='high'\ncost_unit=2\nenabled=true\n",
            self.workspace_dir.to_string_lossy()
        );
        std::fs::write(&config_path, config_contents).context("write test agents.toml")?;
        Config::load(&config_path).context("load test config")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueueRow {
    id: i64,
    session_id: String,
    source: String,
    status: String,
    content: String,
}

fn read_queue_rows(db_path: &PathBuf, session_id: &str) -> Result<Vec<QueueRow>> {
    let conn = Connection::open(db_path).context("open queue database")?;
    let mut statement = conn
        .prepare(
            "SELECT id, session_id, source, status, content
             FROM messages
             WHERE session_id = ?1
             ORDER BY created_at ASC, id ASC",
        )
        .context("prepare queue inspection query")?;

    let rows = statement
        .query_map([session_id], |row| {
            Ok(QueueRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                source: row.get(2)?,
                status: row.get(3)?,
                content: row.get(4)?,
            })
        })
        .context("execute queue inspection query")?;

    let mut collected = Vec::new();
    for row in rows {
        collected.push(row.context("decode queue row")?);
    }
    Ok(collected)
}

fn spawn_t2_child(
    store: &mut Store,
    config: &Config,
    parent_session_id: &str,
    task: impl Into<String>,
) -> Result<SpawnResult> {
    autopoiesis::spawn::spawn_child(
        store,
        config,
        BudgetSnapshot::default(),
        SpawnRequest {
            parent_session_id: parent_session_id.to_string(),
            task: task.into(),
            task_kind: Some("analysis".to_string()),
            tier: Some("t2".to_string()),
            model_override: Some("t2-model".to_string()),
            reasoning_override: Some("high".to_string()),
            skills: vec![],
            skill_token_budget: None,
        },
    )
    .context("spawn t2 child")
}

fn spawn_t3_child(
    store: &mut Store,
    config: &Config,
    parent_session_id: &str,
    task: impl Into<String>,
    skill_name: impl Into<String>,
) -> Result<SpawnResult> {
    autopoiesis::spawn::spawn_child(
        store,
        config,
        BudgetSnapshot::default(),
        SpawnRequest {
            parent_session_id: parent_session_id.to_string(),
            task: task.into(),
            task_kind: Some("analysis".to_string()),
            tier: Some("t3".to_string()),
            model_override: Some("gpt-child".to_string()),
            reasoning_override: None,
            skills: vec![skill_name.into()],
            skill_token_budget: None,
        },
    )
    .context("spawn t3 child")
}

#[derive(Clone)]
struct ScriptedProvider {
    observed_messages: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
    observed_tools: Arc<Mutex<Vec<Vec<String>>>>,
    responses: Arc<Mutex<VecDeque<StreamedTurn>>>,
}

impl ScriptedProvider {
    fn new(responses: impl Into<Vec<StreamedTurn>>) -> Self {
        Self {
            observed_messages: Arc::new(Mutex::new(Vec::new())),
            observed_tools: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(VecDeque::from(responses.into()))),
        }
    }

    fn observed_messages(&self) -> Arc<Mutex<Vec<Vec<ChatMessage>>>> {
        self.observed_messages.clone()
    }

    fn observed_tools(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        self.observed_tools.clone()
    }
}

impl LlmProvider for ScriptedProvider {
    async fn stream_completion(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        _on_token: &mut (dyn FnMut(String) + Send),
    ) -> Result<StreamedTurn> {
        self.observed_messages
            .lock()
            .expect("messages mutex poisoned")
            .push(messages.to_vec());
        self.observed_tools
            .lock()
            .expect("tools mutex poisoned")
            .push(tools.iter().map(|tool| tool.name.clone()).collect());
        let response = self
            .responses
            .lock()
            .expect("responses mutex poisoned")
            .pop_front()
            .expect("scripted provider ran out of responses");
        Ok(response)
    }
}

fn scripted_stop_turn(text: impl Into<String>, model: impl Into<String>) -> StreamedTurn {
    StreamedTurn {
        assistant_message: ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::text(text)],
        },
        tool_calls: Vec::new(),
        meta: Some(TurnMeta {
            model: Some(model.into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            reasoning_tokens: None,
            reasoning_trace: None,
        }),
        stop_reason: StopReason::Stop,
    }
}

fn scripted_tool_call_turn(
    text: impl Into<String>,
    model: impl Into<String>,
    tool_name: impl Into<String>,
    tool_arguments: impl Into<String>,
) -> StreamedTurn {
    let tool_name = tool_name.into();
    StreamedTurn {
        assistant_message: ChatMessage {
            role: ChatRole::Assistant,
            principal: Principal::Agent,
            content: vec![MessageContent::text(text)],
        },
        tool_calls: vec![ToolCall {
            id: format!("call-{tool_name}"),
            name: tool_name.clone(),
            arguments: tool_arguments.into(),
        }],
        meta: Some(TurnMeta {
            model: Some(model.into()),
            input_tokens: Some(1),
            output_tokens: Some(1),
            reasoning_tokens: None,
            reasoning_trace: None,
        }),
        stop_reason: StopReason::ToolCalls,
    }
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).context("create destination directory")?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("read source directory {}", source.display()))?
    {
        let entry = entry.context("read source directory entry")?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        }
    }
    Ok(())
}

async fn process_one_queued_message<F, Fut, P, TS, AH>(
    store: &mut autopoiesis::store::Store,
    child_session_id: &str,
    session: &mut autopoiesis::session::Session,
    turn: &autopoiesis::turn::Turn,
    make_provider: &mut F,
    token_sink: &mut TS,
    approval_handler: &mut AH,
) -> Result<Option<autopoiesis::agent::QueueOutcome>>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<P>>,
    P: autopoiesis::llm::LlmProvider,
    TS: autopoiesis::agent::TokenSink + Send,
    AH: autopoiesis::agent::ApprovalHandler,
{
    let Some(queued_message) = store.dequeue_next_message(child_session_id)? else {
        return Ok(None);
    };

    let message_id = queued_message.id;
    let verdict = autopoiesis::agent::process_message(
        &queued_message,
        session,
        turn,
        make_provider,
        token_sink,
        approval_handler,
    )
    .await;

    match verdict {
        Ok(verdict) => {
            store.mark_processed(message_id)?;
            Ok(Some(verdict))
        }
        Err(err) => {
            store.mark_failed(message_id)?;
            Err(err)
        }
    }
}

fn message_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            MessageContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Deserialize)]
struct ChildMetadataView {
    parent_session_id: String,
    tier: String,
    resolved_model: String,
    resolved_provider_model: String,
    #[serde(default)]
    skills: Vec<autopoiesis::skills::SkillDefinition>,
}

#[tokio::test]
async fn tier_integration_end_to_end_mvp() -> Result<()> {
    let fixtures = TierTestFixtures::new()?;
    fixtures.copy_identity_templates_from_repo()?;
    fixtures.write_workspace_file("notes/workspace.txt", "workspace sentinel alpha")?;
    fixtures.write_skill_file(
        "planning.toml",
        r#"
[skill]
name = "planning"
description = "Plans the final execution step"
instructions = """
Use the workspace evidence and shell result to produce a concise conclusion.
"""
required_caps = ["reasoning"]
token_estimate = 250
"#,
    )?;

    let config = fixtures.build_config()?;
    let mut store = Store::new(fixtures.queue_db_path())?;
    let mut root_session = autopoiesis::session::Session::new(fixtures.root.join("sessions/t1"))?;
    let mut t2_session = autopoiesis::session::Session::new(fixtures.root.join("sessions/t2"))?;
    let mut t3_session = autopoiesis::session::Session::new(fixtures.root.join("sessions/t3"))?;
    store.create_session("t1-root", None)?;

    let t1_turn = autopoiesis::turn::build_turn_for_config(&config);
    assert_eq!(
        t1_turn
            .tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>(),
        vec!["execute".to_string()]
    );

    let t1_provider = ScriptedProvider::new(vec![
        scripted_stop_turn("T1 loaded config and delegated", "t1-model"),
        scripted_stop_turn("T1 received the final T2 conclusion", "t1-model"),
    ]);
    let t1_provider_messages = t1_provider.observed_messages();
    let t1_provider_tools = t1_provider.observed_tools();
    let mut t1_make_provider = {
        let provider = t1_provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };
    let mut token_sink = |_token: String| {};
    let mut approval_handler =
        |_severity: &autopoiesis::gate::Severity, _reason: &str, _command: &str| true;

    autopoiesis::agent::run_agent_loop(
        &mut t1_make_provider,
        &mut root_session,
        "delegate the work to T2".to_string(),
        Principal::Operator,
        &t1_turn,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?;

    let t1_tools = t1_provider_tools
        .lock()
        .expect("t1 tools mutex poisoned")
        .clone();
    assert_eq!(t1_tools, vec![vec!["execute".to_string()]]);
    let t1_messages = t1_provider_messages
        .lock()
        .expect("t1 messages mutex poisoned")
        .clone();
    assert!(
        t1_messages
            .iter()
            .flatten()
            .any(|message| { message_text(message).contains("delegate the work to T2") })
    );

    let spawn_t2 = spawn_t2_child(
        &mut store,
        &config,
        "t1-root",
        "Inspect the workspace, then decide whether execution is needed.",
    )?;
    let t2_child_metadata: ChildMetadataView = serde_json::from_str(
        &store
            .get_session_metadata(&spawn_t2.child_session_id)?
            .context("missing T2 child metadata")?,
    )
    .context("decode T2 child metadata")?;
    assert_eq!(t2_child_metadata.parent_session_id, "t1-root");
    assert_eq!(t2_child_metadata.tier, "t2");
    assert_eq!(t2_child_metadata.resolved_model, "t2-model");
    assert_eq!(t2_child_metadata.resolved_provider_model, "t2-model");

    let t2_config = config
        .with_spawned_child_runtime("t2", "t2-model", Some("high"))
        .context("build T2 runtime config")?;
    let t2_turn = autopoiesis::turn::build_turn_for_config(&t2_config);
    assert_eq!(
        t2_turn
            .tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>(),
        vec!["read_file".to_string()]
    );
    let mut t2_context = Vec::new();
    t2_turn.assemble_context(&mut t2_context);
    let t2_system_text = t2_context
        .iter()
        .find(|message| message.role == ChatRole::System)
        .map(message_text)
        .unwrap_or_default();
    assert!(t2_system_text.contains("Available skills:"));

    let t2_provider = ScriptedProvider::new(vec![
        scripted_tool_call_turn(
            "T2 is reading the workspace file",
            "t2-model",
            "read_file",
            format!(
                r#"{{"path":"{}","offset":1,"limit":256}}"#,
                fixtures.workspace_dir.join("notes/workspace.txt").display()
            ),
        ),
        scripted_stop_turn(
            "T2 read workspace sentinel alpha and will ask for execution",
            "t2-model",
        ),
        scripted_stop_turn(
            "T2 conclusion: workspace sentinel alpha + T3-SENTINEL",
            "t2-model",
        ),
    ]);
    let t2_provider_messages = t2_provider.observed_messages();
    let t2_provider_tools = t2_provider.observed_tools();
    let mut t2_make_provider = {
        let provider = t2_provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };

    let t2_first_verdict = process_one_queued_message(
        &mut store,
        &spawn_t2.child_session_id,
        &mut t2_session,
        &t2_turn,
        &mut t2_make_provider,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?
    .context("expected first T2 turn to execute")?;
    assert!(matches!(
        t2_first_verdict,
        autopoiesis::agent::QueueOutcome::Agent(TurnVerdict::Executed(_))
    ));

    let t2_rows_after_first = read_queue_rows(&fixtures.queue_db_path, &spawn_t2.child_session_id)?;
    assert_eq!(t2_rows_after_first.len(), 1);
    assert_eq!(t2_rows_after_first[0].status, "processed");

    let t2_transcript = t2_provider_messages
        .lock()
        .expect("t2 messages mutex poisoned")
        .clone();
    assert!(t2_transcript.iter().flatten().any(|message| {
        message.role == ChatRole::Tool
            && message.content.iter().any(|block| {
                matches!(
                    block,
                    MessageContent::ToolResult { result }
                        if result.name == "read_file"
                            && result.content.contains("workspace sentinel alpha")
                            && result.content.contains("read_file")
                )
            })
    }));

    let planning_skill = config
        .skills
        .resolve_requested_skills(&["planning".to_string()])
        .context("resolve planning skill")?;
    assert_eq!(planning_skill.len(), 1);

    let spawn_t3 = spawn_t3_child(
        &mut store,
        &config,
        &spawn_t2.child_session_id,
        "Run the shell verification step.",
        "planning",
    )?;
    let t3_child_metadata: ChildMetadataView = serde_json::from_str(
        &store
            .get_session_metadata(&spawn_t3.child_session_id)?
            .context("missing T3 child metadata")?,
    )
    .context("decode T3 child metadata")?;
    assert_eq!(
        t3_child_metadata.parent_session_id,
        spawn_t2.child_session_id
    );
    assert_eq!(t3_child_metadata.tier, "t3");
    assert_eq!(t3_child_metadata.resolved_model, "gpt-child");
    assert_eq!(t3_child_metadata.resolved_provider_model, "gpt-child");
    assert_eq!(t3_child_metadata.skills.len(), 1);
    assert_eq!(t3_child_metadata.skills[0].name, "planning");

    let t3_config = config
        .with_spawned_child_runtime("t3", "gpt-child", None)
        .context("build T3 runtime config")?;
    let t3_turn = autopoiesis::turn::build_spawned_t3_turn(&t3_config, planning_skill.clone());
    assert_eq!(
        t3_turn
            .tool_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>(),
        vec!["execute".to_string()]
    );
    let mut t3_context = Vec::new();
    t3_turn.assemble_context(&mut t3_context);
    let t3_system_text = t3_context
        .iter()
        .find(|message| message.role == ChatRole::System)
        .map(message_text)
        .unwrap_or_default();
    assert!(t3_system_text.contains("Skill: planning"));
    assert!(!t3_system_text.contains("Available skills:"));

    let t3_provider = ScriptedProvider::new(vec![
        scripted_tool_call_turn(
            "T3 will execute the shell step",
            "gpt-child",
            "execute",
            r#"{"command":"printf 'T3-SENTINEL'","timeout_ms":1000}"#,
        ),
        scripted_stop_turn("T3-SENTINEL", "gpt-child"),
    ]);
    let t3_provider_messages = t3_provider.observed_messages();
    let t3_provider_tools = t3_provider.observed_tools();
    let mut t3_make_provider = {
        let provider = t3_provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };

    process_one_queued_message(
        &mut store,
        &spawn_t3.child_session_id,
        &mut t3_session,
        &t3_turn,
        &mut t3_make_provider,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?;
    assert!(autopoiesis::spawn::enqueue_child_completion(
        &mut store,
        &spawn_t3.child_session_id,
        &t3_session
    )?);

    let t3_tools = t3_provider_tools
        .lock()
        .expect("t3 tools mutex poisoned")
        .clone();
    assert_eq!(
        t3_tools,
        vec![vec!["execute".to_string()], vec!["execute".to_string()]]
    );
    let t3_messages = t3_provider_messages
        .lock()
        .expect("t3 messages mutex poisoned")
        .clone();
    assert!(
        t3_messages
            .iter()
            .flatten()
            .any(|message| { message_text(message).contains("Run the shell verification step.") })
    );
    let t3_history = t3_session.history().to_vec();
    assert!(t3_history.iter().any(|message| {
        let text = message_text(message);
        text.contains("T3-SENTINEL")
    }));

    let t2_rows_after_t3 = read_queue_rows(&fixtures.queue_db_path, &spawn_t2.child_session_id)?;
    assert!(
        t2_rows_after_t3
            .iter()
            .any(|row| row.source == format!("agent-{}", spawn_t3.child_session_id))
    );
    assert!(
        t2_rows_after_t3
            .iter()
            .any(|row| row.content.contains("T3-SENTINEL"))
    );

    process_one_queued_message(
        &mut store,
        &spawn_t2.child_session_id,
        &mut t2_session,
        &t2_turn,
        &mut t2_make_provider,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?;
    assert!(autopoiesis::spawn::enqueue_child_completion(
        &mut store,
        &spawn_t2.child_session_id,
        &t2_session
    )?);

    let t2_tools = t2_provider_tools
        .lock()
        .expect("t2 tools mutex poisoned")
        .clone();
    assert_eq!(
        t2_tools,
        vec![
            vec!["read_file".to_string()],
            vec!["read_file".to_string()],
            vec!["read_file".to_string()]
        ]
    );
    let t2_messages = t2_provider_messages
        .lock()
        .expect("t2 messages mutex poisoned")
        .clone();
    assert!(
        t2_messages
            .iter()
            .flatten()
            .any(|message| { message_text(message).contains("Inspect the workspace") })
    );
    let t2_history = t2_session.history().to_vec();
    assert!(t2_history.iter().any(|message| {
        let text = message_text(message);
        text.contains("workspace sentinel alpha") && text.contains("T3-SENTINEL")
    }));

    let t1_rows_after_t2 = read_queue_rows(&fixtures.queue_db_path, "t1-root")?;
    assert!(t1_rows_after_t2.iter().any(|row| {
        row.content
            .contains("T2 conclusion: workspace sentinel alpha + T3-SENTINEL")
    }));

    let t1_turn_after_completion = autopoiesis::turn::build_turn_for_config(&config);
    let mut t1_finalize_make_provider = {
        let provider = t1_provider.clone();
        move || {
            let provider = provider.clone();
            async move { Ok::<_, anyhow::Error>(provider) }
        }
    };

    process_one_queued_message(
        &mut store,
        "t1-root",
        &mut root_session,
        &t1_turn_after_completion,
        &mut t1_finalize_make_provider,
        &mut token_sink,
        &mut approval_handler,
    )
    .await?;

    let t1_finalize_messages = t1_provider_messages
        .lock()
        .expect("t1 messages mutex poisoned")
        .clone();
    assert!(t1_finalize_messages.iter().flatten().any(|message| {
        let text = message_text(message);
        text.contains("Child session")
            && text.contains("T2 conclusion: workspace sentinel alpha + T3-SENTINEL")
    }));
    let t1_finalize_tools = t1_provider_tools
        .lock()
        .expect("t1 tools mutex poisoned")
        .clone();
    assert_eq!(
        t1_finalize_tools,
        vec![vec!["execute".to_string()], vec!["execute".to_string()]]
    );

    Ok(())
}
