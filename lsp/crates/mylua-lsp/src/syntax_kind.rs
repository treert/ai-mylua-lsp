//! Tree-sitter node kind IDs for the mylua grammar.
//!
//! `tree_sitter::Node::kind()` is convenient but goes through the C string
//! path. This module centralizes the generated `kind_id()` constants so hot
//! AST walks can use readable numeric kind checks without scattering raw IDs.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct SyntaxKind(u16);

impl SyntaxKind {
    #[inline]
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    #[inline]
    pub const fn id(self) -> u16 {
        self.0
    }
}

impl From<SyntaxKind> for u16 {
    #[inline]
    fn from(kind: SyntaxKind) -> Self {
        kind.id()
    }
}

impl From<u16> for SyntaxKind {
    #[inline]
    fn from(id: u16) -> Self {
        Self::new(id)
    }
}

impl PartialEq<u16> for SyntaxKind {
    #[inline]
    fn eq(&self, other: &u16) -> bool {
        self.0 == *other
    }
}

impl PartialEq<SyntaxKind> for u16 {
    #[inline]
    fn eq(&self, other: &SyntaxKind) -> bool {
        *self == other.0
    }
}

pub trait NodeKindExt {
    fn syntax_kind(&self) -> SyntaxKind;
    fn is_kind(&self, kind: SyntaxKind) -> bool;
    fn kind_name(&self) -> &'static str;
    fn child_by_field(&self, field: u16) -> Option<Self>
    where
        Self: Sized;
}

impl<'tree> NodeKindExt for tree_sitter::Node<'tree> {
    #[inline]
    fn syntax_kind(&self) -> SyntaxKind {
        SyntaxKind::new(self.kind_id())
    }

    #[inline]
    fn is_kind(&self, kind: SyntaxKind) -> bool {
        self.kind_id() == kind.id()
    }

    #[inline]
    fn kind_name(&self) -> &'static str {
        kind::name(self.syntax_kind()).unwrap_or("<unknown>")
    }

    #[inline]
    fn child_by_field(&self, field: u16) -> Option<Self> {
        self.child_by_field_id(field)
    }
}

pub mod kind {
    include!(concat!(env!("OUT_DIR"), "/syntax_kind_generated.rs"));
}

pub mod field {
    include!(concat!(env!("OUT_DIR"), "/syntax_field_generated.rs"));
}

pub use kind::name;

#[cfg(test)]
mod tests {
    use super::{kind, NodeKindExt};

    #[test]
    fn generated_constants_match_tree_sitter_language() {
        let language: tree_sitter::Language = tree_sitter_mylua::LANGUAGE.into();

        for (kind, name) in kind::ALL {
            let named = !matches!(
                *name,
                ";" | "="
                    | "::"
                    | ","
                    | "."
                    | ":"
                    | "["
                    | "]"
                    | "<"
                    | ">"
                    | "<="
                    | ">="
                    | "=="
                    | "~="
                    | "|"
                    | "~"
                    | "&"
                    | "<<"
                    | ">>"
                    | ".."
                    | "+"
                    | "-"
                    | "*"
                    | "/"
                    | "//"
                    | "%"
                    | "^"
                    | "#"
                    | "..."
                    | "("
                    | ")"
                    | "{"
                    | "}"
                    | "\""
                    | "'"
            );
            assert_eq!(language.id_for_node_kind(name, named), kind.id(), "{name}");
        }
    }

    #[test]
    fn node_extension_reads_kind_id() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_mylua::LANGUAGE.into())
            .expect("mylua grammar");
        let tree = parser
            .parse("local function hello() end\n", None)
            .expect("parse");
        let root = tree.root_node();

        assert!(root.is_kind(kind::SOURCE_FILE));
        assert_eq!(super::name(root.syntax_kind()), Some("source_file"));
        match root.syntax_kind() {
            kind::SOURCE_FILE => {}
            other => panic!("unexpected root kind: {other:?}"),
        }
    }

    #[test]
    fn generated_field_constants_match_tree_sitter_language() {
        let language: tree_sitter::Language = tree_sitter_mylua::LANGUAGE.into();

        for (field, name) in super::field::ALL {
            assert_eq!(
                language.field_id_for_name(name).map(u16::from),
                Some(*field),
                "{name}"
            );
            assert_eq!(language.field_name_for_id(*field), Some(*name), "{name}");
        }
    }

    #[test]
    fn node_extension_reads_child_by_field_id() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_mylua::LANGUAGE.into())
            .expect("mylua grammar");
        let tree = parser.parse("obj:method(1)\n", None).expect("parse");
        let root = tree.root_node();
        let call = root
            .descendant_for_byte_range(0, "obj:method(1)".len())
            .expect("function call");

        assert!(call.is_kind(kind::FUNCTION_CALL));
        assert_eq!(
            call.child_by_field(super::field::CALLEE)
                .map(|node| node.kind_name()),
            Some("variable")
        );
        assert_eq!(
            call.child_by_field(super::field::METHOD)
                .map(|node| node.kind_name()),
            Some("identifier")
        );
        assert_eq!(
            call.child_by_field(super::field::ARGUMENTS)
                .map(|node| node.kind_name()),
            Some("arguments")
        );
    }
}
