use anyhow::{Result, anyhow};

use crate::config::{ModelDefinition, ModelRoute, ModelsConfig};

/// Resolved catalog entry plus its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedModel<'a> {
    pub key: &'a str,
    pub definition: &'a ModelDefinition,
}

/// Fail-closed selector for catalog-backed T3 models.
pub struct ModelSelector<'a> {
    catalog: &'a std::collections::HashMap<String, ModelDefinition>,
    routes: &'a std::collections::HashMap<String, ModelRoute>,
    default: Option<&'a str>,
}

impl<'a> ModelSelector<'a> {
    pub fn new(models: &'a ModelsConfig) -> Self {
        Self {
            catalog: &models.catalog,
            routes: &models.routes,
            default: models.default.as_deref(),
        }
    }

    fn is_enabled(definition: &ModelDefinition) -> bool {
        definition.enabled == Some(true)
    }

    fn get_enabled(&self, key: &'a str) -> Result<SelectedModel<'a>> {
        let definition = self
            .catalog
            .get(key)
            .ok_or_else(|| anyhow!("model catalog entry not found: {key}"))?;
        if !Self::is_enabled(definition) {
            return Err(anyhow!("model catalog entry is disabled: {key}"));
        }
        Ok(SelectedModel { key, definition })
    }

    fn select_by_route(&self, task_kind: &str) -> Result<Option<SelectedModel<'a>>> {
        let mut matches = self
            .routes
            .iter()
            .filter(|(_, route)| route.requires.iter().any(|required| required == task_kind));

        let Some((route_key, route)) = matches.next() else {
            return Ok(None);
        };

        if matches.next().is_some() {
            return Err(anyhow!(
                "multiple model routes match task kind `{task_kind}`"
            ));
        }

        let mut selected = None;
        for model_key in &route.prefer {
            if let Ok(candidate) = self.get_enabled(model_key) {
                selected = Some(candidate);
                break;
            }
        }

        if selected.is_some() {
            Ok(selected)
        } else {
            Err(anyhow!(
                "no enabled preferred model found for route `{route_key}` and task kind `{task_kind}`"
            ))
        }
    }

    pub fn select_model(&self, task_kind: Option<&str>) -> Result<SelectedModel<'a>> {
        if let Some(task_kind) = task_kind
            && let Some(selected) = self.select_by_route(task_kind)?
        {
            return Ok(selected);
        }

        let Some(default_key) = self.default else {
            return Err(anyhow!("default model is missing"));
        };
        self.get_enabled(default_key)
    }
}

#[cfg(all(test, not(clippy)))]
mod tests {
    use super::*;
    use crate::config::{ModelDefinition, ModelRoute, ModelsConfig};
    use std::collections::HashMap;

    fn model(enabled: Option<bool>) -> ModelDefinition {
        ModelDefinition {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            caps: Vec::new(),
            context_window: Some(1),
            cost_tier: None,
            cost_unit: None,
            enabled,
        }
    }

    fn config() -> ModelsConfig {
        let mut catalog = HashMap::new();
        catalog.insert("default".to_string(), model(Some(true)));
        catalog.insert("fast".to_string(), model(Some(true)));
        catalog.insert("disabled".to_string(), model(Some(false)));
        catalog.insert("unset".to_string(), model(None));

        let mut routes = HashMap::new();
        routes.insert(
            "code_review".to_string(),
            ModelRoute {
                requires: vec!["code_review".to_string()],
                prefer: vec!["fast".to_string(), "disabled".to_string()],
            },
        );

        ModelsConfig {
            default: Some("default".to_string()),
            catalog,
            routes,
        }
    }

    #[test]
    fn matching_route_picks_first_enabled_preferred_model() {
        let models = config();
        let selector = ModelSelector::new(&models);
        let selected = selector.select_model(Some("code_review")).unwrap();
        assert_eq!(selected.key, "fast");
    }

    #[test]
    fn skips_disabled_or_missing_preferred_models_in_order() {
        let mut models = config();
        models.routes.get_mut("code_review").unwrap().prefer = vec![
            "missing".to_string(),
            "disabled".to_string(),
            "fast".to_string(),
        ];
        let selector = ModelSelector::new(&models);
        let selected = selector.select_model(Some("code_review")).unwrap();
        assert_eq!(selected.key, "fast");
    }

    #[test]
    fn multiple_routes_matching_same_task_kind_error_fail_closed() {
        let mut models = config();
        models.routes.insert(
            "code_review_alt".to_string(),
            ModelRoute {
                requires: vec!["code_review".to_string()],
                prefer: vec!["fast".to_string()],
            },
        );
        let selector = ModelSelector::new(&models);
        assert!(selector.select_model(Some("code_review")).is_err());
    }

    #[test]
    fn matching_route_with_no_viable_preferred_model_errors_fail_closed() {
        let mut models = config();
        models.routes.get_mut("code_review").unwrap().prefer =
            vec!["disabled".to_string(), "missing".to_string()];
        let selector = ModelSelector::new(&models);
        assert!(selector.select_model(Some("code_review")).is_err());
    }

    #[test]
    fn unknown_task_kind_uses_enabled_default() {
        let models = config();
        let selector = ModelSelector::new(&models);
        let selected = selector.select_model(Some("unknown")).unwrap();
        assert_eq!(selected.key, "default");
    }

    #[test]
    fn no_task_kind_uses_enabled_default() {
        let models = config();
        let selector = ModelSelector::new(&models);
        let selected = selector.select_model(None).unwrap();
        assert_eq!(selected.key, "default");
    }

    #[test]
    fn missing_default_errors_fail_closed() {
        let mut models = config();
        models.default = None;
        let selector = ModelSelector::new(&models);
        assert!(selector.select_model(None).is_err());
    }

    #[test]
    fn disabled_default_errors_fail_closed() {
        let mut models = config();
        models.catalog.get_mut("default").unwrap().enabled = Some(false);
        let selector = ModelSelector::new(&models);
        assert!(selector.select_model(None).is_err());
    }

    #[test]
    fn enabled_none_is_treated_as_disabled() {
        let mut models = config();
        models.catalog.get_mut("default").unwrap().enabled = None;
        let selector = ModelSelector::new(&models);
        assert!(selector.select_model(None).is_err());
    }
}
