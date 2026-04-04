use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::store::SubscriptionRow;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SubscriptionFilter {
    Full,
    Lines { start: usize, end: usize },
    Regex { pattern: String },
    Head { count: usize },
    Tail { count: usize },
    Jq { expression: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubscriptionRecord {
    pub id: i64,
    pub session_id: Option<String>,
    pub topic: String,
    pub path: PathBuf,
    pub filter: SubscriptionFilter,
    pub activated_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum JqSegment {
    Field(String),
    Index(usize),
    Iterate,
}

impl SubscriptionFilter {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lines { .. } => "lines",
            Self::Regex { .. } => "regex",
            Self::Head { .. } => "head",
            Self::Tail { .. } => "tail",
            Self::Jq { .. } => "jq",
        }
    }

    pub fn from_flags(
        lines: Option<&str>,
        regex: Option<&str>,
        head: Option<usize>,
        tail: Option<usize>,
        jq: Option<&str>,
    ) -> Result<Self> {
        let mut selected = None;
        let mut count = 0usize;

        if let Some(spec) = lines {
            count += 1;
            selected = Some(Self::parse_lines(spec)?);
        }

        if let Some(pattern) = regex {
            count += 1;
            Regex::new(pattern).context("invalid regex filter")?;
            selected = Some(Self::Regex {
                pattern: pattern.to_string(),
            });
        }

        if let Some(value) = head {
            count += 1;
            if value == 0 {
                return Err(anyhow!("head count must be at least 1"));
            }
            selected = Some(Self::Head { count: value });
        }

        if let Some(value) = tail {
            count += 1;
            if value == 0 {
                return Err(anyhow!("tail count must be at least 1"));
            }
            selected = Some(Self::Tail { count: value });
        }

        if let Some(expression) = jq {
            count += 1;
            validate_jq_expression(expression)?;
            selected = Some(Self::Jq {
                expression: expression.to_string(),
            });
        }

        if count > 1 {
            return Err(anyhow!("exactly one filter flag may be supplied"));
        }

        Ok(selected.unwrap_or(Self::Full))
    }

    fn parse_lines(spec: &str) -> Result<Self> {
        let (start, end) = spec
            .split_once('-')
            .ok_or_else(|| anyhow!("lines filter must be N-M"))?;
        let start = start
            .trim()
            .parse::<usize>()
            .context("lines filter start must be an integer")?;
        let end = end
            .trim()
            .parse::<usize>()
            .context("lines filter end must be an integer")?;

        if start == 0 {
            return Err(anyhow!("lines filter start must be at least 1"));
        }
        if end < start {
            return Err(anyhow!(
                "lines filter end must be greater than or equal to start"
            ));
        }

        Ok(Self::Lines { start, end })
    }

    pub fn to_storage(&self) -> Option<String> {
        match self {
            Self::Full => None,
            Self::Lines { start, end } => Some(format!("lines:{start}-{end}")),
            Self::Regex { pattern } => Some(format!("regex:{pattern}")),
            Self::Head { count } => Some(format!("head:{count}")),
            Self::Tail { count } => Some(format!("tail:{count}")),
            Self::Jq { expression } => Some(format!("jq:{expression}")),
        }
    }

    pub fn from_storage(value: Option<&str>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::Full);
        };

        let (kind, payload) = value
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid subscription filter encoding"))?;

        match kind {
            "lines" => Self::parse_lines(payload),
            "regex" => Ok(Self::Regex {
                pattern: {
                    Regex::new(payload).context("invalid regex filter")?;
                    payload.to_string()
                },
            }),
            "head" => Ok(Self::Head {
                count: {
                    let count = payload
                        .parse::<usize>()
                        .context("head filter must be an integer")?;
                    if count == 0 {
                        return Err(anyhow!("head count must be at least 1"));
                    }
                    count
                },
            }),
            "tail" => Ok(Self::Tail {
                count: {
                    let count = payload
                        .parse::<usize>()
                        .context("tail filter must be an integer")?;
                    if count == 0 {
                        return Err(anyhow!("tail count must be at least 1"));
                    }
                    count
                },
            }),
            "jq" => {
                validate_jq_expression(payload)?;
                Ok(Self::Jq {
                    expression: payload.to_string(),
                })
            }
            _ => Err(anyhow!("unknown subscription filter kind")),
        }
    }

    pub fn render(&self, input: &str) -> Result<String> {
        match self {
            Self::Full => Ok(input.to_string()),
            Self::Lines { start, end } => {
                let mut out = Vec::new();
                for (index, line) in input.lines().enumerate() {
                    let line_number = index + 1;
                    if (*start..=*end).contains(&line_number) {
                        out.push(line);
                    }
                }
                Ok(out.join("\n"))
            }
            Self::Regex { pattern } => {
                let regex = Regex::new(pattern).context("invalid regex filter")?;
                let lines = input
                    .lines()
                    .filter(|line| regex.is_match(line))
                    .collect::<Vec<_>>();
                Ok(lines.join("\n"))
            }
            Self::Head { count } => {
                let lines = input.lines().take(*count).collect::<Vec<_>>();
                Ok(lines.join("\n"))
            }
            Self::Tail { count } => {
                let lines = input.lines().collect::<Vec<_>>();
                let start = lines.len().saturating_sub(*count);
                Ok(lines[start..].join("\n"))
            }
            Self::Jq { expression } => render_jq(input, expression),
        }
    }
}

impl SubscriptionRecord {
    pub fn from_row(row: SubscriptionRow) -> Result<Self> {
        Ok(Self {
            id: row.id,
            session_id: row.session_id,
            topic: row.topic,
            path: PathBuf::from(row.path),
            filter: SubscriptionFilter::from_storage(row.filter.as_deref())?,
            activated_at: row.activated_at,
            updated_at: row.updated_at,
        })
    }

    pub fn effective_at(&self) -> &str {
        if self.updated_at >= self.activated_at {
            &self.updated_at
        } else {
            &self.activated_at
        }
    }

    pub fn rendered_content(&self) -> Result<String> {
        let raw = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        self.filter.render(&raw)
    }

    pub fn utilization_tokens(&self) -> Result<usize> {
        let rendered = self.rendered_content()?;
        Ok(estimate_tokens(&rendered))
    }

    pub fn format_listing(&self) -> String {
        let filter = self.filter.label();
        format!(
            "{} | {} | {} | {}",
            self.topic,
            self.path.display(),
            filter,
            self.effective_at()
        )
    }
}

pub fn normalize_path(path: impl AsRef<Path>) -> Result<PathBuf> {
    let raw = path.as_ref();
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(raw)
    };

    Ok(normalize_lexical_path(&absolute))
}

pub fn ensure_readable_subscription_path(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(())
}

pub fn estimate_tokens(text: &str) -> usize {
    crate::llm::history_groups::estimate_text_tokens(text)
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn render_jq(input: &str, expression: &str) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_str(input).context("jq filter requires valid JSON input")?;
    let stages = parse_jq_expression(expression)?;
    let mut current = vec![value];

    for stage in stages {
        current = apply_jq_stage(current, &stage)?;
    }

    let mut rendered = Vec::with_capacity(current.len());
    for value in current {
        rendered.push(serde_json::to_string_pretty(&value).context("failed to render jq output")?);
    }

    Ok(rendered.join("\n"))
}

fn validate_jq_expression(expression: &str) -> Result<()> {
    parse_jq_expression(expression).map(|_| ())
}

fn parse_jq_expression(expression: &str) -> Result<Vec<Vec<JqSegment>>> {
    let expr = expression.trim();
    if expr.is_empty() {
        return Err(anyhow!("unsupported jq expression"));
    }

    let mut stages = Vec::new();
    for stage in expr.split('|') {
        stages.push(parse_jq_stage(stage.trim())?);
    }

    Ok(stages)
}

fn parse_jq_stage(stage: &str) -> Result<Vec<JqSegment>> {
    if stage.is_empty() {
        return Err(anyhow!("unsupported jq expression"));
    }
    if stage == "." {
        return Ok(Vec::new());
    }

    let bytes = stage.as_bytes();
    let mut index = 0usize;
    let mut segments = Vec::new();

    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        if index >= bytes.len() {
            break;
        }

        if segments.is_empty() && bytes[index] != b'.' {
            return Err(anyhow!("unsupported jq expression"));
        }

        if bytes[index] == b'.' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index >= bytes.len() {
                return Err(anyhow!("unsupported jq expression"));
            }
            if bytes[index] == b'.' {
                return Err(anyhow!("unsupported jq expression"));
            }
        } else if !segments.is_empty() && !matches!(bytes[index], b'[') {
            return Err(anyhow!("unsupported jq expression"));
        }

        match bytes[index] {
            b'[' => {
                index += 1;
                if index < bytes.len() && bytes[index] == b']' {
                    index += 1;
                    segments.push(JqSegment::Iterate);
                    continue;
                }

                let start = index;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
                if start == index || index >= bytes.len() || bytes[index] != b']' {
                    return Err(anyhow!("unsupported jq expression"));
                }
                let value = stage[start..index]
                    .parse::<usize>()
                    .context("jq array index must be an integer")?;
                index += 1;
                segments.push(JqSegment::Index(value));
            }
            _ => {
                let start = index;
                while index < bytes.len()
                    && !matches!(
                        bytes[index],
                        b'.' | b'[' | b'|' | b' ' | b'\t' | b'\r' | b'\n'
                    )
                {
                    index += 1;
                }
                if start == index {
                    return Err(anyhow!("unsupported jq expression"));
                }
                segments.push(JqSegment::Field(stage[start..index].to_string()));
            }
        }
    }

    Ok(segments)
}

fn apply_jq_stage(
    values: Vec<serde_json::Value>,
    stage: &[JqSegment],
) -> Result<Vec<serde_json::Value>> {
    let mut current = values;

    for segment in stage {
        let mut next = Vec::new();

        for value in std::mem::take(&mut current) {
            match segment {
                JqSegment::Field(name) => match value {
                    serde_json::Value::Object(map) => {
                        let selected = map
                            .get(name)
                            .ok_or_else(|| anyhow!("jq expression did not match any value"))?;
                        next.push(selected.clone());
                    }
                    _ => return Err(anyhow!("jq field access requires an object")),
                },
                JqSegment::Index(index) => match value {
                    serde_json::Value::Array(items) => {
                        let selected = items
                            .get(*index)
                            .ok_or_else(|| anyhow!("jq expression did not match any value"))?;
                        next.push(selected.clone());
                    }
                    _ => return Err(anyhow!("jq array index requires an array")),
                },
                JqSegment::Iterate => match value {
                    serde_json::Value::Array(items) => {
                        next.extend(items);
                    }
                    _ => return Err(anyhow!("jq array iteration requires an array")),
                },
            }
        }

        current = next;
    }

    Ok(current)
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{stamp}"))
    }

    #[test]
    fn parse_lines_filter() {
        assert!(matches!(
            SubscriptionFilter::from_flags(Some("2-4"), None, None, None, None).unwrap(),
            SubscriptionFilter::Lines { start: 2, end: 4 }
        ));
    }

    #[test]
    fn render_head_filter() {
        let rendered = SubscriptionFilter::Head { count: 2 }
            .render("a\nb\nc")
            .unwrap();
        assert_eq!(rendered, "a\nb");
    }

    #[test]
    fn rejects_multiple_filter_flags() {
        assert!(SubscriptionFilter::from_flags(Some("1-2"), None, Some(1), None, None).is_err());
    }

    #[test]
    fn round_trips_storage_filters() {
        let original = SubscriptionFilter::Jq {
            expression: ".items[0] | .name".to_string(),
        };
        let stored = original.to_storage().unwrap();
        let restored = SubscriptionFilter::from_storage(Some(&stored)).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn rejects_unsupported_jq_expression() {
        assert!(
            SubscriptionFilter::from_flags(None, None, None, None, Some(".items | length"))
                .is_err()
        );
    }

    #[test]
    fn renders_jq_pipeline_and_array_iteration() {
        let rendered = SubscriptionFilter::Jq {
            expression: ".items[] | .name".to_string(),
        }
        .render(r#"{"items":[{"name":"a"},{"name":"b"}]}"#)
        .unwrap();
        assert_eq!(rendered, "\"a\"\n\"b\"");
    }

    #[test]
    fn validates_subscription_path_before_insert() {
        let dir = unique_temp_dir("aprs-subs-path");
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("present.txt");
        fs::write(&existing, "hello").unwrap();
        assert!(ensure_readable_subscription_path(&existing).is_ok());

        let missing = dir.join("missing.txt");
        assert!(ensure_readable_subscription_path(&missing).is_err());
    }

    #[test]
    fn store_subscription_round_trip_and_refreshes_timestamps() {
        let dir = unique_temp_dir("aprs-subs-store");
        fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("queue.sqlite");
        let note = dir.join("note.txt");
        fs::write(&note, "hello").unwrap();

        let mut store = crate::store::Store::new(&db_path).unwrap();
        let normalized = normalize_path(&note).unwrap();
        let filter = SubscriptionFilter::Head { count: 1 };
        let id = store
            .create_subscription(
                "_default",
                normalized.to_str().unwrap(),
                filter.to_storage().as_deref(),
            )
            .unwrap();

        let rows = store.list_subscriptions(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);

        let record = SubscriptionRecord::from_row(rows[0].clone()).unwrap();
        assert_eq!(record.rendered_content().unwrap(), "hello");

        assert!(
            store
                .refresh_subscription_timestamps_with(|_| {
                    Some(UNIX_EPOCH + std::time::Duration::from_secs(4_000_000_000))
                })
                .unwrap()
                >= 1
        );

        let refreshed =
            SubscriptionRecord::from_row(store.list_subscriptions(None).unwrap()[0].clone())
                .unwrap();
        assert!(refreshed.updated_at >= record.updated_at);

        assert_eq!(
            store
                .delete_subscription("_default", normalized.to_str().unwrap())
                .unwrap(),
            1
        );
    }
}
