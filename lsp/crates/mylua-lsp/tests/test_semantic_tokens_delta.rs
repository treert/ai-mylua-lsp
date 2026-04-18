//! Integration tests for `compute_semantic_token_delta` against
//! realistic `collect_semantic_tokens_with_version` output. Unit
//! tests in `semantic_tokens.rs::delta_tests` cover the algorithm
//! in isolation; these verify the end-to-end contract (prefix/suffix
//! preservation when real edits happen).

mod test_helpers;

use test_helpers::*;
use mylua_lsp::semantic_tokens;

#[test]
fn delta_zero_edits_for_identical_document() {
    let src = "local a = 1\nlocal b = 2\nprint(a, b)\n";
    let (doc, _uri, _agg) = setup_single_file(src, "same.lua");
    let t1 = semantic_tokens::collect_semantic_tokens_with_version(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
        "5.3",
    );
    let t2 = t1.clone();
    let edits = semantic_tokens::compute_semantic_token_delta(&t1, &t2);
    assert!(edits.is_empty(), "identical token streams produce no edits, got: {:?}", edits);
}

#[test]
fn delta_reflects_appended_line() {
    let before = "local a = 1\nprint(a)\n";
    let after = "local a = 1\nprint(a)\nlocal b = 2\n";
    let (doc1, _, _) = setup_single_file(before, "before.lua");
    let (doc2, _, _) = setup_single_file(after, "after.lua");
    let t1 = semantic_tokens::collect_semantic_tokens_with_version(
        doc1.tree.root_node(),
        doc1.text.as_bytes(),
        &doc1.scope_tree,
        "5.3",
    );
    let t2 = semantic_tokens::collect_semantic_tokens_with_version(
        doc2.tree.root_node(),
        doc2.text.as_bytes(),
        &doc2.scope_tree,
        "5.3",
    );
    let edits = semantic_tokens::compute_semantic_token_delta(&t1, &t2);
    assert_eq!(edits.len(), 1, "single edit for append, got: {:?}", edits);
    // Pure append: `delete_count` must be zero.
    assert_eq!(edits[0].delete_count, 0, "append should delete nothing");
    // `start` should point past the end of the old array (in u32s, so 5×len).
    assert_eq!(edits[0].start as usize, t1.len() * 5);
    // Inserted data length must equal newly added tokens × 5.
    let inserted_tokens = t2.len() - t1.len();
    assert_eq!(
        edits[0].data.as_ref().map(|v| v.len()).unwrap_or(0),
        inserted_tokens,
        "inserted Vec<SemanticToken> length must match net new tokens",
    );
}

#[test]
fn delta_reflects_deleted_line() {
    let before = "local a = 1\nlocal b = 2\nprint(a, b)\n";
    let after = "local a = 1\nprint(a, b)\n";
    let (doc1, _, _) = setup_single_file(before, "del1.lua");
    let (doc2, _, _) = setup_single_file(after, "del2.lua");
    let t1 = semantic_tokens::collect_semantic_tokens_with_version(
        doc1.tree.root_node(),
        doc1.text.as_bytes(),
        &doc1.scope_tree,
        "5.3",
    );
    let t2 = semantic_tokens::collect_semantic_tokens_with_version(
        doc2.tree.root_node(),
        doc2.text.as_bytes(),
        &doc2.scope_tree,
        "5.3",
    );
    let edits = semantic_tokens::compute_semantic_token_delta(&t1, &t2);
    // A deletion might produce 1 edit with delete_count > 0.
    assert_eq!(edits.len(), 1, "single edit expected, got: {:?}", edits);
    assert!(edits[0].delete_count > 0, "deletion must delete at least one token");
}

#[test]
fn delta_middle_edit_preserves_prefix_and_suffix() {
    let before = "local a = 1\nlocal b = 2\nlocal c = 3\n";
    let after = "local a = 1\nlocal bb = 2\nlocal c = 3\n";
    let (doc1, _, _) = setup_single_file(before, "m1.lua");
    let (doc2, _, _) = setup_single_file(after, "m2.lua");
    let t1 = semantic_tokens::collect_semantic_tokens_with_version(
        doc1.tree.root_node(),
        doc1.text.as_bytes(),
        &doc1.scope_tree,
        "5.3",
    );
    let t2 = semantic_tokens::collect_semantic_tokens_with_version(
        doc2.tree.root_node(),
        doc2.text.as_bytes(),
        &doc2.scope_tree,
        "5.3",
    );
    let edits = semantic_tokens::compute_semantic_token_delta(&t1, &t2);
    assert_eq!(edits.len(), 1, "single edit for middle change, got: {:?}", edits);
    // Prefix must skip at least the first token (local `a`).
    assert!(edits[0].start >= 5, "prefix must cover at least one token, got start={}", edits[0].start);
}
