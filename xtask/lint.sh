#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

./xtask/lint_checks.sh

cargo build --release
cargo fmt --check
cargo clippy -- -D warnings
cargo test

if [ -f "$HOME/.autopoiesis/auth.json" ]; then
  cargo test --features integration --test integration
fi
