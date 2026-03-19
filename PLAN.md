# PLAN: GitHub Actions CI for autopoiesis-rs

## Repo facts established

- `Cargo.toml` defines a single Rust package (`autopoiesis`) on edition `2024`.
- `Cargo.toml` has an opt-in `integration` feature.
- `tests/integration.rs` gates every test behind `#[cfg(feature = "integration")]` and those tests require live auth/API access.
- `.github/` does not exist yet, so the workflow should be added from scratch.

## Files to read

- `Cargo.toml`
  - Confirm the crate is a single package and that the `integration` feature is opt-in.
- `tests/integration.rs`
  - Confirm integration tests are feature-gated and should not run in CI.
- `.github/`
  - Confirm there is no existing workflow or repository automation to merge with.

## File to create

- `.github/workflows/ci.yml`

## Workflow specification

- Create exactly one workflow file.
- Keep the workflow minimal: one workflow, one job, no matrix.

### Trigger behavior

- Run on every `pull_request`.
- Run on `push` only when the branch is `main`.

### Job definition

- Use a single job named `ci`.
- Run the job on `ubuntu-latest`.
- Use the stable Rust toolchain.
- Ensure both `rustfmt` and `clippy` components are installed because the workflow must run formatting and lint checks.

### Step order

1. Check out the repository.
2. Install the stable Rust toolchain with `rustfmt` and `clippy`.
3. Restore Cargo/build caches before any Cargo command runs.
4. Run `cargo fmt --check`.
5. Run `cargo clippy -- -D warnings`.
6. Run `cargo test`.

### Caching requirements

- Cache the Cargo registry and the `target/` directory.
- Preferred implementation: use a Rust-specific cache action that already handles Cargo home artifacts and `target/` with minimal configuration.
- The cache step must come after toolchain setup and before the first Cargo command.
- If the implementer chooses explicit cache configuration instead of a Rust-specific cache action, the cache key should include:
  - runner OS
  - Rust toolchain channel (`stable`)
  - dependency state from `Cargo.lock`

## Non-goals and guardrails

- Do not run `cargo test --features integration`.
- Do not use `--all-features` anywhere in the workflow.
- Do not add secrets, API-key setup, or a separate integration-test job.
- Do not introduce a build matrix, extra jobs, or additional workflow files.

## Order of operations for the implementer

1. Re-read `Cargo.toml` and `tests/integration.rs` to confirm the `integration` feature remains opt-in and excluded from CI.
2. Verify `.github/` is still absent or has no overlapping workflow file.
3. Create `.github/workflows/ci.yml`.
4. Add the `pull_request` and `push` to `main` triggers.
5. Add one `ci` job on `ubuntu-latest`.
6. Configure stable Rust with `rustfmt` and `clippy`.
7. Add caching for the Cargo registry and `target/`.
8. Add the three commands in this exact order:
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `cargo test`
9. Verify the workflow never enables the `integration` feature.
10. Run `cargo test` locally before committing, per repository rules. Do not run the integration-feature test path in CI.
