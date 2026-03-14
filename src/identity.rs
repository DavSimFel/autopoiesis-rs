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
