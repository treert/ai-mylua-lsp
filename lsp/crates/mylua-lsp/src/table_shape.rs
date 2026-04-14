use std::collections::HashMap;
use tower_lsp_server::ls_types::Range;

/// Stable identity for a table literal or constructed table within a file.
/// The inner `u32` is a per-file unique id assigned during summary generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TableShapeId(pub u32);

/// Describes the statically known shape of a Lua table value.
#[derive(Debug, Clone, PartialEq)]
pub struct TableShape {
    pub id: TableShapeId,
    pub fields: HashMap<String, FieldInfo>,
    /// Element type for array-style `t[i] = v` writes with non-static keys.
    pub array_element_type: Option<crate::type_system::TypeFact>,
    /// `true` if the field set is considered exhaustive (no dynamic key writes observed).
    pub is_closed: bool,
    /// `true` if recursive nesting hit the depth limit during extraction.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldInfo {
    pub name: String,
    pub type_fact: crate::type_system::TypeFact,
    /// Where this field was first defined.
    pub def_range: Option<Range>,
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
        }
    }

    pub fn set_field(&mut self, name: String, info: FieldInfo) {
        self.fields.insert(name, info);
    }

    pub fn mark_open(&mut self) {
        self.is_closed = false;
    }
}
