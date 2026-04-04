use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[cfg(all(test, not(clippy)))]
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub required_caps: Vec<String>,
    pub token_estimate: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    skills: Vec<SkillDefinition>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillFile {
    skill: SkillFileDefinition,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillFileDefinition {
    name: String,
    description: String,
    instructions: String,
    required_caps: Vec<String>,
    token_estimate: u64,
}

impl SkillCatalog {
    pub fn load_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(path)
            .with_context(|| format!("failed to read skills directory {}", path.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read directory entry in {}", path.display()))?;
            let entry_path = entry.path();
            if entry_path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            if !entry_path.is_file() {
                continue;
            }
            entries.push(entry_path);
        }

        entries.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));

        let mut skills = Vec::with_capacity(entries.len());
        let mut seen_names = HashSet::new();
        for path in entries {
            let skill = Self::load_skill_file(&path)?;
            Self::push_skill(&mut skills, &mut seen_names, skill)?;
        }

        Ok(Self { skills })
    }

    fn push_skill(
        skills: &mut Vec<SkillDefinition>,
        seen_names: &mut HashSet<String>,
        skill: SkillDefinition,
    ) -> Result<()> {
        if !seen_names.insert(skill.name.clone()) {
            return Err(anyhow!("duplicate skill name: {}", skill.name));
        }

        skills.push(skill);
        Ok(())
    }

    fn load_skill_file(path: &Path) -> Result<SkillDefinition> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let parsed: SkillFile =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        let skill = parsed.skill;

        if skill.name.trim().is_empty() {
            return Err(anyhow!(
                "skill name must not be empty in {}",
                path.display()
            ));
        }

        let expected_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("skill file name is not valid UTF-8: {}", path.display()))?;

        if expected_name != skill.name {
            return Err(anyhow!(
                "skill file name mismatch: {} declares `{}`",
                path.display(),
                skill.name
            ));
        }

        Ok(SkillDefinition {
            name: skill.name,
            description: skill.description,
            instructions: skill.instructions,
            required_caps: skill.required_caps,
            token_estimate: skill.token_estimate,
        })
    }

    pub fn browse(&self) -> Vec<SkillSummary> {
        self.skills
            .iter()
            .map(|skill| SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&SkillDefinition> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    pub fn resolve_requested_skills(&self, requested: &[String]) -> Result<Vec<SkillDefinition>> {
        let mut resolved = Vec::with_capacity(requested.len());
        let mut seen = HashSet::new();

        for name in requested {
            if !seen.insert(name.clone()) {
                return Err(anyhow!("duplicate skill request: {name}"));
            }

            let skill = self
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow!("unknown skill requested: {name}"))?;
            resolved.push(skill);
        }

        Ok(resolved)
    }

    pub fn sum_token_estimates(skills: &[SkillDefinition]) -> Result<u64> {
        skills.iter().try_fold(0u64, |total, skill| {
            total
                .checked_add(skill.token_estimate)
                .ok_or_else(|| anyhow!("skill token estimate overflow"))
        })
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_skill_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autopoiesis_skill_test_{prefix}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn write_skill(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
    }

    fn catalog() -> SkillCatalog {
        let dir = temp_skill_dir("catalog");
        fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir.join("code-review.toml"),
            r#"
[skill]
name = "code-review"
description = "Reviews code changes for correctness, style, and security"
required_caps = ["code"]
token_estimate = 500
instructions = """
Review code carefully.
"""
"#,
        );
        write_skill(
            &dir.join("planning.toml"),
            r#"
[skill]
name = "planning"
description = "Produces implementation plans"
required_caps = ["reasoning"]
token_estimate = 300
instructions = """
Plan work carefully.
"""
"#,
        );

        let catalog = SkillCatalog::load_from_dir(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        catalog
    }

    #[test]
    fn load_from_dir_missing_returns_empty_catalog() {
        let dir = temp_skill_dir("missing");
        let catalog = SkillCatalog::load_from_dir(&dir).unwrap();
        assert!(catalog.is_empty());
    }

    #[test]
    fn load_from_dir_parses_skill_table() {
        let dir = temp_skill_dir("parse");
        fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir.join("code-review.toml"),
            r#"
[skill]
name = "code-review"
description = "Reviews code changes for correctness, style, and security"
required_caps = ["code", "reasoning"]
token_estimate = 500
instructions = """
Full prompt body.
"""
"#,
        );

        let catalog = SkillCatalog::load_from_dir(&dir).unwrap();
        let skill = catalog.get("code-review").unwrap();
        assert_eq!(skill.name, "code-review");
        assert_eq!(
            skill.description,
            "Reviews code changes for correctness, style, and security"
        );
        assert_eq!(
            skill.required_caps,
            vec!["code".to_string(), "reasoning".to_string()]
        );
        assert_eq!(skill.token_estimate, 500);
        assert!(skill.instructions.contains("Full prompt body."));
    }

    #[test]
    fn browse_hides_instructions() {
        let catalog = catalog();
        let summaries = catalog.browse();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "code-review");
        assert_eq!(summaries[1].name, "planning");
        assert_eq!(
            summaries[0].description,
            "Reviews code changes for correctness, style, and security"
        );
        assert_eq!(summaries[1].description, "Produces implementation plans");
    }

    #[test]
    fn filename_name_mismatch_fails() {
        let dir = temp_skill_dir("mismatch");
        fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir.join("foo.toml"),
            r#"
[skill]
name = "bar"
description = "Mismatch"
required_caps = ["code"]
token_estimate = 1
instructions = "bad"
"#,
        );

        let err = SkillCatalog::load_from_dir(&dir).expect_err("mismatch should fail");
        assert!(err.to_string().contains("skill file name mismatch"));
    }

    #[test]
    fn catalog_order_is_deterministic() {
        let catalog = catalog();
        let names: Vec<_> = catalog
            .browse()
            .into_iter()
            .map(|skill| skill.name)
            .collect();
        assert_eq!(
            names,
            vec!["code-review".to_string(), "planning".to_string()]
        );
    }

    #[test]
    fn non_toml_files_and_subdirectories_are_ignored() {
        let dir = temp_skill_dir("ignore");
        fs::create_dir_all(&dir).unwrap();
        write_skill(
            &dir.join("README.md"),
            r#"
not a skill
"#,
        );
        fs::create_dir_all(dir.join("nested")).unwrap();
        write_skill(
            &dir.join("nested").join("ignored.toml"),
            r#"
[skill]
name = "ignored"
description = "Ignored nested file"
required_caps = ["code"]
token_estimate = 1
instructions = "ignored"
"#,
        );
        write_skill(
            &dir.join("real.toml"),
            r#"
[skill]
name = "real"
description = "Real skill"
required_caps = ["code"]
token_estimate = 1
instructions = "real"
"#,
        );

        let catalog = SkillCatalog::load_from_dir(&dir).unwrap();
        assert_eq!(catalog.browse().len(), 1);
        assert!(catalog.get("real").is_some());
        assert!(catalog.get("ignored").is_none());
    }

    #[test]
    fn get_missing_skill_returns_none() {
        let catalog = catalog();
        assert!(catalog.get("missing").is_none());
    }

    #[test]
    fn duplicate_skill_names_are_rejected() {
        let mut skills = Vec::new();
        let mut seen_names = HashSet::new();
        let first = SkillDefinition {
            name: "code-review".to_string(),
            description: "First".to_string(),
            instructions: "First".to_string(),
            required_caps: vec!["code".to_string()],
            token_estimate: 1,
        };
        let duplicate = SkillDefinition {
            name: "code-review".to_string(),
            description: "Second".to_string(),
            instructions: "Second".to_string(),
            required_caps: vec!["code".to_string()],
            token_estimate: 1,
        };

        SkillCatalog::push_skill(&mut skills, &mut seen_names, first).unwrap();
        let err = SkillCatalog::push_skill(&mut skills, &mut seen_names, duplicate)
            .expect_err("duplicate names should fail");
        assert!(err.to_string().contains("duplicate skill name"));
        assert_eq!(skills.len(), 1);
    }

    #[test]
    fn resolve_requested_skills_preserves_order() {
        let catalog = catalog();
        let requested = vec!["planning".to_string(), "code-review".to_string()];
        let resolved = catalog.resolve_requested_skills(&requested).unwrap();
        assert_eq!(resolved[0].name, "planning");
        assert_eq!(resolved[1].name, "code-review");
    }

    #[test]
    fn resolve_requested_skills_rejects_unknown_name() {
        let catalog = catalog();
        let err = catalog
            .resolve_requested_skills(&["missing".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("unknown skill requested"));
    }

    #[test]
    fn resolve_requested_skills_rejects_duplicate_name() {
        let catalog = catalog();
        let err = catalog
            .resolve_requested_skills(&["planning".to_string(), "planning".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("duplicate skill request"));
    }

    #[test]
    fn sum_skill_token_estimates_matches_requested_set() {
        let catalog = catalog();
        let requested = vec!["planning".to_string(), "code-review".to_string()];
        let resolved = catalog.resolve_requested_skills(&requested).unwrap();
        assert_eq!(SkillCatalog::sum_token_estimates(&resolved).unwrap(), 800);
    }

    #[test]
    fn sum_skill_token_estimates_rejects_overflow() {
        let skills = vec![
            SkillDefinition {
                name: "overflow-a".to_string(),
                description: "a".to_string(),
                instructions: "a".to_string(),
                required_caps: vec![],
                token_estimate: u64::MAX,
            },
            SkillDefinition {
                name: "overflow-b".to_string(),
                description: "b".to_string(),
                instructions: "b".to_string(),
                required_caps: vec![],
                token_estimate: 1,
            },
        ];

        let err = SkillCatalog::sum_token_estimates(&skills).unwrap_err();
        assert!(err.to_string().contains("overflow"));
    }
}
