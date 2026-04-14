use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LspConfig {
    pub runtime: RuntimeConfig,
    pub require: RequireConfig,
    pub workspace: WorkspaceConfig,
    pub index: IndexConfig,
    pub diagnostics: DiagnosticsConfig,
    #[serde(rename = "gotoDefinition")]
    pub goto_definition: GotoDefinitionConfig,
    pub references: ReferencesConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub version: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            version: "5.3".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RequireConfig {
    pub paths: Vec<String>,
    pub aliases: HashMap<String, String>,
}

impl Default for RequireConfig {
    fn default() -> Self {
        Self {
            paths: vec!["?.lua".to_string(), "?/init.lua".to_string()],
            aliases: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    #[serde(rename = "indexMode")]
    pub index_mode: IndexMode,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.lua".to_string()],
            exclude: vec!["**/.*".to_string(), "**/node_modules".to_string()],
            index_mode: IndexMode::Merged,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexMode {
    Merged,
    Isolated,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IndexConfig {
    #[serde(rename = "cacheMode")]
    pub cache_mode: CacheMode,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            cache_mode: CacheMode::Summary,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    Summary,
    Memory,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub enable: bool,
    #[serde(rename = "undefinedGlobal")]
    pub undefined_global: DiagnosticSeverityOption,
    #[serde(rename = "emmyTypeMismatch")]
    pub emmy_type_mismatch: DiagnosticSeverityOption,
    #[serde(rename = "emmyUnknownField")]
    pub emmy_unknown_field: DiagnosticSeverityOption,
    #[serde(rename = "luaFieldError")]
    pub lua_field_error: DiagnosticSeverityOption,
    #[serde(rename = "luaFieldWarning")]
    pub lua_field_warning: DiagnosticSeverityOption,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            enable: true,
            undefined_global: DiagnosticSeverityOption::Warning,
            emmy_type_mismatch: DiagnosticSeverityOption::Error,
            emmy_unknown_field: DiagnosticSeverityOption::Error,
            lua_field_error: DiagnosticSeverityOption::Error,
            lua_field_warning: DiagnosticSeverityOption::Warning,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverityOption {
    Error,
    Warning,
    Hint,
    Off,
}

impl DiagnosticSeverityOption {
    pub fn to_lsp_severity(&self) -> Option<tower_lsp_server::ls_types::DiagnosticSeverity> {
        use tower_lsp_server::ls_types::DiagnosticSeverity;
        match self {
            Self::Error => Some(DiagnosticSeverity::ERROR),
            Self::Warning => Some(DiagnosticSeverity::WARNING),
            Self::Hint => Some(DiagnosticSeverity::HINT),
            Self::Off => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GotoDefinitionConfig {
    pub strategy: GotoStrategy,
}

impl Default for GotoDefinitionConfig {
    fn default() -> Self {
        Self {
            strategy: GotoStrategy::Auto,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GotoStrategy {
    Auto,
    Single,
    List,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReferencesConfig {
    pub strategy: ReferencesStrategy,
}

impl Default for ReferencesConfig {
    fn default() -> Self {
        Self {
            strategy: ReferencesStrategy::Best,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReferencesStrategy {
    Best,
    Merge,
    Select,
}

impl LspConfig {
    pub fn from_value(value: serde_json::Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }
}
