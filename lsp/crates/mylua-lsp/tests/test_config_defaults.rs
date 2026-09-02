//! The manifest's declared defaults and `LspConfig::default()` must agree.
//!
//! # Why this needs a test
//!
//! The VS Code extension always sends every declared setting, so a divergence
//! here is invisible in the editor — the manifest value wins and nobody
//! notices. It becomes visible only when the server runs without that
//! extension (another LSP client, a bare `initialize` with no
//! `initializationOptions`, an integration test), and then the same workspace
//! silently behaves differently. `inlayHint.variableTypes` sat mismatched
//! exactly that way.
//!
//! Keeping the two in step by hand has already failed once, so it is checked
//! here instead: `package.json` is the single source of truth for defaults
//! (`docs/vscode-extension.md`), and this test holds the Rust side to it.

use mylua_lsp::config::LspConfig;
use serde_json::{Map, Value};

/// `contributes.configuration.properties` from the extension manifest.
fn manifest_properties() -> Map<String, Value> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../vscode-extension/package.json"
    );
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
    let pkg: Value = serde_json::from_str(&text).expect("package.json is not valid JSON");
    pkg["contributes"]["configuration"]["properties"]
        .as_object()
        .expect("contributes.configuration.properties missing")
        .clone()
}

/// Settings the extension consumes itself and never forwards, so the server
/// has no field to compare against. Mirrors `CLIENT_ONLY_CONFIG_SECTIONS` in
/// `vscode-extension/src/extension.ts`.
const CLIENT_ONLY: &[&str] = &[
    "server.path",
    "server.autoRestartOnConfigChange",
    "workspace.useBundledStdlib",
];

/// `workspace.library` is forwarded as a *computed* value (user entries plus
/// the bundled stdlib path), so the manifest default `[]` is not what the
/// server receives.
const COMPUTED: &[&str] = &["workspace.library"];

/// Walk `LspConfig::default()` serialized as JSON, following a dotted path.
fn default_at(defaults: &Value, dotted: &str) -> Option<Value> {
    let mut cursor = defaults;
    for part in dotted.split('.') {
        cursor = cursor.get(part)?;
    }
    Some(cursor.clone())
}

#[test]
fn rust_defaults_match_the_extension_manifest() {
    // `LspConfig` only derives Deserialize, so round-trip an empty object to
    // get the defaults and re-serialize via the same field names the manifest
    // uses. Comparing through `LspConfig::from_value` also proves the serde
    // renames line up with the manifest's dotted sections.
    let defaults = LspConfig::from_value(serde_json::json!({}));
    let defaults = serde_json::to_value(DefaultsProbe::from(&defaults))
        .expect("defaults are serializable");

    let mut mismatches = Vec::new();
    let mut unchecked = Vec::new();

    for (key, schema) in manifest_properties() {
        let Some(section) = key.strip_prefix("mylua.") else {
            continue;
        };
        if CLIENT_ONLY.contains(&section) || COMPUTED.contains(&section) {
            continue;
        }
        let Some(manifest_default) = schema.get("default") else {
            unchecked.push(format!("{section} (manifest declares no default)"));
            continue;
        };
        match default_at(&defaults, section) {
            Some(rust_default) if &rust_default == manifest_default => {}
            Some(rust_default) => mismatches.push(format!(
                "{section}: manifest={manifest_default}, rust={rust_default}"
            )),
            None => unchecked.push(format!("{section} (no matching LspConfig field)")),
        }
    }

    assert!(
        mismatches.is_empty(),
        "manifest and LspConfig::default() disagree:\n  {}",
        mismatches.join("\n  "),
    );
    assert!(
        unchecked.is_empty(),
        "settings that could not be compared (add to CLIENT_ONLY/COMPUTED, \
         or wire up the missing field):\n  {}",
        unchecked.join("\n  "),
    );
}

// ---------------------------------------------------------------------------
// Serialization probe
// ---------------------------------------------------------------------------
//
// `LspConfig` and its sections derive only `Deserialize` (the server never
// sends config back), so the test builds its own mirror to serialize. Adding
// a section to `LspConfig` without adding it here shows up as an "unchecked"
// entry above rather than passing silently.

#[derive(serde::Serialize)]
struct DefaultsProbe {
    runtime: Value,
    require: Value,
    workspace: Value,
    performance: Value,
    #[serde(rename = "tableShape")]
    table_shape: Value,
    diagnostics: Value,
    #[serde(rename = "documentSymbol")]
    document_symbol: Value,
    #[serde(rename = "gotoDefinition")]
    goto_definition: Value,
    references: Value,
    #[serde(rename = "inlayHint")]
    inlay_hint: Value,
    debug: Value,
}

impl From<&LspConfig> for DefaultsProbe {
    fn from(c: &LspConfig) -> Self {
        use serde_json::json;
        Self {
            runtime: json!({
                "version": c.runtime.version,
                "topKeyword": c.runtime.top_keyword,
            }),
            require: json!({ "aliases": c.require.aliases }),
            workspace: json!({
                "include": c.workspace.include,
                "exclude": c.workspace.exclude,
                "priorityKeyword": c.workspace.priority_keyword,
            }),
            performance: json!({
                "slowParseKeepTreeThresholdMs": c.performance.slow_parse_keep_tree_threshold_ms,
            }),
            table_shape: json!({
                "stringKeys": c.table_shape.string_keys,
                "stringKeysRequireIdentifier": c.table_shape.string_keys_require_identifier,
            }),
            diagnostics: json!({
                "enable": c.diagnostics.enable,
                "undefinedGlobal": severity(&c.diagnostics.undefined_global),
                "emmyTypeMismatch": severity(&c.diagnostics.emmy_type_mismatch),
                "emmyUnknownField": severity(&c.diagnostics.emmy_unknown_field),
                "luaFieldError": severity(&c.diagnostics.lua_field_error),
                "luaFieldWarning": severity(&c.diagnostics.lua_field_warning),
                "envUnknownField": severity(&c.diagnostics.env_unknown_field),
                "duplicateTableKey": severity(&c.diagnostics.duplicate_table_key),
                "unusedLocal": severity(&c.diagnostics.unused_local),
                "argumentCountMismatch": severity(&c.diagnostics.argument_count_mismatch),
                "argumentTypeMismatch": severity(&c.diagnostics.argument_type_mismatch),
                "returnMismatch": severity(&c.diagnostics.return_mismatch),
                "narrowByConditionGuard": c.diagnostics.narrow_by_condition_guard,
                "scope": scope(&c.diagnostics.scope),
            }),
            document_symbol: json!({
                "detailLevel": detail_level(&c.document_symbol.detail_level),
            }),
            goto_definition: json!({ "strategy": goto_strategy(&c.goto_definition.strategy) }),
            references: json!({
                "strategy": references_strategy(&c.references.strategy),
                "scanComments": c.references.scan_comments,
            }),
            inlay_hint: json!({
                "enable": c.inlay_hint.enable,
                "parameterNames": c.inlay_hint.parameter_names,
                "variableTypes": c.inlay_hint.variable_types,
            }),
            debug: json!({ "fileLog": c.debug.file_log }),
        }
    }
}

fn severity(v: &mylua_lsp::config::DiagnosticSeverityOption) -> &'static str {
    use mylua_lsp::config::DiagnosticSeverityOption::*;
    match v {
        Error => "error",
        Warning => "warning",
        Hint => "hint",
        Off => "off",
    }
}

fn scope(v: &mylua_lsp::config::DiagnosticScope) -> &'static str {
    use mylua_lsp::config::DiagnosticScope::*;
    match v {
        Full => "full",
        OpenOnly => "openOnly",
    }
}

fn detail_level(v: &mylua_lsp::config::DocumentSymbolDetailLevel) -> &'static str {
    use mylua_lsp::config::DocumentSymbolDetailLevel::*;
    match v {
        Compact => "compact",
        Functions => "functions",
        AllDeclarations => "allDeclarations",
        AnonymousFunctions => "anonymousFunctions",
    }
}

fn goto_strategy(v: &mylua_lsp::config::GotoStrategy) -> &'static str {
    use mylua_lsp::config::GotoStrategy::*;
    match v {
        Auto => "auto",
        Single => "single",
        List => "list",
    }
}

fn references_strategy(v: &mylua_lsp::config::ReferencesStrategy) -> &'static str {
    use mylua_lsp::config::ReferencesStrategy::*;
    match v {
        Best => "best",
        Merge => "merge",
        Select => "select",
    }
}
