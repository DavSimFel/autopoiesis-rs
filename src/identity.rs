use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::template::render_template;

pub fn load_system_prompt_from_files(
    identity_files: &[PathBuf],
    vars: &HashMap<String, String>,
) -> Result<String> {
    if identity_files.is_empty() {
        return Err(anyhow!("identity file list is empty"));
    }

    let mut sections = Vec::with_capacity(identity_files.len());
    for path in identity_files {
        sections.push(
            fs::read_to_string(path)
                .map_err(|error| anyhow!("failed to read {}: {error}", path.display()))?,
        );
    }

    let template = sections.join("\n\n");
    Ok(render_template(&template, vars))
}

pub fn t1_identity_files(identity_root: impl AsRef<Path>, agent_name: &str) -> Vec<PathBuf> {
    let root = identity_root.as_ref();
    vec![
        root.join("constitution.md"),
        root.join("agents").join(agent_name).join("agent.md"),
        root.join("context.md"),
    ]
}

pub fn t2_identity_files(identity_root: impl AsRef<Path>) -> Vec<PathBuf> {
    let root = identity_root.as_ref();
    vec![root.join("constitution.md"), root.join("context.md")]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempIdentityDir {
        path: PathBuf,
    }

    impl Drop for TempIdentityDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    impl TempIdentityDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn temp_identity_dir(prefix: &str) -> TempIdentityDir {
        let mut path = env::temp_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        path.push(format!("autopoiesis_test_{prefix}_{now}"));
        fs::create_dir_all(&path).expect("failed to create temp identity dir");
        TempIdentityDir { path }
    }

    #[test]
    fn loads_and_concatenates_explicit_files() {
        let dir = temp_identity_dir("load_concat");
        let constitution = dir.path().join("constitution.md");
        let agent = dir.path().join("agents").join("silas");
        let agent_file = agent.join("agent.md");
        let context = dir.path().join("context.md");
        fs::create_dir_all(&agent).expect("failed to create agent dir");

        File::create(&constitution)
            .expect("failed to create constitution")
            .write_all(b"A")
            .expect("failed to write constitution");
        File::create(&agent_file)
            .expect("failed to create agent")
            .write_all(b"B")
            .expect("failed to write agent");
        File::create(&context)
            .expect("failed to create context")
            .write_all(b"C")
            .expect("failed to write context");

        let prompt =
            load_system_prompt_from_files(&[constitution, agent_file, context], &HashMap::new())
                .expect("expected prompt load to succeed");
        assert_eq!(prompt, "A\n\nB\n\nC");
    }

    #[test]
    fn applies_template_vars_to_concatenated_result() {
        let dir = temp_identity_dir("template_vars");
        let constitution = dir.path().join("constitution.md");
        let agent = dir.path().join("agents").join("silas");
        let agent_file = agent.join("agent.md");
        let context = dir.path().join("context.md");
        fs::create_dir_all(&agent).expect("failed to create agent dir");

        File::create(&constitution)
            .expect("failed to create constitution")
            .write_all(b"name: {{name}}")
            .expect("failed to write constitution");
        File::create(&agent_file)
            .expect("failed to create agent")
            .write_all(b"cwd: {{cwd}}")
            .expect("failed to write agent");
        File::create(&context)
            .expect("failed to create context")
            .write_all(b"done")
            .expect("failed to write context");

        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada".to_string());
        vars.insert("cwd".to_string(), "/tmp".to_string());

        let prompt = load_system_prompt_from_files(&[constitution, agent_file, context], &vars)
            .expect("expected prompt load to succeed");
        assert_eq!(prompt, "name: Ada\n\ncwd: /tmp\n\ndone");
    }

    #[test]
    fn t1_identity_files_include_agent_md() {
        let files = t1_identity_files("identity-templates", "silas");
        assert_eq!(
            files,
            vec![
                PathBuf::from("identity-templates/constitution.md"),
                PathBuf::from("identity-templates/agents/silas/agent.md"),
                PathBuf::from("identity-templates/context.md"),
            ]
        );
    }

    #[test]
    fn t2_identity_files_skip_agent_md() {
        let files = t2_identity_files("identity-templates");
        assert_eq!(
            files,
            vec![
                PathBuf::from("identity-templates/constitution.md"),
                PathBuf::from("identity-templates/context.md"),
            ]
        );
    }

    #[test]
    fn errors_when_any_identity_file_is_missing() {
        let dir = temp_identity_dir("missing_file");
        let constitution = dir.path().join("constitution.md");
        let context = dir.path().join("context.md");
        File::create(&constitution)
            .expect("failed to create constitution")
            .write_all(b"constitution")
            .expect("failed to write constitution");
        File::create(&context)
            .expect("failed to create context")
            .write_all(b"context")
            .expect("failed to write context");

        let result = load_system_prompt_from_files(
            &[
                constitution,
                dir.path().join("agents").join("silas").join("agent.md"),
                context,
            ],
            &HashMap::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn errors_when_file_list_is_empty() {
        let result = load_system_prompt_from_files(&[], &HashMap::new());
        assert!(result.is_err());
    }
}
