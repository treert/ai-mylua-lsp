use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_lua() -> *const ();
}

pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_lua) };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_load_grammar() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&LANGUAGE.into())
            .expect("failed to load mylua grammar");

        let source = b"print('hello')";
        let tree = parser.parse(source, None).expect("failed to parse");
        let root = tree.root_node();
        assert_eq!(root.kind(), "source_file");
        assert!(!root.has_error());
    }
}
