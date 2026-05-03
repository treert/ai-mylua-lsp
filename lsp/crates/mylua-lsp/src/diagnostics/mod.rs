mod call_args;
mod duplicate_key;
mod field_access;
mod param_annotation;
mod return_mismatch;
mod suppression;
mod syntax;
pub(crate) mod type_compat;
mod type_mismatch;
mod undefined_global;
mod unused_local;

use crate::aggregation::WorkspaceAggregation;
use crate::config::DiagnosticsConfig;
use crate::scope::ScopeTree;
use crate::uri_id::{intern as intern_uri, UriId};
use crate::util::LineIndex;
use std::collections::HashSet;
use tower_lsp_server::ls_types::*;

pub use suppression::{apply_diagnostic_suppressions, classify_diagnostic_code};

// Built-in identifier set is now version-dependent and lives in
// `lua_builtins::builtins_for(version)`. Diagnostic paths pull the
// set through `collect_semantic_diagnostics`'s config parameter.

pub fn collect_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut cursor = root.walk();
    syntax::collect_errors_recursive(&mut cursor, source, &mut diagnostics, line_index);
    diagnostics
}

pub fn collect_semantic_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    let uri_id = intern_uri(uri.clone());
    collect_semantic_diagnostics_with_version_id(
        root,
        source,
        uri,
        uri_id,
        index,
        scope_tree,
        diag_config,
        "5.3",
        line_index,
    )
}

/// Version-aware variant — `runtime_version` (e.g. `"5.3"` / `"5.4"`
/// / `"luajit"`) selects which built-in identifiers are considered
/// defined so that `undefinedGlobal` and related checks stay
/// accurate per runtime.
pub fn collect_semantic_diagnostics_with_version(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
    runtime_version: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    let uri_id = intern_uri(uri.clone());
    collect_semantic_diagnostics_with_version_id(
        root,
        source,
        uri,
        uri_id,
        index,
        scope_tree,
        diag_config,
        runtime_version,
        line_index,
    )
}

pub(crate) fn collect_semantic_diagnostics_with_version_id(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    uri_id: UriId,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
    runtime_version: &str,
    line_index: &LineIndex,
) -> Vec<Diagnostic> {
    if !diag_config.enable {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    let builtins: HashSet<&str> = crate::lua_builtins::builtins_for(runtime_version)
        .into_iter()
        .collect();

    let mut cursor = root.walk();
    // `---@meta` files declare stubs for runtime-provided APIs, so
    // many of the identifiers they reference are intentionally not
    // declared in the workspace. Skip `undefinedGlobal` there to
    // avoid a wall of noise on a legitimate stub file.
    let is_meta = index.summary_by_id(uri_id).map(|s| s.is_meta).unwrap_or(false);
    if let Some(severity) = diag_config.undefined_global.to_lsp_severity() {
        if !is_meta {
            undefined_global::check_undefined_globals(
                &mut cursor,
                source,
                &builtins,
                index,
                scope_tree,
                &mut diagnostics,
                severity,
                line_index,
            );
        }
    }
    let emmy_severity = diag_config.emmy_unknown_field.to_lsp_severity();
    let lua_error_severity = diag_config.lua_field_error.to_lsp_severity();
    let lua_warn_severity = diag_config.lua_field_warning.to_lsp_severity();
    if emmy_severity.is_some() || lua_error_severity.is_some() || lua_warn_severity.is_some() {
        field_access::check_field_access_diagnostics(
            root,
            source,
            uri,
            uri_id,
            index,
            scope_tree,
            &mut diagnostics,
            emmy_severity,
            lua_error_severity,
            lua_warn_severity,
            line_index,
        );
    }
    if let Some(severity) = diag_config.emmy_type_mismatch.to_lsp_severity() {
        type_mismatch::check_type_mismatch_diagnostics(
            root,
            source,
            scope_tree,
            &mut diagnostics,
            severity,
            line_index,
        );
    }
    if let Some(severity) = diag_config.duplicate_table_key.to_lsp_severity() {
        duplicate_key::check_duplicate_table_keys(
            root,
            source,
            &mut diagnostics,
            severity,
            line_index,
        );
    }
    if let Some(severity) = diag_config.unused_local.to_lsp_severity() {
        unused_local::check_unused_locals(root, source, scope_tree, &mut diagnostics, severity);
    }
    let count_sev = diag_config.argument_count_mismatch.to_lsp_severity();
    let type_sev = diag_config.argument_type_mismatch.to_lsp_severity();
    if count_sev.is_some() || type_sev.is_some() {
        call_args::check_call_argument_diagnostics(
            root,
            source,
            uri,
            index,
            scope_tree,
            &mut diagnostics,
            count_sev,
            type_sev,
            line_index,
        );
    }
    if let Some(severity) = diag_config.return_mismatch.to_lsp_severity() {
        return_mismatch::check_return_mismatch_diagnostics(
            root,
            source,
            &mut diagnostics,
            severity,
            line_index,
        );
    }
    param_annotation::check_param_annotation_diagnostics(
        root,
        source,
        &mut diagnostics,
        line_index,
    );
    diagnostics
}
