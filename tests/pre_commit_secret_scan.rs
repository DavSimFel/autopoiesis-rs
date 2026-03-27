use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn helper_script() -> PathBuf {
    repo_root().join("scripts/pre_commit_secret_scan.sh")
}

struct TempRepo {
    root: PathBuf,
}

impl TempRepo {
    fn new(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "autopoiesis_pre_commit_secret_scan_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let status = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        Self { root }
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl AsRef<Path> for TempRepo {
    fn as_ref(&self) -> &Path {
        &self.root
    }
}

fn temp_repo(prefix: &str) -> TempRepo {
    TempRepo::new(prefix)
}

fn write_staged_file<P: AsRef<Path>>(repo: P, relative: &str, contents: &str) {
    let path = repo.as_ref().join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, contents).unwrap();
    let status = Command::new("git")
        .arg("add")
        .arg(relative)
        .current_dir(repo.as_ref())
        .status()
        .unwrap();
    assert!(status.success());
}

fn run_helper<P: AsRef<Path>>(repo: P) -> std::process::Output {
    Command::new("bash")
        .arg(helper_script())
        .current_dir(repo.as_ref())
        .output()
        .unwrap()
}

#[test]
fn rejects_sk_secret_in_non_test_rust_file() {
    let repo = temp_repo("rejects_non_test_sk");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r#"pub fn demo() {
    let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    println!("{key}");
}
"#,
    );

    let output = run_helper(&repo);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("possible secret"));
}

#[test]
fn allows_sk_secret_in_cfg_test_block() {
    let repo = temp_repo("allows_cfg_test_sk");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r###"pub fn demo() {}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_fixture() {
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        assert!(!key.is_empty());
    }
}
"###,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn allows_sk_secret_in_cfg_test_block_with_hashed_raw_string() {
    let repo = temp_repo("allows_cfg_test_sk_raw_string");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r###"pub fn demo() {}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_fixture() {
        let payload = r#"{"marker":"value"}"#;
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        assert!(payload.contains("marker"));
        assert!(!key.is_empty());
    }
}
"###,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn allows_sk_secret_in_cfg_test_block_with_lifetime_signature() {
    let repo = temp_repo("allows_cfg_test_sk_lifetime");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r#"pub fn demo() {}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_lifetime_helper() {
        fn helper<'a>(value: &'a str) -> &'a str {
            value
        }

        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        assert_eq!(helper("value"), "value");
        assert!(!key.is_empty());
    }
}
"#,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rejects_sk_secret_in_mixed_file_outside_cfg_test_block() {
    let repo = temp_repo("rejects_mixed_file_sk");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r#"pub fn demo() {
    let marker = "{";
    println!("{marker}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_fixture() {
        let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        assert!(!key.is_empty());
    }
}

pub fn prod() {
    let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    println!("{key}");
}
"#,
    );

    let output = run_helper(&repo);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("possible secret"));
}

#[test]
fn allows_sk_secret_in_test_only_rust_file() {
    let repo = temp_repo("allows_test_only_sk");
    write_staged_file(
        &repo,
        "src/feature/tests.rs",
        r#"#[test]
fn uses_fixture() {
    let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    assert!(!key.is_empty());
}
"#,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn allows_sk_secret_in_nested_tests_directory_file() {
    let repo = temp_repo("allows_nested_tests_file");
    write_staged_file(
        &repo,
        "src/agent/tests/common.rs",
        r#"#[test]
fn uses_fixture() {
    let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    assert!(!key.is_empty());
}
"#,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn allows_large_non_secret_file() {
    let repo = temp_repo("allows_large_non_secret_file");
    let large_content = "a".repeat(3 * 1024 * 1024);
    write_staged_file(&repo, "src/big.txt", &large_content);

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rejects_sk_secret_in_nested_tests_non_rust_file() {
    let repo = temp_repo("rejects_nested_tests_non_rust_file");
    write_staged_file(
        &repo,
        "src/agent/tests/fixture.txt",
        "sk-proj-abcdefghijklmnopqrstuvwxyz012345\n",
    );

    let output = run_helper(&repo);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("possible secret"));
}

#[test]
fn allows_sk_secret_in_nested_tests_mod_file() {
    let repo = temp_repo("allows_nested_tests_mod");
    write_staged_file(
        &repo,
        "src/agent/tests/mod.rs",
        r#"#[test]
fn uses_fixture() {
    let key = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    assert!(!key.is_empty());
}
"#,
    );

    let output = run_helper(&repo);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rejects_other_secret_patterns_everywhere() {
    let repo = temp_repo("rejects_other_patterns");
    write_staged_file(
        &repo,
        "src/lib.rs",
        r#"pub fn demo() {
    let github = "ghp_123456789012345678901234567890123456";
    let aws = "AKIA1234567890ABCDEF";
    let pem = "-----BEGIN PRIVATE KEY-----";
    println!("{github}{aws}{pem}");
}
"#,
    );

    let output = run_helper(&repo);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("possible secret"));
}
