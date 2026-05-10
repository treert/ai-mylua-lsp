use crate::lua_symbol::{get_lua_symbol, intern_lua_symbol, LuaSymbol};
use crate::util::ByteRange;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    /// Key type for bracket-key-only tables (e.g. `{ [string] = value, ... }`).
    /// When set, `fields` is empty and the shape represents a map-like table
    /// whose individual entries are not tracked. Consumers should use
    /// `key_type` + `array_element_type` for type information.
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
}
