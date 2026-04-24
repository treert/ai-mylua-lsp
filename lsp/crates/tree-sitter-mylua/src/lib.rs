use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_lua() -> *const ();
    fn mylua_set_top_keyword_default_disabled(value: bool);
}

pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_lua) };

/// Set the global default for `top_keyword_disabled` in the external
/// scanner. When `true` (the default), column-0 keywords emit normal
/// `WORD_*` tokens; when `false`, they emit `TOP_WORD_*` for error
/// front-loading.
///
/// Must be called **before** creating any `Parser` instances (i.e.
/// before `Parser::new()` + `set_language()`). Individual files can
/// still override via `---#enable top_keyword` / `---#disable top_keyword`.
///
/// # Safety
/// Writes to a C global variable. Safe as long as it is called once
/// during initialization before any parser threads are spawned.
pub fn set_top_keyword_default_disabled(value: bool) {
    unsafe {
        mylua_set_top_keyword_default_disabled(value);
    }
}

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
