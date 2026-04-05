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
        Some(Component::Normal(root)) if root == "src" => {}
        _ => {
            return Err(anyhow!(
                "domain context_extend must stay under src/shipped/identity-templates/"
            ));
        }
    }

    match components.next() {
        Some(Component::Normal(root)) if root == "shipped" => {}
        _ => {
            return Err(anyhow!(
                "domain context_extend must stay under src/shipped/identity-templates/"
            ));
        }
    }

    match components.next() {
        Some(Component::Normal(root)) if root == "identity-templates" => {}
        _ => {
            return Err(anyhow!(
                "domain context_extend must stay under src/shipped/identity-templates/"
            ));
        }
    }

    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(anyhow!(
            "domain context_extend must stay under src/shipped/identity-templates/"
        ));
    }

    Ok(())
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::validate_domain_context_extend;

    #[test]
    fn rejects_absolute_traversal_and_wrong_root_paths() {
        assert!(validate_domain_context_extend("/src/shipped/identity-templates/demo.md").is_err());
        assert!(
            validate_domain_context_extend("../src/shipped/identity-templates/demo.md").is_err()
        );
        assert!(validate_domain_context_extend("skills/demo.md").is_err());
    }

    #[test]
    fn rejects_non_normal_components_after_identity_templates_root() {
        assert!(
            validate_domain_context_extend("src/shipped/identity-templates/dir/../demo.md")
                .is_err()
        );
        assert!(
            validate_domain_context_extend("src/shipped/identity-templates/../demo.md").is_err()
        );
    }

    #[test]
    fn accepts_normal_identity_templates_paths() {
        assert!(
            validate_domain_context_extend("src/shipped/identity-templates/domains/demo.md")
                .is_ok()
        );
    }
}
