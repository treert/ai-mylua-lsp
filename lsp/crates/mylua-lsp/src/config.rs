use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LspConfig {
    pub runtime: RuntimeConfig,
    pub require: RequireConfig,
    pub workspace: WorkspaceConfig,
    pub diagnostics: DiagnosticsConfig,
    #[serde(rename = "gotoDefinition")]
    pub goto_definition: GotoDefinitionConfig,
    pub references: ReferencesConfig,
    #[serde(rename = "inlayHint")]
    pub inlay_hint: InlayHintConfig,
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DebugConfig {
    #[serde(rename = "fileLog")]
    pub file_log: bool,
}

impl Default for DebugConfig {
    fn default() -> Self {
        Self { file_log: true }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub version: String,
    /// Global toggle for top-level keyword splitting in the
    /// tree-sitter scanner. When `true`, column-0 keywords emit
    /// `TOP_WORD_*` tokens that force block closure — useful for
    /// error front-loading. When `false` (default), all keywords
    /// emit normal `WORD_*` regardless of column.
    ///
    /// Individual files can still override via `---#enable top_keyword`
    /// / `---#disable top_keyword` directives.
    #[serde(rename = "topKeyword")]
    pub top_keyword: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            version: "5.3".to_string(),
            top_keyword: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RequireConfig {
    /// Path aliases for require resolution, e.g. `{"@": "src"}`.
    pub aliases: HashMap<String, String>,
}

impl Default for RequireConfig {
    fn default() -> Self {
        Self {
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
    /// Additional directories to index alongside the user's workspace
    /// roots. Intended for Lua stdlib stubs (bundled with the VS Code
    /// extension) and optional third-party annotation packages.
    ///
    /// Each entry may be:
    /// - Absolute path — used as-is;
    /// - `~/…` — expanded against `$HOME` / `%USERPROFILE%`;
    /// - Relative — resolved against the first workspace root.
    ///
    /// Files reached via these roots are force-flagged
    /// `DocumentSummary.is_meta = true` (so `undefinedGlobal` stays
    /// quiet even though the stubs reference runtime-provided
    /// symbols), and the diagnostic consumer publishes an empty
    /// diagnostic set for them so they never pollute the client's
    /// Problems panel.
    ///
    /// Duplicates with the user's own workspace roots are harmless:
    /// `resolve_library_roots` canonicalizes and deduplicates; when a
    /// path appears in both, the scan walks it once and library
    /// semantics (is_meta / empty diagnostics) take precedence only
    /// for URIs that originated from the library walk.
    pub library: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.lua".to_string()],
            exclude: vec!["**/.*".to_string(), "**/node_modules".to_string()],
            index_mode: IndexMode::Merged,
            library: Vec::new(),
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
    /// P2-3: report duplicate keys in a single `{ ... }` table
    /// constructor, e.g. `{ a = 1, a = 2 }`.
    #[serde(rename = "duplicateTableKey")]
    pub duplicate_table_key: DiagnosticSeverityOption,
    /// P2-3: report locals that are declared but never read. `_` /
    /// `_prefix` names are skipped by the diagnostic implementation.
    #[serde(rename = "unusedLocal")]
    pub unused_local: DiagnosticSeverityOption,
    /// P2-3 continued: call-site arg count vs FunctionSummary params
    /// mismatch. Respects vararg (`...` absorbs extras) and overloads
    /// (any overload matching clears the diagnostic).
    #[serde(rename = "argumentCountMismatch")]
    pub argument_count_mismatch: DiagnosticSeverityOption,
    /// P2-3 continued: call-site arg type vs `@param` declared type
    /// mismatch. Only fires when both sides have a known Known
    /// KnownType (literals, resolved locals); `Unknown` is skipped.
    #[serde(rename = "argumentTypeMismatch")]
    pub argument_type_mismatch: DiagnosticSeverityOption,
    /// P2-3 continued: `---@return` count/type mismatch vs actual
    /// `return` statements in the function body. Walks all nested
    /// `return` statements (including inside `if`/`do`/`while`).
    #[serde(rename = "returnMismatch")]
    pub return_mismatch: DiagnosticSeverityOption,
    /// Scope of cold-start diagnostics publishing + cascade fan-out.
    /// Default `"full"`. See `DiagnosticScope` for semantics.
    pub scope: DiagnosticScope,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            enable: true,
            undefined_global: DiagnosticSeverityOption::Warning,
            emmy_type_mismatch: DiagnosticSeverityOption::Warning,
            emmy_unknown_field: DiagnosticSeverityOption::Warning,
            lua_field_error: DiagnosticSeverityOption::Warning,
            lua_field_warning: DiagnosticSeverityOption::Warning,
            duplicate_table_key: DiagnosticSeverityOption::Warning,
            unused_local: DiagnosticSeverityOption::Hint,
            argument_count_mismatch: DiagnosticSeverityOption::Off,
            argument_type_mismatch: DiagnosticSeverityOption::Warning,
            return_mismatch: DiagnosticSeverityOption::Off,
            scope: DiagnosticScope::Full,
        }
    }
}

/// Scope of diagnostics publishing.
///
/// - `Full` (default): cold-start seeds the entire workspace (already
///   open → Hot queue, others → Cold); cascade 触发所有 dependant URIs.
/// - `OpenOnly`: cold-start seeds only `open_uris` as Hot; cascade 跳过
///   未打开的 dependant URIs. Matches the default behavior of most LSPs
///   (rust-analyzer, pyright).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum DiagnosticScope {
    #[default]
    Full,
    OpenOnly,
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

/// Inlay hint options.
///
/// - `enable` master switch
/// - `parameter_names`: show `name:` before each non-variadic argument
///   at function call sites where we have a FunctionSummary
/// - `variable_types`: show `: Type` after `local x = ...` names when
///   a useful inferred type is available
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InlayHintConfig {
    pub enable: bool,
    #[serde(rename = "parameterNames")]
    pub parameter_names: bool,
    #[serde(rename = "variableTypes")]
    pub variable_types: bool,
}

impl Default for InlayHintConfig {
    fn default() -> Self {
        Self {
            // All three default off — inlay hints can feel cluttered
            // for users who don't expect them. Clients that want
            // them send `initializationOptions.inlayHint.enable =
            // true` to opt in.
            enable: false,
            parameter_names: true,
            variable_types: true,
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
