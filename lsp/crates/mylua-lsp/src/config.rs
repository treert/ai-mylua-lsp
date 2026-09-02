use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LspConfig {
    pub runtime: RuntimeConfig,
    pub require: RequireConfig,
    pub workspace: WorkspaceConfig,
    pub performance: PerformanceConfig,
    #[serde(rename = "tableShape")]
    pub table_shape: TableShapeConfig,
    pub diagnostics: DiagnosticsConfig,
    #[serde(rename = "documentSymbol")]
    pub document_symbol: DocumentSymbolConfig,
    #[serde(rename = "gotoDefinition")]
    pub goto_definition: GotoDefinitionConfig,
    pub references: ReferencesConfig,
    #[serde(rename = "inlayHint")]
    pub inlay_hint: InlayHintConfig,
    pub debug: DebugConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DocumentSymbolConfig {
    #[serde(rename = "detailLevel")]
    pub detail_level: DocumentSymbolDetailLevel,
}

impl Default for DocumentSymbolConfig {
    fn default() -> Self {
        Self {
            detail_level: DocumentSymbolDetailLevel::Compact,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[derive(Default)]
pub enum DocumentSymbolDetailLevel {
    #[default]
    Compact,
    Functions,
    AllDeclarations,
    AnonymousFunctions,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DebugConfig {
    #[serde(rename = "fileLog")]
    pub file_log: bool,
}

impl Default for DebugConfig {
    fn default() -> Self {
        // Matches `mylua.debug.fileLog` in the extension manifest. Writing a
        // log file into the user's workspace is not something a client that
        // sent no configuration should get by surprise.
        Self { file_log: false }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PerformanceConfig {
    #[serde(rename = "slowParseKeepTreeThresholdMs")]
    pub slow_parse_keep_tree_threshold_ms: u128,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            slow_parse_keep_tree_threshold_ms: 500,
        }
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
            // Matches `mylua.runtime.version` in the extension manifest, which
            // is the single source of truth for defaults
            // (`docs/vscode-extension.md`). 5.4 is also the only bundled stub
            // tree, so a client that sends no config still gets stdlib
            // annotations that match the assumed runtime.
            version: "5.4".to_string(),
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
    /// Path segments (case-insensitive) that boost a URI's definition
    /// candidate priority. When multiple files define the same symbol,
    /// files whose path contains any of these segments win over others.
    /// Useful for marking annotation/stub directories.
    ///
    /// Defaults to `["annotation"]`. Changes require a server restart to
    /// apply to already-interned URIs (priority is computed once at
    /// intern time and cached).
    #[serde(rename = "priorityKeyword")]
    pub priority_keyword: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.lua".to_string()],
            exclude: vec!["**/.*".to_string(), "**/node_modules".to_string()],
            library: Vec::new(),
            priority_keyword: vec!["annotation".to_string()],
        }
    }
}

/// How static string keys (`{ ["k"] = v }`, `t["k"] = v`) contribute to
/// `TableShape`.
///
/// Both switches are read through `table_shape::classify_string_key`, the
/// single decision point shared by the constructor and assignment paths.
/// They are pinned at `initialize`: summaries built under one policy stay
/// resident, so a mid-session change would leave the index half-converted.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TableShapeConfig {
    /// Whether a static string key becomes a named field at all.
    ///
    /// With this off, `t.foo` after `t["foo"] = 1` is unresolvable — so the
    /// shape is marked non-exhaustive instead of reporting a bogus
    /// "Unknown field". Turning it off buys a style push (forcing `.Name`)
    /// at the cost of navigation, which is why the default is on.
    #[serde(rename = "stringKeys")]
    pub string_keys: bool,
    /// Whether the key text must match Lua's `Name` production
    /// (`[A-Za-z_][A-Za-z0-9_]*`) to be recorded.
    ///
    /// On (default): only keys that also have a dot spelling become fields,
    /// so the shape's field set stays exactly "what `t.x` can reach".
    /// Off: `t["a-b"]`, `t["1"]` and non-ASCII keys become fields too —
    /// reachable only through the bracket-read path.
    #[serde(rename = "stringKeysRequireIdentifier")]
    pub string_keys_require_identifier: bool,
}

impl Default for TableShapeConfig {
    fn default() -> Self {
        Self {
            string_keys: true,
            string_keys_require_identifier: true,
        }
    }
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
    /// Reading a field the *redirected* `_ENV` does not have (yet).
    ///
    /// Only fires when `_ENV` points at a table whose shape is fully known,
    /// and only for reads on the top-level straight-line flow of the chunk —
    /// see `diagnostics::env_field`. Deliberately separate from
    /// `undefinedGlobal`: inside a sandbox the name is a table field, not a
    /// global, and users running heavily sandboxed code may want to silence
    /// this category on its own.
    #[serde(rename = "envUnknownField")]
    pub env_unknown_field: DiagnosticSeverityOption,
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
    ///
    /// **Hint by default.** Omitting trailing arguments is idiomatic Lua —
    /// they arrive as `nil` — so without `---@param` annotations marking
    /// which parameters are optional this fires on a lot of correct code.
    /// `Hint` keeps it out of the Problems panel's error/warning counts and
    /// off the scrollbar while still underlining the call, so a real
    /// arity bug stays visible without drowning an unannotated codebase.
    /// Contrast `argument_type_mismatch`, which is a full `Warning`: it only
    /// fires when both sides have a known concrete type, so it has evidence
    /// of an actual conflict rather than a missing annotation.
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
    ///
    /// **Hint by default**, for the same reason as
    /// `argument_count_mismatch`: a bare `return` used as an early exit, and
    /// branches that legitimately return different arities, are ordinary Lua
    /// that a static count comparison cannot distinguish from a real
    /// mismatch.
    #[serde(rename = "returnMismatch")]
    pub return_mismatch: DiagnosticSeverityOption,
    /// Suppress `undefinedGlobal` / unknown-field reports at reads the
    /// author already guarded with an existence check, e.g.
    /// `if X then … X … end` for a symbol the host application registers
    /// at run time.
    ///
    /// A bool rather than a `DiagnosticSeverityOption`: this does not
    /// produce diagnostics of its own, it only filters others. It covers
    /// both affected codes with one switch because they share the same
    /// mechanism, and it changes no types — see `diagnostics::condition_guard`.
    #[serde(rename = "narrowByConditionGuard")]
    pub narrow_by_condition_guard: bool,
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
            env_unknown_field: DiagnosticSeverityOption::Warning,
            duplicate_table_key: DiagnosticSeverityOption::Warning,
            unused_local: DiagnosticSeverityOption::Hint,
            argument_count_mismatch: DiagnosticSeverityOption::Hint,
            argument_type_mismatch: DiagnosticSeverityOption::Warning,
            return_mismatch: DiagnosticSeverityOption::Hint,
            narrow_by_condition_guard: true,
            scope: DiagnosticScope::Full,
        }
    }
}

/// Scope of diagnostics publishing.
///
/// - `Full` (default): cold-start seeds the entire workspace (already
///   open → Hot queue, others → Cold); cascade 入队全部已索引文件。
/// - `OpenOnly`: cold-start seeds only `open_uris` as Hot; cascade 只入队
///   已打开文件. Matches the default behavior of most LSPs
///   (rust-analyzer, pyright).
///
/// 两种模式都**不做依赖筛选**——没有反向依赖图，"dependant" 一词不适用。
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
        // Mirrors `mylua.inlayHint.*` in the extension manifest, which is the
        // single source of truth for defaults (`docs/vscode-extension.md`).
        // Clients can still turn each category on/off via
        // `initializationOptions.inlayHint.*`.
        Self {
            enable: true,
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
    /// Whether to scan plain (non-`---@`) comments for occurrences of a
    /// registered Emmy type name when collecting references.
    ///
    /// When `true` (default), a type name mentioned in an ordinary
    /// `-- ...` comment is reported as a reference — matching the
    /// historical behavior. Set to `false` to only match type names
    /// inside `---@` annotation lines, reducing false positives from
    /// prose comments that merely mention a type in passing.
    #[serde(rename = "scanComments")]
    pub scan_comments: bool,
}

impl Default for ReferencesConfig {
    fn default() -> Self {
        Self {
            strategy: ReferencesStrategy::Best,
            scan_comments: true,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn performance_config_defaults_slow_parse_keep_tree_threshold_to_500_ms() {
        let cfg = LspConfig::default();

        assert_eq!(cfg.performance.slow_parse_keep_tree_threshold_ms, 500);
    }

    #[test]
    fn performance_config_reads_slow_parse_keep_tree_threshold_from_json() {
        let cfg = LspConfig::from_value(serde_json::json!({
            "performance": {
                "slowParseKeepTreeThresholdMs": 42
            }
        }));

        assert_eq!(cfg.performance.slow_parse_keep_tree_threshold_ms, 42);
    }

    #[test]
    fn table_shape_string_keys_default_to_identifier_only() {
        let cfg = LspConfig::default();

        assert!(cfg.table_shape.string_keys);
        assert!(cfg.table_shape.string_keys_require_identifier);
    }

    #[test]
    fn table_shape_switches_read_from_json_independently() {
        let cfg = LspConfig::from_value(serde_json::json!({
            "tableShape": {
                "stringKeysRequireIdentifier": false
            }
        }));

        // Omitted keys keep their default, so relaxing the identifier
        // requirement alone does not silently disable string keys.
        assert!(cfg.table_shape.string_keys);
        assert!(!cfg.table_shape.string_keys_require_identifier);
    }

    #[test]
    fn document_symbol_detail_level_defaults_to_compact() {
        let cfg = LspConfig::default();

        assert_eq!(
            cfg.document_symbol.detail_level,
            DocumentSymbolDetailLevel::Compact
        );
    }

    #[test]
    fn inlay_hint_defaults_enable_both_categories() {
        // Kept in step with `mylua.inlayHint.*` in the extension manifest;
        // `tests/test_config_defaults.rs` enforces that agreement wholesale.
        let cfg = LspConfig::default();

        assert!(cfg.inlay_hint.enable);
        assert!(cfg.inlay_hint.parameter_names);
        assert!(cfg.inlay_hint.variable_types);
    }

    #[test]

    fn document_symbol_detail_level_reads_all_declarations_from_json() {
        let cfg = LspConfig::from_value(serde_json::json!({
            "documentSymbol": {
                "detailLevel": "allDeclarations"
            }
        }));

        assert_eq!(
            cfg.document_symbol.detail_level,
            DocumentSymbolDetailLevel::AllDeclarations
        );
    }

    #[test]
    fn document_symbol_detail_level_reads_anonymous_functions_from_json() {
        let level: DocumentSymbolDetailLevel =
            serde_json::from_value(serde_json::json!("anonymousFunctions")).unwrap();

        assert_eq!(level, DocumentSymbolDetailLevel::AnonymousFunctions);
    }
}
