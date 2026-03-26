#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

config_paths="src/config.rs"
if [ -d src/config ]; then
  config_paths="src/config"
fi

if grep -RIn --include='*.rs' --exclude-dir='tests' --exclude='tests.rs' -F '#[allow(' src; then
  echo 'xtask lint: production code must not use #[allow(...)]' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -F 'format!(r#"{{"error": "' src; then
  echo 'xtask lint: manual JSON error strings are forbidden' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -F 'pub default: String' "$config_paths"; then
  echo 'xtask lint: shell policy defaults must be typed, not raw String fields' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -F 'pub default_severity: String' "$config_paths"; then
  echo 'xtask lint: shell policy severity must be typed, not raw String fields' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -F '_ => ShellDefaultAction::Approve' src/gate/shell_safety.rs; then
  echo 'xtask lint: unknown shell default must not fall back to approve' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -F '_ => Severity::Medium' src/gate/shell_safety.rs; then
  echo 'xtask lint: unknown shell severity must not fall back to medium' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -E 'Regex::new\([^)]*\)\.ok\(\)' src/gate/secret_redactor.rs; then
  echo 'xtask lint: SecretRedactor must validate regexes and fail closed' >&2
  exit 1
fi

if grep -RIn --include='*.rs' -E 'shell_words::split\([^)]*\)\.ok\(\)' src/gate/shell_safety.rs src/gate/exfil_detector.rs; then
  echo 'xtask lint: shell parsing must not fail open' >&2
  exit 1
fi
