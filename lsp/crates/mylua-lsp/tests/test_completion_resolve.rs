mod test_helpers;

use std::collections::HashMap;
use mylua_lsp::completion;
use mylua_lsp::document::DocumentStoreView;
use mylua_lsp::uri_id::intern;
use test_helpers::*;
use tower_lsp_server::ls_types::{CompletionItem, CompletionItemKind};

fn find_item<'a>(items: &'a [CompletionItem], label: &str) -> Option<&'a CompletionItem> {
    items.iter().find(|i| i.label == label)
}

#[test]
fn completion_items_carry_resolve_data() {
    // P2-8: `local` and `global` identifier completions must embed
    // resolve-payload so `completion/resolve` can re-locate them.
    //
    // Two probes — one with prefix `f` (picks up foo), one with
    // prefix `b` (picks up bar). We probe separately because
    // completion filters by prefix.
    let src = "local foo = 1\nfunction bar() end\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let uri_id = intern(&uri);

    // Probe 1: cursor after `f` on line 2
    let items_f = completion::complete(&doc, uri_id, pos(2, 0), &mut agg);
    let foo = find_item(&items_f, "foo").expect("foo local in empty-prefix completion");
    assert!(foo.data.is_some(), "local completion should have data for resolve");
    assert_eq!(foo.data.as_ref().unwrap()["kind"], "local");

    // Probe 2: global `bar` also shows up in empty-prefix completion.
    let bar = find_item(&items_f, "bar").expect("bar global in empty-prefix completion");
    assert!(bar.data.is_some(), "global completion should have data for resolve");
    assert_eq!(bar.data.as_ref().unwrap()["kind"], "global");
}

#[test]
fn completion_resolve_enriches_global_with_detail() {
    // Global `bar()` → resolve should attach `detail` with type info.
    let src = "function bar() return 1 end\nb";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let uri_id = intern(&uri);
    let items = completion::complete(&doc, uri_id, pos(1, 1), &mut agg);
    let bar = find_item(&items, "bar").cloned().expect("bar");
    assert!(bar.detail.is_none(), "initial item should have no detail");

    let docs = HashMap::from([(uri_id, doc)]);
    let view = DocumentStoreView::new(&docs);
    let resolved = completion::resolve_completion(bar, &agg, &view, None);
    assert!(
        resolved.detail.is_some(),
        "resolve should attach detail, got: {:?}", resolved,
    );
    let detail = resolved.detail.as_deref().unwrap();
    assert!(
        detail.contains("function") || detail.contains("fun"),
        "detail should mention function type, got: {}", detail,
    );
}

#[test]
fn completion_resolve_enriches_local_with_type() {
    // Local `foo: number` → resolve should attach typed detail.
    let src = "local foo = 42\nf";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let uri_id = intern(&uri);
    let items = completion::complete(&doc, uri_id, pos(1, 1), &mut agg);
    let foo = find_item(&items, "foo").cloned().expect("foo");

    let docs = HashMap::from([(uri_id, doc)]);
    let view = DocumentStoreView::new(&docs);
    let resolved = completion::resolve_completion(foo, &agg, &view, Some(uri_id));
    let detail = resolved
        .detail
        .as_deref()
        .expect("resolve should attach detail for local");
    assert!(detail.contains("foo"), "detail includes name, got: {}", detail);
    assert!(
        detail.contains("number") || detail.contains("integer"),
        "detail should include inferred type, got: {}", detail,
    );
}

#[test]
fn completion_resolve_preserves_items_without_data() {
    // Keywords / emmy tags / require-path items have no `data` — the
    // resolve pass must be a no-op for them.
    let keyword_item = CompletionItem {
        label: "local".to_string(),
        kind: Some(CompletionItemKind::KEYWORD),
        ..Default::default()
    };
    let (_, _, agg) = setup_single_file("", "a.lua");
    let docs = HashMap::new();
    let view = DocumentStoreView::new(&docs);
    let resolved = completion::resolve_completion(keyword_item.clone(), &agg, &view, None);
    assert_eq!(resolved.label, keyword_item.label);
    assert_eq!(resolved.kind, keyword_item.kind);
    assert!(resolved.detail.is_none());
    assert!(resolved.documentation.is_none());
}

#[test]
fn completion_resolve_function_adds_markdown_signature() {
    // Global function with a FunctionSummary entry → resolve should
    // attach `documentation` with a Lua code block showing the
    // signature.
    let src = "function doWork(a, b, c) return a end\nd";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let uri_id = intern(&uri);
    let items = completion::complete(&doc, uri_id, pos(1, 1), &mut agg);
    let item = find_item(&items, "doWork").cloned().expect("doWork");

    let docs = HashMap::from([(uri_id, doc)]);
    let view = DocumentStoreView::new(&docs);
    let resolved = completion::resolve_completion(item, &agg, &view, None);
    match &resolved.documentation {
        Some(tower_lsp_server::ls_types::Documentation::MarkupContent(m)) => {
            assert!(
                m.value.contains("doWork") && m.value.contains("a") && m.value.contains("b"),
                "markdown should show signature, got: {}", m.value,
            );
        }
        other => panic!("expected MarkupContent documentation, got: {:?}", other),
    }
}
