//! Structured file read tool with provenance tagging and path policy checks.

use std::convert::TryFrom;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use crate::config::ReadToolConfig;
use crate::gate::secret_patterns::path_is_protected;
use crate::llm::FunctionTool;
use crate::tool::{Tool, ToolFuture};

const PROVENANCE_PRINCIPAL: &str = "operator";

#[derive(Debug, Clone)]
pub struct ReadFile {
    allowed_paths: Vec<PathBuf>,
    max_read_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
    offset: Option<u64>,
    limit: Option<u64>,
}

impl ReadFile {
    pub fn new(allowed_paths: Vec<PathBuf>, max_read_bytes: usize) -> Self {
        Self {
            allowed_paths,
            max_read_bytes,
        }
    }

    pub fn from_config(config: &ReadToolConfig) -> Self {
        Self::new(
            config.allowed_paths.iter().map(PathBuf::from).collect(),
            config.max_read_bytes,
        )
    }

    fn parse_args(arguments: &str) -> Result<ReadFileArgs> {
        serde_json::from_str(arguments).context("failed to decode tool call arguments")
    }

    fn provenance_header(path: &str) -> String {
        format!(
            "<meta source=read_file path={} principal={PROVENANCE_PRINCIPAL} />",
            Self::encode_provenance_path(path)
        )
    }

    fn encode_provenance_path(path: &str) -> String {
        use std::fmt::Write;

        let mut encoded = String::with_capacity(path.len());
        for byte in path.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                    encoded.push(byte as char);
                }
                _ => {
                    let _ = write!(&mut encoded, "%{byte:02X}");
                }
            }
        }
        encoded
    }

    fn normalize_to_absolute(path: &Path) -> Result<PathBuf> {
        let mut normalized = if path.is_absolute() {
            PathBuf::from("/")
        } else {
            std::env::current_dir().context("failed to resolve current working directory")?
        };

        for component in path.components() {
            match component {
                Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
                Component::RootDir => {}
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::Normal(part) => normalized.push(part),
            }
        }

        Ok(normalized)
    }

    fn resolve_allowed_roots(allowed_paths: &[PathBuf]) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
        let normalized_roots = allowed_paths
            .iter()
            .map(|path| Self::normalize_to_absolute(path))
            .collect::<Result<Vec<_>>>()?;

        let canonical_roots = normalized_roots
            .iter()
            .map(|normalized| {
                if normalized.exists() {
                    std::fs::canonicalize(normalized).with_context(|| {
                        format!("failed to resolve allowed path {}", normalized.display())
                    })
                } else {
                    Ok(normalized.clone())
                }
            })
            .collect::<Result<Vec<_>>>()?;

        Ok((normalized_roots, canonical_roots))
    }

    fn path_is_explicitly_denied(path: &Path) -> bool {
        path_is_protected(path)
            || path
                .components()
                .any(|component| {
                    matches!(component, Component::Normal(part) if part == ".ssh" || part == "auth.json")
                })
            || path == Path::new("/etc/shadow")
    }

    fn is_within_allowed_root(path: &Path, allowed_roots: &[PathBuf]) -> bool {
        allowed_roots.iter().any(|root| path.starts_with(root))
    }

    fn read_text_file(path: &Path, max_read_bytes: usize) -> Result<String> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?;

        if metadata.is_dir() {
            return Err(anyhow!("access denied: path is not a file"));
        }

        if metadata.len() > max_read_bytes as u64 {
            return Err(anyhow!(
                "too large: {} exceeds {} bytes",
                path.display(),
                max_read_bytes
            ));
        }

        let bytes =
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() > max_read_bytes {
            return Err(anyhow!(
                "too large: {} exceeds {} bytes",
                path.display(),
                max_read_bytes
            ));
        }

        String::from_utf8(bytes).with_context(|| format!("failed to decode {}", path.display()))
    }

    fn slice_lines(contents: &str, offset: Option<u64>, limit: Option<u64>) -> Result<String> {
        let start = match offset {
            Some(0) => return Err(anyhow!("offset must be greater than zero")),
            Some(value) => usize::try_from(value).context("offset is too large")?,
            None => 1,
        };
        let count = match limit {
            Some(0) => return Err(anyhow!("limit must be greater than zero")),
            Some(value) => Some(usize::try_from(value).context("limit is too large")?),
            None => None,
        };

        let start_index = start.saturating_sub(1);
        let iter = contents.lines().skip(start_index);
        let selected: Vec<&str> = match count {
            Some(limit) => iter.take(limit).collect(),
            None => iter.collect(),
        };

        Ok(selected.join("\n"))
    }

    fn read_file_inner(
        args: ReadFileArgs,
        allowed_paths: Vec<PathBuf>,
        max_read_bytes: usize,
    ) -> Result<String> {
        let ReadFileArgs {
            path,
            offset,
            limit,
        } = args;

        if path.trim().is_empty() {
            return Err(anyhow!("tool call requires a non-empty 'path' argument"));
        }

        let requested_path = PathBuf::from(&path);
        let normalized_requested = Self::normalize_to_absolute(&requested_path)?;
        let (normalized_allowed_roots, canonical_allowed_roots) =
            Self::resolve_allowed_roots(&allowed_paths)?;

        if Self::path_is_explicitly_denied(&requested_path)
            || Self::path_is_explicitly_denied(&normalized_requested)
            || !Self::is_within_allowed_root(&normalized_requested, &normalized_allowed_roots)
        {
            return Err(anyhow!("access denied: path is outside allowed roots"));
        }

        if !normalized_requested.exists() {
            return Err(anyhow!("file not found: {}", requested_path.display()));
        }

        let canonical_requested = std::fs::canonicalize(&normalized_requested)
            .with_context(|| format!("failed to resolve {}", normalized_requested.display()))?;

        if Self::path_is_explicitly_denied(&canonical_requested) {
            return Err(anyhow!("access denied: protected path"));
        }

        if !Self::is_within_allowed_root(&canonical_requested, &canonical_allowed_roots) {
            return Err(anyhow!("access denied: path escapes allowed roots"));
        }

        let contents = Self::read_text_file(&canonical_requested, max_read_bytes)?;
        let sliced = Self::slice_lines(&contents, offset, limit)?;

        if sliced.is_empty() {
            Ok(Self::provenance_header(&path))
        } else {
            Ok(format!("{}\n{}", Self::provenance_header(&path), sliced))
        }
    }
}

impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn definition(&self) -> FunctionTool {
        FunctionTool {
            name: "read_file".to_string(),
            description: "Read a file with provenance metadata and optional line slicing"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to read"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional 1-based line number to start reading from"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional maximum number of lines to return"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    #[tracing::instrument(level = "debug", skip(self, arguments))]
    fn execute(&self, arguments: &str) -> ToolFuture<'_> {
        let arguments = arguments.to_string();
        let allowed_paths = self.allowed_paths.clone();
        let max_read_bytes = self.max_read_bytes;
        Box::pin(async move {
            let args = Self::parse_args(&arguments)?;
            let output = tokio::task::spawn_blocking(move || {
                Self::read_file_inner(args, allowed_paths, max_read_bytes)
            })
            .await
            .context("failed to join read_file blocking task")??;

            Ok(output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos()
    }

    fn temp_root(prefix: &str) -> PathBuf {
        let mut root = std::env::temp_dir();
        root.push(format!(
            "autopoiesis_read_tool_{prefix}_{}",
            unique_suffix()
        ));
        fs::create_dir_all(&root).expect("failed to create temp root");
        root
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("failed to create parent directories");
        }
        let mut file = File::create(path).expect("failed to create file");
        file.write_all(contents.as_bytes())
            .expect("failed to write file");
    }

    fn tool_with_allowed_root(root: &Path, max_read_bytes: usize) -> ReadFile {
        ReadFile::new(vec![root.to_path_buf()], max_read_bytes)
    }

    fn escaped_path(path: &Path) -> String {
        ReadFile::encode_provenance_path(path.to_string_lossy().as_ref())
    }

    fn call(tool: &ReadFile, arguments: serde_json::Value) -> String {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&arguments.to_string()))
            .expect("tool call should succeed")
    }

    #[test]
    fn definition_describes_required_path_and_optional_line_bounds() {
        let tool = ReadFile::new(vec![PathBuf::from(".")], 64);
        let definition = tool.definition();

        assert_eq!(definition.name, "read_file");
        assert!(definition.description.contains("provenance"));
        assert_eq!(definition.parameters["required"], json!(["path"]));
        assert_eq!(
            definition.parameters["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(definition.parameters["properties"]["offset"]["minimum"], 1);
        assert_eq!(definition.parameters["properties"]["limit"]["minimum"], 1);
        assert_eq!(definition.parameters["additionalProperties"], json!(false));
    }

    #[test]
    fn reads_file_with_provenance_header() {
        let root = temp_root("reads_file");
        let path = root.join("note.txt");
        write_file(&path, "alpha\nbeta\n");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(&tool, json!({ "path": path.to_string_lossy() }));

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nalpha\nbeta",
                escaped_path(&path)
            )
        );
    }

    #[test]
    fn reads_file_with_escaped_header_path() {
        let root = temp_root("escaped_header");
        let path = root.join("space > \"quote\".txt");
        write_file(&path, "alpha");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(&tool, json!({ "path": path.to_string_lossy() }));

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nalpha",
                escaped_path(&path)
            )
        );
    }

    #[test]
    fn resolves_relative_path_against_current_directory() {
        let rel_dir = format!("autopoiesis_read_tool_rel_{}", unique_suffix());
        let rel_root = std::env::current_dir()
            .expect("failed to read cwd")
            .join(&rel_dir);
        fs::create_dir_all(&rel_root).expect("failed to create relative test root");

        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let _cleanup = Cleanup(rel_root.clone());
        let rel_path = PathBuf::from(&rel_dir).join("file.txt");
        write_file(&rel_root.join("file.txt"), "from cwd");

        let tool = ReadFile::new(vec![PathBuf::from(".")], 64);
        let output = call(&tool, json!({ "path": rel_path.to_string_lossy() }));

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nfrom cwd",
                escaped_path(&rel_path)
            )
        );
    }

    #[test]
    fn slices_lines_with_offset_and_limit() {
        let root = temp_root("slice");
        let path = root.join("story.txt");
        write_file(&path, "zero\none\ntwo\nthree\n");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(
            &tool,
            json!({ "path": path.to_string_lossy(), "offset": 2, "limit": 2 }),
        );

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\none\ntwo",
                escaped_path(&path)
            )
        );
    }

    #[test]
    fn offset_past_eof_returns_header_only() {
        let root = temp_root("offset_eof");
        let path = root.join("story.txt");
        write_file(&path, "alpha\nbeta\n");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(
            &tool,
            json!({ "path": path.to_string_lossy(), "offset": 10 }),
        );

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />",
                escaped_path(&path)
            )
        );
    }

    #[test]
    fn limit_past_eof_truncates_naturally() {
        let root = temp_root("limit_eof");
        let path = root.join("story.txt");
        write_file(&path, "alpha\nbeta\n");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(
            &tool,
            json!({ "path": path.to_string_lossy(), "limit": 10 }),
        );

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nalpha\nbeta",
                escaped_path(&path)
            )
        );
    }

    #[test]
    fn offset_zero_is_rejected() {
        let root = temp_root("offset_zero");
        let path = root.join("story.txt");
        write_file(&path, "alpha");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(
                tool.execute(&json!({ "path": path.to_string_lossy(), "offset": 0 }).to_string()),
            )
            .expect_err("expected offset zero to fail");

        assert!(err.to_string().contains("offset must be greater than zero"));
    }

    #[test]
    fn limit_zero_is_rejected() {
        let root = temp_root("limit_zero");
        let path = root.join("story.txt");
        write_file(&path, "alpha");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(
                tool.execute(&json!({ "path": path.to_string_lossy(), "limit": 0 }).to_string()),
            )
            .expect_err("expected limit zero to fail");

        assert!(err.to_string().contains("limit must be greater than zero"));
    }

    #[test]
    fn path_outside_allowed_roots_is_denied() {
        let root = temp_root("outside_root");
        let other = temp_root("outside_target");
        let path = other.join("note.txt");
        write_file(&path, "secret");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": path.to_string_lossy() }).to_string()))
            .expect_err("expected path to be denied");

        assert!(err.to_string().contains("access denied"));

        fs::remove_dir_all(&other).expect("failed to clean temp root");
    }

    #[test]
    fn env_example_is_allowed_when_shared_policy_allows_it() {
        let root = temp_root("env_example");
        let path = root.join(".env.example");
        write_file(&path, "EXAMPLE=1");

        let tool = tool_with_allowed_root(&root, 64);
        let output = call(&tool, json!({ "path": path.to_string_lossy() }));

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nEXAMPLE=1",
                escaped_path(&path)
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_allowed_root_is_respected() {
        let target_root = temp_root("symlink_root_target");
        let allowed_root = temp_root("symlink_root_link");
        let symlink_root = allowed_root.join("linked");
        symlink(&target_root, &symlink_root).expect("failed to create root symlink");

        let path = symlink_root.join("note.txt");
        write_file(&target_root.join("note.txt"), "linked root");

        let tool = tool_with_allowed_root(&symlink_root, 64);
        let output = call(&tool, json!({ "path": path.to_string_lossy() }));

        assert_eq!(
            output,
            format!(
                "<meta source=read_file path={} principal=operator />\nlinked root",
                escaped_path(&path)
            )
        );

        fs::remove_dir_all(&allowed_root).expect("failed to clean temp root");
        fs::remove_dir_all(&target_root).expect("failed to clean temp root");
    }

    #[test]
    fn traversal_out_of_allowed_root_is_denied() {
        let root = temp_root("traversal");
        let nested = root.join("nested");
        let path = nested.join("..").join("..").join("escape.txt");
        write_file(&root.join("escape.txt"), "secret");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": path.to_string_lossy() }).to_string()))
            .expect_err("expected traversal to be denied");

        assert!(err.to_string().contains("access denied"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_denied() {
        let root = temp_root("symlink");
        let outside = temp_root("symlink_outside");
        let outside_path = outside.join("note.txt");
        write_file(&outside_path, "secret");

        let symlink_path = root.join("link.txt");
        symlink(&outside_path, &symlink_path).expect("failed to create symlink");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": symlink_path.to_string_lossy() }).to_string()))
            .expect_err("expected symlink escape to be denied");

        assert!(err.to_string().contains("access denied"));

        fs::remove_dir_all(&outside).expect("failed to clean temp root");
    }

    #[test]
    fn protected_path_is_denied_even_under_allowed_root() {
        let root = temp_root("protected");
        let path = root.join("auth.json");
        write_file(&path, "{}");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": path.to_string_lossy() }).to_string()))
            .expect_err("expected protected path to be denied");

        assert!(err.to_string().contains("access denied"));
    }

    #[test]
    fn missing_file_returns_clear_error() {
        let root = temp_root("missing");
        let path = root.join("absent.txt");

        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": path.to_string_lossy() }).to_string()))
            .expect_err("expected missing file to fail");

        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn oversized_file_is_rejected() {
        let root = temp_root("too_large");
        let path = root.join("big.txt");
        write_file(&path, "1234567890");

        let tool = tool_with_allowed_root(&root, 4);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": path.to_string_lossy() }).to_string()))
            .expect_err("expected large file to fail");

        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn empty_path_argument_is_rejected() {
        let root = temp_root("empty_path");
        let tool = tool_with_allowed_root(&root, 64);
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime")
            .block_on(tool.execute(&json!({ "path": "   " }).to_string()))
            .expect_err("expected empty path to fail");

        assert!(err.to_string().contains("non-empty 'path' argument"));
    }
}
