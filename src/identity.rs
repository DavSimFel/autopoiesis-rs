use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::template::render_template;

pub fn load_system_prompt(identity_dir: &str, vars: &HashMap<String, String>) -> Result<String> {
    let root = Path::new(identity_dir);
    if !root.exists() {
        return Err(anyhow!("identity directory missing: {identity_dir}"));
    }

    let paths = [
        root.join("constitution.md"),
        root.join("identity.md"),
        root.join("context.md"),
    ];

    let mut sections = Vec::with_capacity(paths.len());
    for path in paths {
        sections.push(fs::read_to_string(&path)?);
    }

    let template = sections.join("\n\n");
    Ok(render_template(&template, vars))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_identity_dir(prefix: &str) -> std::path::PathBuf {
        let mut path = env::temp_dir();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be valid")
            .as_nanos();
        path.push(format!("autopoiesis_test_{prefix}_{now}"));
        fs::create_dir_all(&path).expect("failed to create temp identity dir");
        path
    }

    #[test]
    fn loads_and_concatenates_three_files() {
        let dir = temp_identity_dir("load_concat");

        File::create(dir.join("constitution.md"))
            .expect("failed to create constitution")
            .write_all(b"A")
            .expect("failed to write constitution");
        File::create(dir.join("identity.md"))
            .expect("failed to create identity")
            .write_all(b"B")
            .expect("failed to write identity");
        File::create(dir.join("context.md"))
            .expect("failed to create context")
            .write_all(b"C")
            .expect("failed to write context");

        let prompt = load_system_prompt(dir.to_str().expect("temp path should be utf8"), &HashMap::new())
            .expect("expected prompt load to succeed");
        assert_eq!(prompt, "A\n\nB\n\nC");
    }

    #[test]
    fn applies_template_vars_to_concatenated_result() {
        let dir = temp_identity_dir("template_vars");

        File::create(dir.join("constitution.md"))
            .expect("failed to create constitution")
            .write_all(b"name: {{name}}")
            .expect("failed to write constitution");
        File::create(dir.join("identity.md"))
            .expect("failed to create identity")
            .write_all(b"cwd: {{cwd}}")
            .expect("failed to write identity");
        File::create(dir.join("context.md"))
            .expect("failed to create context")
            .write_all(b"done")
            .expect("failed to write context");

        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Ada".to_string());
        vars.insert("cwd".to_string(), "/tmp".to_string());

        let prompt = load_system_prompt(dir.to_str().expect("temp path should be utf8"), &vars)
            .expect("expected prompt load to succeed");
        assert_eq!(prompt, "name: Ada\n\ncwd: /tmp\n\ndone");
    }

    #[test]
    fn errors_when_identity_dir_missing() {
        let result = load_system_prompt("/does/not/exist", &HashMap::new());
        assert!(result.is_err());
    }

    #[test]
    fn errors_when_file_is_missing() {
        let dir = temp_identity_dir("missing_file");
        File::create(dir.join("identity.md"))
            .expect("failed to create identity")
            .write_all(b"identity")
            .expect("failed to write identity");
        File::create(dir.join("context.md"))
            .expect("failed to create context")
            .write_all(b"context")
            .expect("failed to write context");

        let result = load_system_prompt(dir.to_str().expect("temp path should be utf8"), &HashMap::new());
        assert!(result.is_err());
    }
}
