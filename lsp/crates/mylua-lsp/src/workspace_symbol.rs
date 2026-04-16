use tower_lsp_server::ls_types::*;
use crate::summary::GlobalContributionKind;
use crate::aggregation::WorkspaceAggregation;

pub fn search_workspace_symbols(
    query: &str,
    index: &WorkspaceAggregation,
) -> Vec<SymbolInformation> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (name, candidates) in &index.global_shard {
        if query.is_empty() || name.to_lowercase().contains(&query_lower) {
            for candidate in candidates {
                let kind = match candidate.kind {
                    GlobalContributionKind::Function => SymbolKind::FUNCTION,
                    _ => SymbolKind::VARIABLE,
                };

                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: name.clone(),
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: candidate.source_uri.clone(),
                        range: candidate.selection_range,
                    },
                    container_name: None,
                });
            }
        }
    }

    for (name, candidates) in &index.type_shard {
        if query.is_empty() || name.to_lowercase().contains(&query_lower) {
            for candidate in candidates {
                let kind = match candidate.kind {
                    crate::summary::TypeDefinitionKind::Class => SymbolKind::CLASS,
                    crate::summary::TypeDefinitionKind::Alias => SymbolKind::TYPE_PARAMETER,
                    crate::summary::TypeDefinitionKind::Enum => SymbolKind::ENUM,
                };

                #[allow(deprecated)]
                results.push(SymbolInformation {
                    name: name.clone(),
                    kind,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: candidate.source_uri.clone(),
                        range: candidate.range,
                    },
                    container_name: None,
                });
            }
        }
    }

    results.sort_by(|a, b| {
        let a_exact = a.name.to_lowercase() == query_lower;
        let b_exact = b.name.to_lowercase() == query_lower;
        b_exact.cmp(&a_exact).then_with(|| a.name.cmp(&b.name))
    });

    results
}
