use autopoiesis::config::Config;
use autopoiesis::gate::{Guard, GuardContext, GuardEvent, ShellSafety, Verdict};
use autopoiesis::llm::ToolCall;
use std::path::PathBuf;

fn shipped_agents_toml_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("agents.toml")
}

fn make_tool_call(command: &str, id: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: "execute".to_string(),
        arguments: serde_json::json!({ "command": command }).to_string(),
    }
}

#[test]
fn committed_agents_toml_matches_tightened_shell_defaults() {
    let path = shipped_agents_toml_path();
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));

    for removed in [
        "git *", "cat *", "grep *", "head *", "tail *", "sed *", "awk *", "find *", "wc *", "env",
        "echo *",
    ] {
        assert!(
            !contents.contains(removed),
            "unexpected shipped shell allowlist entry: {removed}"
        );
    }

    for kept in ["cargo *", "ls *", "pwd", "which *", "date", "uname *"] {
        assert!(
            contents.contains(kept),
            "missing shipped shell allowlist entry: {kept}"
        );
    }
}

#[test]
fn committed_agents_toml_flows_into_shell_safety_behavior() {
    let config = Config::load(shipped_agents_toml_path())
        .unwrap_or_else(|error| panic!("failed to load shipped agents.toml: {error}"));
    let gate = ShellSafety::with_policy(config.shell_policy);

    let allow_call = make_tool_call("ls /tmp", "call-1");
    let mut allow_event = GuardEvent::ToolCall(&allow_call);
    assert!(matches!(
        gate.check(&mut allow_event, &GuardContext::default()),
        Verdict::Allow
    ));

    let denied_call = make_tool_call("env", "call-2");
    let mut denied_event = GuardEvent::ToolCall(&denied_call);
    assert!(matches!(
        gate.check(&mut denied_event, &GuardContext::default()),
        Verdict::Approve { .. }
    ));
}
