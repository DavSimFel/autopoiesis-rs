#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

prod_dir="src/__lint_path_probe__"
ignored_src_dir="$prod_dir/tests"
ignored_root_dir="tests/__lint_path_probe__"

cleanup() {
  rm -rf "$prod_dir" "$ignored_root_dir"
}

trap cleanup EXIT INT TERM

mkdir -p "$ignored_src_dir" "$ignored_root_dir"

cat >"$prod_dir/tests.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_prod_tests_rs_probe() {}
EOF

cat >"$ignored_root_dir/probe.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_root_probe() {}
EOF

cat >"$ignored_src_dir/tests.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_src_tests_rs_probe() {}
EOF

cat >"$ignored_src_dir/probe.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_src_tests_dir_probe() {}
EOF

cat >"$prod_dir/probe.rs" <<'EOF'
#[allow(dead_code)]
fn production_probe() {}
EOF

if ./xtask/lint_checks.sh >/tmp/xtask_lint_paths_pass.log 2>&1; then
  echo 'xtask lint smoke test: expected production probe to fail lint' >&2
  exit 1
fi

rm -f /tmp/xtask_lint_paths_pass.log
rm -f "$prod_dir/probe.rs"
rm -f "$ignored_root_dir/probe.rs"
rm -f "$ignored_src_dir/tests.rs" "$ignored_src_dir/probe.rs"
rmdir "$ignored_src_dir" 2>/dev/null || true
rmdir "$prod_dir" 2>/dev/null || true
rmdir "$ignored_root_dir" 2>/dev/null || true

mkdir -p "$ignored_src_dir" "$ignored_root_dir"

cat >"$ignored_root_dir/probe.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_root_probe() {}
EOF

cat >"$ignored_src_dir/tests.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_src_tests_rs_probe() {}
EOF

cat >"$ignored_src_dir/probe.rs" <<'EOF'
#[allow(dead_code)]
fn ignored_src_tests_dir_probe() {}
EOF

if ! ./xtask/lint_checks.sh >/tmp/xtask_lint_paths_ok.log 2>&1; then
  cat /tmp/xtask_lint_paths_ok.log >&2
  echo 'xtask lint smoke test: ignored test-path probes should not fail lint' >&2
  exit 1
fi

rm -f /tmp/xtask_lint_paths_ok.log
rm -f "$prod_dir/tests.rs"
