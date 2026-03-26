use std::collections::HashMap;
use std::path::{Component, Path};

use anyhow::{Result, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct DomainsConfig {
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(flatten, default)]
    pub entries: HashMap<String, DomainConfig>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct DomainConfig {
    pub context_extend: Option<String>,
}

pub fn validate_domain_context_extend(path: &str) -> Result<()> {
    let mut components = Path::new(path).components();
    match components.next() {
        Some(Component::Normal(root)) if root == "identity-templates" => {}
        _ => {
            return Err(anyhow!(
                "domain context_extend must stay under identity-templates/"
            ));
        }
    }

    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(anyhow!(
            "domain context_extend must stay under identity-templates/"
        ));
    }

    Ok(())
}
