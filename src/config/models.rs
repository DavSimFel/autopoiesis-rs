use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ModelsConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub catalog: HashMap<String, ModelDefinition>,
    #[serde(default)]
    pub routes: HashMap<String, ModelRoute>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ModelDefinition {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub caps: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub cost_tier: Option<String>,
    #[serde(default)]
    pub cost_unit: Option<u64>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct ModelRoute {
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub prefer: Vec<String>,
}
