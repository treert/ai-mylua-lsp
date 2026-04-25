use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::util::ByteRange;

/// Stable identity for a table literal or constructed table within a file.
/// The inner `u32` is a per-file unique id assigned during summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableShapeId(pub u32);

/// Describes the statically known shape of a Lua table value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableShape {
    pub id: TableShapeId,
    pub fields: HashMap<String, FieldInfo>,
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
    /// `#[serde(default)]` keeps caches written by earlier builds
    /// loadable.
    #[serde(default)]
    pub owner_name: Option<String>,
    /// Key type for bracket-key-only tables (e.g. `{ [string] = value, ... }`).
    /// When set, `fields` is empty and the shape represents a map-like table
    /// whose individual entries are not tracked. Consumers should use
    /// `key_type` + `array_element_type` for type information.
    #[serde(default)]
    pub key_type: Option<crate::type_system::TypeFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
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
    pub fn set_owner(&mut self, name: String) {
        if self.owner_name.is_none() && !name.is_empty() {
            self.owner_name = Some(name);
        }
    }

    pub fn set_field(&mut self, name: String, info: FieldInfo) {
        self.fields.insert(name, info);
    }

    pub fn mark_open(&mut self) {
        self.is_closed = false;
    }
}
