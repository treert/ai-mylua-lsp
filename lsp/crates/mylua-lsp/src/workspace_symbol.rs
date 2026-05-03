//! `workspace/symbol` — fuzzy search across the whole workspace.
//!
//! Surfaces:
//!
//! - Global functions / variables from `global_shard`. Entries whose
//!   name is `Class:method` or `Class.method` are exploded: the
//!   displayed name becomes just the member, `container_name` is set
//!   to `Class`, and the kind is METHOD (for `:`) or FUNCTION (for `.`).
//! - Emmy types from `type_shard` → CLASS / INTERFACE / ENUM.
//! - **Class fields** walked from every file's `DocumentSummary.type_definitions`
//!   → FIELD with `container_name = ClassName` (P1-5). Lets a user
//!   search e.g. `ba` and find `Foo.bar` and `Bar.bar` as two separate
//!   entries with different `container_name`.

use std::collections::HashSet;

use tower_lsp_server::ls_types::*;

use crate::aggregation::WorkspaceAggregation;
use crate::summary::{GlobalContributionKind, TypeDefinitionKind};
use crate::type_system::{KnownType, TypeFact};
use crate::uri_id::resolve as resolve_uri;

pub fn search_workspace_symbols(
    query: &str,
    index: &WorkspaceAggregation,
) -> Vec<SymbolInformation> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    // Dedup key for FIELD / METHOD / dot-accessor entries. A member
    // may reach workspace/symbol via two separate indices:
    //   - `@field bar integer` (via `type_definitions.fields`)
    //   - `Foo.bar = 1` or `function Foo.bar() end` (via `global_shard`)
    // Both are legitimate locations, but the IDE only needs one
    // entry per (name, container, uri) to avoid noise.
    let mut member_keys: HashSet<(String, Option<String>, String)> = HashSet::new();

    // --- global_shard: functions + variables + Class:method splits ---
    for (name, candidates) in index.global_shard.iter_all_entries() {
        let (display_name, container) = split_qualified_name(&name);
        if !matches_query(&display_name, &query_lower, query.is_empty()) {
            continue;
        }
        for candidate in candidates {
            let candidate_uri = resolve_uri(candidate.source_uri_id());
            let effective_kind = if container.is_some() {
                // Qualified name: determine kind from the function signature.
                candidate_symbol_kind(candidate, index)
            } else {
                match candidate.kind {
                    GlobalContributionKind::Function => SymbolKind::FUNCTION,
                    _ => SymbolKind::VARIABLE,
                }
            };
            if container.is_some() {
                let key = (
                    display_name.clone(),
                    container.clone(),
                    candidate_uri.to_string(),
                );
                if !member_keys.insert(key) {
                    continue;
                }
            }
            #[allow(deprecated)]
            results.push(SymbolInformation {
                name: display_name.clone(),
                kind: effective_kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: candidate_uri,
                    range: candidate.selection_range.into(),
                },
                container_name: container.clone(),
            });
        }
    }

    // --- type_shard: classes / enums / aliases ---
    for (name, candidates) in &index.type_shard {
        if !matches_query(name, &query_lower, query.is_empty()) {
            continue;
        }
        for candidate in candidates {
            let candidate_uri = resolve_uri(candidate.source_uri_id());
            let kind = match candidate.kind {
                TypeDefinitionKind::Class => SymbolKind::CLASS,
                TypeDefinitionKind::Alias => SymbolKind::INTERFACE,
                TypeDefinitionKind::Enum => SymbolKind::ENUM,
            };
            // Prefer `name_range` (byte range of the `Foo` identifier
            // inside `---@class Foo`) over the broader anchor range so
            // VS Code highlights just the type name after navigation.
            // Look up via the owning summary because `TypeShardEntry`
            // only carries the coarse anchor range.
            let location_range = index
                .summary_by_id(candidate.source_uri_id())
                .and_then(|s| {
                    s.type_definitions
                        .iter()
                        .find(|td| td.name == *name)
                        .and_then(|td| td.name_range)
                })
                .unwrap_or(candidate.range);
            #[allow(deprecated)]
            results.push(SymbolInformation {
                name: name.clone(),
                kind,
                tags: None,
                deprecated: None,
                location: Location {
                    uri: candidate_uri,
                    range: location_range.into(),
                },
                container_name: None,
            });
        }
    }

    // --- Class fields: `@field x integer` in any file's @class ---
    for (uri_id, summary) in index.summaries_iter_id() {
        let uri = resolve_uri(uri_id);
        for td in &summary.type_definitions {
            for fd in &td.fields {
                if !matches_query(&fd.name, &query_lower, query.is_empty()) {
                    continue;
                }
                let key = (
                    fd.name.clone(),
                    Some(td.name.clone()),
                    uri.to_string(),
                );
                if !member_keys.insert(key) {
                    continue;
                }
                let location_range = fd.name_range.unwrap_or(fd.range);
                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: fd.name.clone(),
                    kind: SymbolKind::FIELD,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: location_range.into(),
                    },
                    container_name: Some(td.name.clone()),
                });
            }
        }
    }

    results.sort_by(|a, b| {
        let a_exact = a.name.to_lowercase() == query_lower;
        let b_exact = b.name.to_lowercase() == query_lower;
        b_exact.cmp(&a_exact)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.container_name.cmp(&b.container_name))
            // Final tiebreaker on URI so ordering is deterministic
            // across runs (HashMap iteration order is otherwise
            // nondeterministic).
            .then_with(|| a.location.uri.as_str().cmp(b.location.uri.as_str()))
    });

    results
}

fn matches_query(name: &str, query_lower: &str, query_is_empty: bool) -> bool {
    query_is_empty || name.to_lowercase().contains(query_lower)
}

/// Parse an entry name from `global_shard` into `(display, container)`.
/// Non-qualified names (no `.`) return `(name, None)`.
fn split_qualified_name(name: &str) -> (String, Option<String>) {
    if let Some((class, member)) = name.rsplit_once('.') {
        if !class.is_empty() && !member.is_empty() {
            return (member.to_string(), Some(class.to_string()));
        }
    }
    (name.to_string(), None)
}

/// Determine the SymbolKind for a qualified global candidate by inspecting
/// the function signature. A colon-method (`function Foo:m()`) is METHOD;
/// a dot-function is FUNCTION; anything else is FIELD.
fn candidate_symbol_kind(
    candidate: &crate::aggregation::GlobalCandidate,
    index: &WorkspaceAggregation,
) -> SymbolKind {
    let func_id = match &candidate.type_fact {
        TypeFact::Known(KnownType::FunctionRef(id)) => Some(*id),
        TypeFact::Known(KnownType::Function(_)) => {
            return SymbolKind::FUNCTION;
        }
        _ => None,
    };

    if let Some(id) = func_id {
        if let Some(summary) = index.summary_by_id(candidate.source_uri_id()) {
            if let Some(func) = summary.function_summaries.get(&id) {
                // FunctionSummary.name preserves the original colon form
                // (e.g. "Foo:myMethod"), while GlobalContribution.name is
                // normalized to dot. Check for colon in the name.
                return if func.name.contains(':') {
                    SymbolKind::METHOD
                } else {
                    SymbolKind::FUNCTION
                };
            }
        }
        return SymbolKind::FUNCTION;
    }

    SymbolKind::FIELD
}
