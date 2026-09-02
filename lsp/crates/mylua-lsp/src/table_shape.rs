use crate::lua_symbol::{get_lua_symbol, intern_lua_symbol, LuaSymbol};
use crate::util::ByteRange;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stable identity for a table literal or constructed table within a file.
/// The inner `u32` is a per-file unique id assigned during summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableShapeId(pub u32);

/// Describes the statically known shape of a Lua table value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TableShape {
    pub id: TableShapeId,
    pub fields: HashMap<LuaSymbol, FieldInfo>,
    /// Element type for array-style `t[i] = v` writes with non-static keys.
    pub array_element_type: Option<crate::type_system::TypeFact>,
    /// `true` if the field set is considered exhaustive (no dynamic key writes observed).
    pub is_closed: bool,
    /// `true` if recursive nesting hit the depth limit during extraction.
    pub truncated: bool,
    /// Binding name that anchored this shape, if any. Filled in by
    /// `summary_builder` when the shape is allocated for a
    /// `local <name> = { ... }` / `<name> = { ... }` RHS — this
    /// gives hover / signature_help a human-readable owner so
    /// popups can say `(method of t)` when two shape tables in the
    /// same file share a field name. Dotted / subscripted LHS
    /// (`M.field = { ... }`) preserves the full text form.
    pub owner_name: Option<LuaSymbol>,
    /// Key type for map-like bracket entries whose individual keys are
    /// not represented as named fields.
    pub key_type: Option<crate::type_system::TypeFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldInfo {
    pub name: LuaSymbol,
    pub type_fact: crate::type_system::TypeFact,
    /// Where this field was first defined.
    pub def_range: Option<ByteRange>,
    /// Accumulates when the same field is assigned multiple times (union).
    pub assignment_count: u32,
}

/// Maximum nesting depth for recursive table shape extraction.
pub const MAX_TABLE_SHAPE_DEPTH: usize = 8;

// ---------------------------------------------------------------------------
// String-literal key policy
// ---------------------------------------------------------------------------
//
// Both spellings of a static string key —
//
//     local t = { ["foo"] = 1 }   -- constructor entry
//     t["foo"] = 1                -- assignment statement
//
// — go through `classify_string_key`, so the two can never drift apart. The
// policy lives in process globals rather than on `BuildContext` because
// `summary_builder::build_file_analysis` takes no config: the same precedent
// as `uri_id::set_priority_keywords` and
// `tree_sitter_mylua::set_top_keyword_default_disabled`. Both switches are
// therefore pinned at `initialize`; changing them mid-session needs a restart
// so every resident summary was built under one policy.

/// `mylua.tableShape.stringKeys` — default on, matching the historical
/// behaviour of the constructor path.
static STRING_KEYS_ENABLED: AtomicBool = AtomicBool::new(true);
/// `mylua.tableShape.stringKeysRequireIdentifier` — default on.
static STRING_KEYS_REQUIRE_IDENTIFIER: AtomicBool = AtomicBool::new(true);

/// Pin the string-key policy. Called once from `initialize`.
pub fn set_string_key_policy(enabled: bool, require_identifier: bool) {
    STRING_KEYS_ENABLED.store(enabled, Ordering::Relaxed);
    STRING_KEYS_REQUIRE_IDENTIFIER.store(require_identifier, Ordering::Relaxed);
}

/// What to do with a `["<text>"] = value` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKeyDecision {
    /// Record `<text>` as a named field of the shape.
    Field,
    /// Do not record it, and the shape stops being an exhaustive description
    /// of the table: the key *does* have a dot spelling (`t.<text>`), so
    /// leaving the shape closed would turn every such read into a false
    /// "Unknown field" error.
    DropAndOpen,
    /// Do not record it, and keep the shape closed. The key has no dot
    /// spelling (`t["a-b"]`, `t["1"]`), so no dotted read can ever ask about
    /// it and the recorded field set stays an exhaustive answer to the only
    /// question `is_closed` is consulted for.
    Drop,
}

/// Apply the two switches to one static string key.
pub fn classify_string_key(text: &str) -> StringKeyDecision {
    classify_string_key_with(
        text,
        STRING_KEYS_ENABLED.load(Ordering::Relaxed),
        STRING_KEYS_REQUIRE_IDENTIFIER.load(Ordering::Relaxed),
    )
}

/// The decision itself, with the policy passed in.
///
/// Split out from [`classify_string_key`] purely so the matrix can be unit
/// tested: the live policy is a process global, and mutating it from a test
/// would race every other test in the same binary.
pub(crate) fn classify_string_key_with(
    text: &str,
    enabled: bool,
    require_identifier: bool,
) -> StringKeyDecision {
    let is_identifier = is_lua_identifier_key(text);
    if !enabled {
        return if is_identifier {
            StringKeyDecision::DropAndOpen
        } else {
            StringKeyDecision::Drop
        };
    }
    if require_identifier && !is_identifier {
        return StringKeyDecision::Drop;
    }
    StringKeyDecision::Field
}

/// `true` when `text` could be written as `t.<text>` — i.e. it matches Lua's
/// `Name` production. Lua keywords are deliberately **not** excluded: `t.end`
/// is a syntax error, but recording the field costs nothing and keeps
/// `t["end"]` navigable once bracket reads are resolved.
pub fn is_lua_identifier_key(text: &str) -> bool {
    let mut bytes = text.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first == b'_' || first.is_ascii_alphabetic()) {
        return false;
    }
    bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

impl TableShape {
    pub fn new(id: TableShapeId) -> Self {
        Self {
            id,
            fields: HashMap::new(),
            array_element_type: None,
            is_closed: true,
            truncated: false,
            owner_name: None,
            key_type: None,
        }
    }

    /// Attach the binding name that anchors this shape. Idempotent:
    /// the first non-empty name wins, later writes are ignored so a
    /// subsequent field-level extraction can't overwrite the
    /// original binding with a nested-scope alias.
    pub fn set_owner(&mut self, name: &str) {
        if self.owner_name.is_none() && !name.is_empty() {
            self.owner_name = Some(intern_lua_symbol(name));
        }
    }

    pub fn set_field(&mut self, name: &str, info: FieldInfo) {
        self.fields.insert(intern_lua_symbol(name), info);
    }

    pub fn get_field(&self, name: &str) -> Option<&FieldInfo> {
        let name = get_lua_symbol(name)?;
        self.fields.get(&name)
    }

    pub fn mark_open(&mut self) {
        self.is_closed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua_symbol::{get_lua_symbol, intern_lua_symbol};
    use crate::type_system::TypeFact;

    #[test]
    fn long_lived_table_names_use_symbols_but_serialize_as_strings() {
        let mut shape = TableShape::new(TableShapeId(7));
        shape.set_owner("Player");
        shape.set_field(
            "name",
            FieldInfo {
                name: intern_lua_symbol("name"),
                type_fact: TypeFact::Unknown,
                def_range: None,
                assignment_count: 1,
            },
        );

        assert_eq!(shape.owner_name.unwrap().as_str(), "Player");
        assert!(shape.fields.contains_key(&intern_lua_symbol("name")));

        let json = serde_json::to_value(&shape).unwrap();
        assert_eq!(json["owner_name"], "Player");
        assert_eq!(json["fields"]["name"]["name"], "name");
    }

    #[test]
    fn field_lookup_misses_do_not_intern_request_names() {
        let mut shape = TableShape::new(TableShapeId(8));
        shape.set_field(
            "existing",
            FieldInfo {
                name: intern_lua_symbol("existing"),
                type_fact: TypeFact::Unknown,
                def_range: None,
                assignment_count: 1,
            },
        );
        let missing = "__missing_table_field_should_not_intern__";
        assert_eq!(get_lua_symbol(missing), None);

        assert!(shape.get_field("existing").is_some());
        assert!(shape.get_field(missing).is_none());

        assert_eq!(get_lua_symbol(missing), None);
    }

    #[test]
    fn default_policy_records_identifier_string_keys_only() {
        // `stringKeys = true`, `stringKeysRequireIdentifier = true`.
        assert_eq!(
            classify_string_key_with("foo", true, true),
            StringKeyDecision::Field
        );
        assert_eq!(
            classify_string_key_with("_A1", true, true),
            StringKeyDecision::Field
        );
        // No dotted spelling → dropped, but the shape stays closed because
        // no dotted read can ever ask about these.
        for key in ["a-b", "1", "", "名字", "a b"] {
            assert_eq!(
                classify_string_key_with(key, true, true),
                StringKeyDecision::Drop,
                "key {key:?}",
            );
        }
    }

    #[test]
    fn relaxed_identifier_requirement_records_every_string_key() {
        for key in ["foo", "a-b", "1", "名字"] {
            assert_eq!(
                classify_string_key_with(key, true, false),
                StringKeyDecision::Field,
                "key {key:?}",
            );
        }
        // An empty key is still a key; recording it is harmless and keeps the
        // shape exhaustive.
        assert_eq!(
            classify_string_key_with("", true, false),
            StringKeyDecision::Field
        );
    }

    #[test]
    fn disabling_string_keys_opens_the_shape_for_dot_readable_keys_only() {
        // `t.foo` would otherwise become a false "Unknown field" error.
        assert_eq!(
            classify_string_key_with("foo", false, true),
            StringKeyDecision::DropAndOpen
        );
        assert_eq!(
            classify_string_key_with("foo", false, false),
            StringKeyDecision::DropAndOpen
        );
        // Nothing dotted was lost, so exhaustiveness is preserved.
        assert_eq!(
            classify_string_key_with("a-b", false, true),
            StringKeyDecision::Drop
        );
    }
}
