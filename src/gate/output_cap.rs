use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

const DEFAULT_INLINE_CAP_BYTES: usize = 4096;
const RESULTS_DIR_NAME: &str = "results";
const CALL_PREFIX: &str = "call_";
const EMPTY_CALL_FALLBACK_SUFFIX: &str = "empty";
const HEX_WIDTH: usize = 2;
const KB_DIVISOR: usize = 1024;
const SED_PREVIEW_START_LINE: usize = 10;
const SED_PREVIEW_END_LINE: usize = 20;

pub(crate) const DEFAULT_OUTPUT_CAP_BYTES: usize = DEFAULT_INLINE_CAP_BYTES;

pub(crate) fn safe_call_id_for_filename(call_id: &str) -> String {
    let mut safe = String::from(CALL_PREFIX);
    for byte in call_id.as_bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => safe.push(*byte as char),
            _ => write!(&mut safe, "_{:0width$X}", byte, width = HEX_WIDTH)
                .expect("writing to String cannot fail"),
        }
    }

    if safe == CALL_PREFIX {
        safe.push_str(EMPTY_CALL_FALLBACK_SUFFIX);
    }

    safe
}

pub(crate) fn cap_tool_output(
    sessions_dir: &Path,
    call_id: &str,
    output: String,
    threshold: usize,
) -> Result<String> {
    let results_dir = sessions_dir.join(RESULTS_DIR_NAME);
    fs::create_dir_all(&results_dir).with_context(|| {
        format!(
            "failed to create results directory {}",
            results_dir.display()
        )
    })?;

    let result_path = results_dir.join(format!("{}.txt", safe_call_id_for_filename(call_id)));
    fs::write(&result_path, &output)
        .with_context(|| format!("failed to write tool output to {}", result_path.display()))?;

    if output.len() <= threshold {
        return Ok(output);
    }

    let line_count = output.lines().count();
    let size_kb = output.len().div_ceil(KB_DIVISOR);
    let path_display = result_path.display();
    Ok(format!(
        "[output exceeded inline limit ({line_count} lines, {size_kb} KB) -> {path_display}]\nTo read: cat {path_display}\nTo read specific lines: sed -n '{SED_PREVIEW_START_LINE},{SED_PREVIEW_END_LINE}p' {path_display}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sessions_dir(prefix: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "aprs_output_cap_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn tool_output_below_threshold_is_inline_and_saved_to_file() {
        let dir = temp_sessions_dir("inline");
        let call_id = "call-inline";
        let output = "stdout:\nsmall output\nstderr:\n\nexit_code=0".to_string();

        let result =
            cap_tool_output(&dir, call_id, output.clone(), DEFAULT_OUTPUT_CAP_BYTES).unwrap();

        assert_eq!(result, output);
        let result_path = dir
            .join(RESULTS_DIR_NAME)
            .join(format!("{}.txt", safe_call_id_for_filename(call_id)));
        assert!(result_path.exists(), "result file should be created");
        assert_eq!(fs::read_to_string(result_path).unwrap(), output);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_tool_output_sanitizes_call_id_before_path_use() {
        let dir = temp_sessions_dir("sanitized");
        let call_id = "../../escape;rm -rf /";
        let output = "line\n".repeat(2048);
        let sanitized = safe_call_id_for_filename(call_id);

        let capped =
            cap_tool_output(&dir, call_id, output.clone(), DEFAULT_OUTPUT_CAP_BYTES).unwrap();

        let result_path = dir.join(RESULTS_DIR_NAME).join(format!("{sanitized}.txt"));
        let result_path_str = result_path.display().to_string();
        assert!(
            result_path.exists(),
            "sanitized result file should be created"
        );
        assert_eq!(fs::read_to_string(&result_path).unwrap(), output);
        assert!(capped.contains(&result_path_str));
        assert!(!capped.contains(call_id));
        assert!(
            sanitized
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cap_tool_output_creates_results_directory() {
        let dir = temp_sessions_dir("creates_dir");
        let output = "stdout:\nhello\nstderr:\n\nexit_code=0".to_string();

        let result =
            cap_tool_output(&dir, "call-dir", output.clone(), DEFAULT_OUTPUT_CAP_BYTES).unwrap();

        assert_eq!(result, output);
        let result_path = dir
            .join(RESULTS_DIR_NAME)
            .join(format!("{}.txt", safe_call_id_for_filename("call-dir")));
        assert!(result_path.exists(), "result file should be created");
        assert_eq!(fs::read_to_string(result_path).unwrap(), output);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_output_above_threshold_is_capped_with_metadata_pointer() {
        let dir = temp_sessions_dir("capped");
        let call_id = "call-capped";
        let output = "line\n".repeat(2048);

        let capped =
            cap_tool_output(&dir, call_id, output.clone(), DEFAULT_OUTPUT_CAP_BYTES).unwrap();

        let expected_path = dir
            .join(RESULTS_DIR_NAME)
            .join(format!("{}.txt", safe_call_id_for_filename(call_id)));
        let expected_path_str = expected_path.display().to_string();
        assert!(capped.contains(&format!(
            "[output exceeded inline limit (2048 lines, 10 KB) -> {expected_path_str}]"
        )));
        assert!(capped.contains(&format!("To read: cat {expected_path_str}")));
        assert!(capped.contains(&format!(
            "To read specific lines: sed -n '10,20p' {expected_path_str}"
        )));
        assert!(expected_path.exists(), "result file should be created");
        assert_eq!(fs::read_to_string(expected_path).unwrap(), output);

        let _ = fs::remove_dir_all(&dir);
    }
}
