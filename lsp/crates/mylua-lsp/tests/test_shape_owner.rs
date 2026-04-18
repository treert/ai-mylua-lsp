//! Tests for `TableShape.owner_name` — the binding name that
//! anchored a shape. Filled in by summary_builder so downstream
//! consumers (hover, signature_help) can disambiguate same-named
//! methods across two shape tables in the same file.

mod test_helpers;

use test_helpers::*;
use mylua_lsp::type_system::{KnownType, TypeFact};

#[test]
fn local_binding_anchors_owner() {
    let src = r#"
local t = { name = "hello" }
"#;
    let (_doc, uri, agg) = setup_single_file(src, "owner_local.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let ltf = summary.local_type_facts.get("t").expect("t fact");
    match &ltf.type_fact {
        TypeFact::Known(KnownType::Table(id)) => {
            let shape = summary.table_shapes.get(id).expect("shape");
            assert_eq!(shape.owner_name.as_deref(), Some("t"), "owner should be `t`");
        }
        other => panic!("expected Table shape, got: {:?}", other),
    }
}

#[test]
fn global_assignment_anchors_owner() {
    let src = r#"
Foo = { x = 1 }
"#;
    let (_doc, uri, agg) = setup_single_file(src, "owner_global.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    // The global contribution carries the type_fact; look up shape via its id.
    let contrib = summary
        .global_contributions
        .iter()
        .find(|c| c.name == "Foo")
        .expect("Foo contribution");
    match &contrib.type_fact {
        TypeFact::Known(KnownType::Table(id)) => {
            let shape = summary.table_shapes.get(id).expect("shape");
            assert_eq!(shape.owner_name.as_deref(), Some("Foo"));
        }
        other => panic!("expected Table shape, got: {:?}", other),
    }
}

#[test]
fn two_shapes_each_know_their_owner() {
    // Regression focus for the future-work note: two shape tables
    // in the same file each with a same-named method should carry
    // distinct owner_names, giving hover / signature_help a
    // disambiguation hook even without any class annotation.
    let src = r#"
local t1 = { m = function() return 1 end }
local t2 = { m = function() return "s" end }
"#;
    let (_doc, uri, agg) = setup_single_file(src, "owner_two.lua");
    let summary = agg.summaries.get(&uri).expect("summary");

    let id1 = match &summary.local_type_facts.get("t1").unwrap().type_fact {
        TypeFact::Known(KnownType::Table(id)) => *id,
        other => panic!("t1 not a Table, got: {:?}", other),
    };
    let id2 = match &summary.local_type_facts.get("t2").unwrap().type_fact {
        TypeFact::Known(KnownType::Table(id)) => *id,
        other => panic!("t2 not a Table, got: {:?}", other),
    };
    assert_ne!(id1, id2, "shapes must have distinct ids");

    let o1 = summary.table_shapes.get(&id1).unwrap().owner_name.as_deref();
    let o2 = summary.table_shapes.get(&id2).unwrap().owner_name.as_deref();
    assert_eq!(o1, Some("t1"));
    assert_eq!(o2, Some("t2"));
}

#[test]
fn non_table_rhs_leaves_shape_untouched() {
    // `local s = "hello"` isn't a shape owner — no shape was
    // allocated. Verify no panic and no shape produced under this
    // name.
    let src = r#"local s = "hello"
"#;
    let (_doc, uri, agg) = setup_single_file(src, "non_table.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let ltf = summary.local_type_facts.get("s").expect("s fact");
    assert!(
        !matches!(&ltf.type_fact, TypeFact::Known(KnownType::Table(_))),
        "local string must not be a Table, got: {:?}", ltf.type_fact,
    );
}
