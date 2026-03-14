use serde_json::Value;
use serde_json::from_str;

use crate::llm::{ChatMessage, MessageContent, ToolCall};

/// Execution bands for the gate pipeline.
#[derive(Clone, Copy)]
pub enum Band {
    /// Build prompt content (identity, memory, context).
    Assemble,
    /// Add dynamic context.
    Enrich,
    /// Scrub secrets and redact sensitive patterns.
    Sanitize,
    /// Safety checks — currently run sequentially.
    Validate,
}

/// Directional gate application.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Both,
}

/// Structured event passed to each gate implementation.
pub enum GateEvent<'a> {
    /// Inbound: entire message list before sending to the LLM.
    Messages(&'a mut Vec<ChatMessage>),
    /// Outbound: one tool call as it completes.
    ToolCallComplete(&'a ToolCall),
    /// Outbound: full batch of tool calls for the turn.
    ToolCallBatch(&'a [ToolCall]),
    /// Outbound: text delta chunk from streaming.
    TextDelta(&'a str),
}

/// Result from running one gate.
pub enum GateResult {
    Allow,
    Block { reason: String, gate_id: String },
    Edit,
    Request { prompt: String, gate_id: String },
}

/// Interface for all gates.
pub trait Gate: Send + Sync {
    fn id(&self) -> &str;
    fn band(&self) -> Band;
    fn direction(&self) -> Direction;
    fn check(&self, event: &mut GateEvent) -> GateResult;
}

/// Gate pipeline with explicit bands.
pub struct Pipeline {
    assemble: Vec<Box<dyn Gate>>,
    enrich: Vec<Box<dyn Gate>>,
    sanitize: Vec<Box<dyn Gate>>,
    validate: Vec<Box<dyn Gate>>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            assemble: Vec::new(),
            enrich: Vec::new(),
            sanitize: Vec::new(),
            validate: Vec::new(),
        }
    }

    pub fn assemble(mut self, gate: impl Gate + 'static) -> Self {
        self.assemble.push(Box::new(gate));
        self
    }

    pub fn enrich(mut self, gate: impl Gate + 'static) -> Self {
        self.enrich.push(Box::new(gate));
        self
    }

    pub fn sanitize(mut self, gate: impl Gate + 'static) -> Self {
        self.sanitize.push(Box::new(gate));
        self
    }

    pub fn validate(mut self, gate: impl Gate + 'static) -> Self {
        self.validate.push(Box::new(gate));
        self
    }

    /// Run all inbound gates on the outbound/ inbound message list before LLM request.
    pub fn run_inbound(&self, messages: &mut Vec<ChatMessage>) -> GateResult {
        let mut event = GateEvent::Messages(messages);
        let mut edited = false;

        for gate in &self.assemble {
            if matches!(gate.direction(), Direction::In | Direction::Both) {
                let verdict = gate.check(&mut event);
                match verdict {
                    GateResult::Allow => {}
                    GateResult::Edit => edited = true,
                    GateResult::Block { .. } | GateResult::Request { .. } => return verdict,
                }
            }
        }

        for gate in &self.enrich {
            if matches!(gate.direction(), Direction::In | Direction::Both) {
                let verdict = gate.check(&mut event);
                match verdict {
                    GateResult::Allow => {}
                    GateResult::Edit => edited = true,
                    GateResult::Block { .. } | GateResult::Request { .. } => return verdict,
                }
            }
        }

        for gate in &self.sanitize {
            if matches!(gate.direction(), Direction::In | Direction::Both) {
                let verdict = gate.check(&mut event);
                match verdict {
                    GateResult::Allow => {}
                    GateResult::Edit => edited = true,
                    GateResult::Block { .. } | GateResult::Request { .. } => return verdict,
                }
            }
        }

        if edited { GateResult::Edit } else { GateResult::Allow }
    }

    /// Gate a single tool call (individual check).
    pub fn check_tool_call(&self, call: &ToolCall) -> GateResult {
        let mut event = GateEvent::ToolCallComplete(call);
        let mut has_edit = false;

        for gate in &self.validate {
            if matches!(gate.direction(), Direction::Out | Direction::Both) {
                let verdict = gate.check(&mut event);
                match verdict {
                    GateResult::Allow => {}
                    GateResult::Edit => has_edit = true,
                    GateResult::Block { .. } | GateResult::Request { .. } => return verdict,
                }
            }
        }

        if has_edit {
            GateResult::Edit
        } else {
            GateResult::Allow
        }
    }

    /// Gate all tool calls together for cross-call checks.
    pub fn check_tool_batch(&self, calls: &[ToolCall]) -> GateResult {
        let mut event = GateEvent::ToolCallBatch(calls);
        let mut has_edit = false;

        for gate in &self.validate {
            if matches!(gate.direction(), Direction::Out | Direction::Both) {
                let verdict = gate.check(&mut event);
                match verdict {
                    GateResult::Allow => {}
                    GateResult::Edit => has_edit = true,
                    GateResult::Block { .. } | GateResult::Request { .. } => return verdict,
                }
            }
        }

        if has_edit {
            GateResult::Edit
        } else {
            GateResult::Allow
        }
    }
}

/// Secret redaction gate. Replaces matching substrings with `[REDACTED]`.
pub struct SecretRedactor {
    id: String,
    patterns: Vec<regex::Regex>,
}

impl SecretRedactor {
    pub fn new(patterns: &[&str]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(|pattern| regex::Regex::new(pattern).ok())
            .collect();

        Self {
            id: "secret-redactor".to_string(),
            patterns,
        }
    }

    fn redact_text(&self, text: &mut String) -> bool {
        let mut edited = false;
        let original = text.clone();

        let mut next = text.clone();
        for pattern in &self.patterns {
            next = pattern.replace_all(&next, "[REDACTED]").to_string();
        }

        if next != original {
            *text = next;
            edited = true;
        }

        edited
    }

    fn redact_messages(&self, messages: &mut Vec<ChatMessage>) -> bool {
        let mut edited = false;

        for message in messages {
            for block in &mut message.content {
                match block {
                    MessageContent::Text { text } => {
                        if self.redact_text(text) {
                            edited = true;
                        }
                    }
                    MessageContent::ToolResult { result } => {
                        if self.redact_text(&mut result.content) {
                            edited = true;
                        }
                    }
                    MessageContent::ToolCall { .. } => {}
                }
            }
        }

        edited
    }
}

impl Gate for SecretRedactor {
    fn id(&self) -> &str {
        &self.id
    }

    fn band(&self) -> Band {
        Band::Sanitize
    }

    fn direction(&self) -> Direction {
        Direction::Both
    }

    fn check(&self, event: &mut GateEvent) -> GateResult {
        match event {
            GateEvent::Messages(messages) => {
                if self.redact_messages(messages) {
                    GateResult::Edit
                } else {
                    GateResult::Allow
                }
            }
            _ => GateResult::Allow,
        }
    }
}

/// Heuristic shell validator used for tool call argument inspection.
pub struct ShellHeuristic {
    id: String,
    split_re: regex::Regex,
    fork_bomb_re: regex::Regex,
}

impl ShellHeuristic {
    pub fn new() -> Self {
        Self {
            id: "shell-heuristic".to_string(),
            split_re: regex::Regex::new(r"\s*(\|\||&&|;|\|)\s*").expect("valid split regex"),
            fork_bomb_re: regex::Regex::new(r":\(\)\s*\{\s*:\|:&\s*;\s*:\}").expect("valid fork bomb regex"),
        }
    }

    fn command_from_args(&self, call: &ToolCall) -> Option<String> {
        let value = from_str::<Value>(&call.arguments).ok()?;
        value.get("command").and_then(Value::as_str).map(ToString::to_string)
    }

    fn is_root_recursive_rm(binary: &str, args: &[&str]) -> bool {
        if binary != "rm" {
            return false;
        }

        let mut recursive = false;
        let mut deletes_root = false;

        for arg in args {
            let arg = arg.to_lowercase();
            if arg == "-rf" || arg == "-fr" || arg == "-r" || arg == "-R" {
                recursive = true;
            }
            if arg == "/" || arg == "/*" {
                deletes_root = true;
            }
        }

        recursive && deletes_root
    }

    fn is_blocked(binary: &str, args: &[&str]) -> Option<String> {
        if self.blocklist_match(binary, args).is_some() {
            return self.blocklist_match(binary, args);
        }
        if binary == "dd" && args.iter().any(|arg| arg.starts_with("if=")) {
            return Some("dd command with if= input redirection is blocked".to_string());
        }
        if self.fork_bomb_re.is_match(&args.join(" ")) {
            return Some("fork bomb pattern detected".to_string());
        }

        None
    }

    fn blocklist_match(&self, binary: &str, args: &[&str]) -> Option<String> {
        let joined = args.join(" ");
        match binary {
            "mkfs" | "format" | "shutdown" | "reboot" => {
                Some(format!("{binary} usage is blocked"))
            }
            "rm" if Self::is_root_recursive_rm(binary, args) => {
                Some(format!("rm -rf on {joined} is blocked"))
            }
            _ => None,
        }
    }

    fn is_request(binary: &str, args: &[&str]) -> Option<String> {
        match binary {
            "sudo" | "chmod" | "chown" | "kill" | "pkill" | "systemctl" => {
                Some(format!("{binary} usage requires approval"))
            }
            "rm" => Some("rm usage requires approval".to_string()),
            "apt" | "yum" => {
                if args.iter().any(|arg| *arg == "install" || *arg == "remove") {
                    Some(format!("{binary} install/remove requires approval"))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn analyze_segment(&self, segment: &str) -> GateResult {
        let mut request: Option<String> = None;
        let mut tokens = segment.split_whitespace().collect::<Vec<_>>();
        if tokens.is_empty() {
            return GateResult::Allow;
        }

        // Skip assignments at the beginning of commands.
        while !tokens.is_empty() && tokens[0].contains('=') {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            return GateResult::Allow;
        }

        let binary = tokens[0].to_lowercase();
        let args: Vec<&str> = tokens.iter().map(|token| token.as_ref()).collect();

        if let Some(reason) = self.is_blocked(&binary, &args) {
            return GateResult::Block {
                reason,
                gate_id: self.id.clone(),
            };
        }

        if let Some(reason) = Self::is_request(&binary, &args) {
            request = Some(reason);
        }

        if let Some(reason) = request {
            GateResult::Request {
                prompt: reason,
                gate_id: self.id.clone(),
            }
        } else {
            GateResult::Allow
        }
    }
}

impl Default for ShellHeuristic {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate for ShellHeuristic {
    fn id(&self) -> &str {
        &self.id
    }

    fn band(&self) -> Band {
        Band::Validate
    }

    fn direction(&self) -> Direction {
        Direction::Out
    }

    fn check(&self, event: &mut GateEvent) -> GateResult {
        match event {
            GateEvent::ToolCallComplete(call) => {
                let command = match self.command_from_args(call) {
                    Some(command) => command,
                    None => return GateResult::Allow,
                };

                if self.fork_bomb_re.is_match(&command) {
                    return GateResult::Block {
                        reason: "fork bomb pattern detected".to_string(),
                        gate_id: self.id.clone(),
                    };
                }

                let segments = self
                    .split_re
                    .split(&command)
                    .map(str::trim)
                    .filter(|segment| !segment.is_empty())
                    .collect::<Vec<_>>();

                let mut most_restrictive: GateResult = GateResult::Allow;

                for segment in segments {
                    match self.analyze_segment(segment) {
                        GateResult::Allow => {}
                        GateResult::Edit => {
                            most_restrictive = GateResult::Edit;
                        }
                        GateResult::Block { reason, .. } => {
                            return GateResult::Block {
                                reason,
                                gate_id: self.id.clone(),
                            }
                        }
                        GateResult::Request { reason, .. } => {
                            most_restrictive = GateResult::Request {
                                prompt: reason,
                                gate_id: self.id.clone(),
                            };
                        }
                    }
                }

                match most_restrictive {
                    GateResult::Request { reason, .. } => GateResult::Request {
                        prompt: reason,
                        gate_id: self.id.clone(),
                    },
                    GateResult::Edit => GateResult::Edit,
                    _ => GateResult::Allow,
                }
            }
            _ => GateResult::Allow,
        }
    }
}

/// Batch gate to catch read + send patterns across tool calls.
pub struct ExfiltrationDetector {
    id: String,
}

impl ExfiltrationDetector {
    pub fn new() -> Self {
        Self {
            id: "exfiltration-detector".to_string(),
        }
    }

    fn command_from_args(&self, call: &ToolCall) -> Option<String> {
        let value = from_str::<Value>(&call.arguments).ok()?;
        value.get("command").and_then(Value::as_str).map(ToString::to_string)
    }

    fn has_sensitive_read(command: &str) -> bool {
        let command = command.to_lowercase();
        command.contains("/etc/passwd")
            || command.contains("~/.ssh")
            || command.contains(".env")
            || command.contains("auth.json")
    }

    fn has_send_path(command: &str) -> bool {
        let command = command.to_lowercase();
        command.contains("/dev/tcp")
            || command.contains(" curl ")
            || command.starts_with("curl ")
            || command.ends_with(" curl")
            || command.contains(" wget ")
            || command.starts_with("wget ")
            || command.ends_with(" wget")
            || command.contains(" nc ")
            || command.starts_with("nc ")
            || command.ends_with(" nc")
    }
}

impl Default for ExfiltrationDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate for ExfiltrationDetector {
    fn id(&self) -> &str {
        &self.id
    }

    fn band(&self) -> Band {
        Band::Validate
    }

    fn direction(&self) -> Direction {
        Direction::Out
    }

    fn check(&self, event: &mut GateEvent) -> GateResult {
        match event {
            GateEvent::ToolCallBatch(calls) => {
                let mut seen_read = false;
                let mut seen_send = false;

                for call in calls {
                    let Some(command) = self.command_from_args(call) else {
                        continue;
                    };

                    if Self::has_sensitive_read(&command) {
                        seen_read = true;
                    }
                    if Self::has_send_path(&command) {
                        seen_send = true;
                    }
                }

                if seen_read && seen_send {
                    return GateResult::Block {
                        reason: "possible read-and-send exfiltration sequence detected across tool calls".to_string(),
                        gate_id: self.id.clone(),
                    };
                }

                GateResult::Allow
            }
            _ => GateResult::Allow,
        }
    }
}
