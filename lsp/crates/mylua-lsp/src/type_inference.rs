//! AST-level type inference utilities.
//!
//! These functions infer the `TypeFact` of arbitrary expression nodes by
//! walking the tree-sitter AST and consulting the workspace aggregation
//! (summaries, global shard, etc.).  They were originally part of
//! `hover.rs` but are used by multiple LSP features (goto, completion,
//! signature help, diagnostics), so they live in their own module to
//! avoid circular / unnatural dependencies.

use tower_lsp_server::ls_types::Uri;

use crate::aggregation::WorkspaceAggregation;
use crate::resolver;
use crate::type_system::TypeFact;
use crate::util::{node_text, extract_string_literal};

/// Recursively infer the type of an AST expression node.
///
/// The mylua grammar uses `variable` nodes for both plain identifiers and
/// dotted access (`a.b.c` is a `variable` whose `object` field is another
/// `variable` and whose `field` field is an identifier). `field_expression`
/// is kept as a legacy alias for future grammar revisions.
///
/// Handles:
/// - Pure dotted chains (`a.b.c`) via recursive `variable.object/.field`.
/// - Array-style subscripts (`a[1]`, `a[k]`) via `array_element_type` on
///   the base's file-local Table shape.
/// - Call returns (`foo()`, `mod.f()`, `obj:m()`) by reconstructing a
///   `CallReturn` stub so the resolver can track declared `@return`
///   types through the chain — this is what makes `make().field` hover
///   work when `make`'s summary has `@return Foo`.
pub fn infer_node_type(
    node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> TypeFact {
    match node.kind() {
        "variable" | "field_expression" => {
            if let (Some(object), Some(field)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("field"),
            ) {
                let base_fact = infer_node_type(object, source, uri, index);
                let field_name = node_text(field, source).to_string();
                let resolved = resolver::resolve_field_chain_in_file(
                    uri, &base_fact, &[field_name], index,
                );
                return resolved.type_fact;
            }
            // Subscript variant: `variable { object, index }` — look up
            // the base's shape `array_element_type` so chains like
            // `a[1].field` can continue with a real element type.
            if let (Some(object), Some(_index_node)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("index"),
            ) {
                let base_fact = infer_node_type(object, source, uri, index);
                if let TypeFact::Known(crate::type_system::KnownType::Table(shape_id)) = &base_fact {
                    if let Some(summary) = index.summaries.get(uri) {
                        if let Some(shape) = summary.table_shapes.get(shape_id) {
                            if let Some(elem) = &shape.array_element_type {
                                return elem.clone();
                            }
                        }
                    }
                }
                return TypeFact::Unknown;
            }
            // `variable` wrapping a single identifier — local/global lookup.
            if node.named_child_count() == 1 {
                if let Some(child) = node.named_child(0) {
                    if child.kind() == "identifier" {
                        let text = node_text(child, source);
                        if let Some(summary) = index.summaries.get(uri) {
                            if let Some(ltf) = summary.local_type_facts.get(text) {
                                return ltf.type_fact.clone();
                            }
                        }
                        return TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                            name: text.to_string(),
                        });
                    }
                }
            }
            TypeFact::Unknown
        }
        "function_call" => {
            // Reconstruct a `CallReturn` stub (or `RequireRef` for
            // `require("…")`) so the resolver can pick up declared
            // `@return` types. Mirrors the logic in
            // `summary_builder::infer_call_return_type` but works off the
            // workspace aggregation + summary cache rather than the
            // per-file `BuildContext`.
            infer_call_return_fact(node, source, uri, index)
        }
        "parenthesized_expression" => {
            node.named_child(0)
                .map(|inner| infer_node_type(inner, source, uri, index))
                .unwrap_or(TypeFact::Unknown)
        }
        "identifier" => {
            let text = node_text(node, source);
            if let Some(summary) = index.summaries.get(uri) {
                if let Some(ltf) = summary.local_type_facts.get(text) {
                    return ltf.type_fact.clone();
                }
            }
            TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                name: text.to_string(),
            })
        }
        // Literal types — needed for function-level generic inference
        // so that `identity("abc")` can infer `T = string`.
        "number" => TypeFact::Known(crate::type_system::KnownType::Number),
        "string" => TypeFact::Known(crate::type_system::KnownType::String),
        "true" | "false" => TypeFact::Known(crate::type_system::KnownType::Boolean),
        "nil" => TypeFact::Known(crate::type_system::KnownType::Nil),
        _ => TypeFact::Unknown,
    }
}

/// Collect the inferred types of actual arguments at a function call site.
/// Used by function-level generic inference.
pub fn collect_call_arg_types(
    call_node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Vec<TypeFact> {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    crate::util::extract_call_arg_nodes(args, source)
        .into_iter()
        .map(|e| infer_node_type(e, source, uri, index))
        .collect()
}

/// Build a `TypeFact` for the return value of a `function_call` node.
/// Handles three shapes:
/// - `require("mod")`  → `SymbolicStub::RequireRef { module_path }`
/// - `obj:m(...)`      → `CallReturn { base: <obj-fact-as-stub>, func_name: "m" }`
/// - `callee(...)` where callee is a `variable` (identifier or dotted) →
///   `CallReturn { base: <callee-base-as-stub>, func_name }`
/// - Plain local/global function call → look up `FunctionSummary.returns[0]`
///   in the workspace to return the declared first return type.
fn infer_call_return_fact(
    node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> TypeFact {
    use crate::type_system::{SymbolicStub, KnownType};

    let callee = match node.child_by_field_name("callee") {
        Some(c) => c,
        None => return TypeFact::Unknown,
    };

    // `require("mod")` — note callee is a plain identifier.
    if callee.kind() == "identifier" && node_text(callee, source) == "require" {
        if let Some(args) = node.child_by_field_name("arguments") {
            if let Some(first_arg) = args.named_child(0) {
                if let Some(module_path) = extract_string_literal(first_arg, source) {
                    return TypeFact::Stub(SymbolicStub::RequireRef { module_path });
                }
            }
        }
        return TypeFact::Unknown;
    }

    // `obj:m()` — grammar sets `method` field on the call node itself.
    if let Some(method_node) = node.child_by_field_name("method") {
        let method_name = node_text(method_node, source).to_string();
        let base_fact = infer_node_type(callee, source, uri, index);

        // When the base is a generic class instance (e.g. `Stack<string>`),
        // resolve the method's return type eagerly and substitute generic
        // parameters. A `CallReturn` stub would lose the actual type args.
        if let TypeFact::Known(KnownType::EmmyGeneric(ref type_name, ref actual_params)) = base_fact {
            let field_result = resolver::resolve_field_chain_in_file(
                uri, &base_fact, std::slice::from_ref(&method_name), index,
            );
            // If the field resolved to a function, extract its first return
            // type (already substituted by resolve_field_chain_in_file's
            // EmmyGeneric branch).
            if let TypeFact::Known(KnownType::Function(ref sig)) = field_result.type_fact {
                if let Some(ret) = sig.returns.first() {
                    return ret.clone();
                }
            }
            // Fallback: look up the method in function_summaries and
            // substitute generics on the raw return type.
            let ret_fact = resolver::resolve_method_return_with_generics(
                type_name, &method_name, actual_params, index,
            );
            if ret_fact != TypeFact::Unknown {
                return ret_fact;
            }
        }

        let (base_stub, generic_args) = type_fact_to_stub_for_call_base(&base_fact, callee, source);
        return TypeFact::Stub(SymbolicStub::CallReturn {
            base: Box::new(base_stub),
            func_name: method_name,
            generic_args,
        });
    }

    // Dotted call `mod.f()` — callee is a `variable` with `object`+`field`.
    if matches!(callee.kind(), "variable" | "field_expression") {
        if let (Some(base_node), Some(field_node)) = (
            callee.child_by_field_name("object"),
            callee.child_by_field_name("field"),
        ) {
            let func_name = node_text(field_node, source).to_string();
            let base_fact = infer_node_type(base_node, source, uri, index);
            let (base_stub, generic_args) = type_fact_to_stub_for_call_base(&base_fact, base_node, source);
            return TypeFact::Stub(SymbolicStub::CallReturn {
                base: Box::new(base_stub),
                func_name,
                generic_args,
            });
        }
    }

    // Plain local/global call — pick up the declared first return type
    // from the callee's FunctionSummary (if any).
    let callee_text = node_text(callee, source);

    // Extract function summary data first (immutable borrow of index),
    // then release the borrow before calling collect_call_arg_types
    // (which needs mutable borrow).
    let fs_data = index.summaries.get(uri).and_then(|summary| {
        summary.function_summaries.get(callee_text).map(|fs| {
            (
                fs.generic_params.clone(),
                fs.signature.params.clone(),
                fs.signature.returns.clone(),
            )
        })
    });

    if let Some((generic_params, formal_params, returns)) = fs_data {
        // Function-level generic inference: if the callee has @generic params,
        // try to unify them from the actual argument types at the call site.
        if !generic_params.is_empty() {
            let actual_arg_types = collect_call_arg_types(node, source, uri, index);
            if let Some(substituted_returns) = resolver::unify_function_generics(
                &generic_params,
                &formal_params,
                &actual_arg_types,
                &returns,
            ) {
                if let Some(ret) = substituted_returns.first() {
                    return ret.clone();
                }
            }
        }
        if let Some(ret) = returns.first() {
            // `@return T` gives us an EmmyType stub; keep it as-is
            // so the resolver can look up `T`'s fields.
            return match ret {
                TypeFact::Known(KnownType::EmmyType(name)) => {
                    TypeFact::Stub(SymbolicStub::TypeRef { name: name.clone() })
                }
                other => other.clone(),
            };
        }
    }
    TypeFact::Unknown
}

/// Best-effort conversion of a base expression's inferred `TypeFact`
/// into a `SymbolicStub` suitable for `CallReturn.base`. Mirrors the
/// build-time logic in `summary_builder::infer_call_return_type`.
fn type_fact_to_stub_for_call_base(
    base_fact: &TypeFact,
    base_node: tree_sitter::Node,
    source: &[u8],
) -> (crate::type_system::SymbolicStub, Vec<TypeFact>) {
    use crate::type_system::{SymbolicStub, KnownType};
    match base_fact {
        TypeFact::Stub(s) => (s.clone(), vec![]),
        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            (SymbolicStub::TypeRef { name: type_name.clone() }, vec![])
        }
        TypeFact::Known(KnownType::EmmyGeneric(type_name, params)) => {
            (SymbolicStub::TypeRef { name: type_name.clone() }, params.clone())
        }
        _ => (SymbolicStub::GlobalRef {
            name: node_text(base_node, source).to_string(),
        }, vec![]),
    }
}
