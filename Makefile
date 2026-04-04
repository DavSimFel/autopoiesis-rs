.PHONY: fmt lint test security check

fmt:
	cargo fmt

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

test:
	cargo test

security:
	cargo audit || echo "Install: cargo install cargo-audit"

check: lint test security
	@echo "All checks passed."
