use std::fmt;
use std::sync::OnceLock;

use lasso::{Spur, ThreadedRodeo};
use serde::{Serialize, Serializer};

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LuaSymbol(Spur);

static LUA_SYMBOLS: OnceLock<ThreadedRodeo> = OnceLock::new();

pub fn intern_lua_symbol(text: &str) -> LuaSymbol {
    LuaSymbol(symbols().get_or_intern(text))
}

pub fn resolve_lua_symbol(symbol: LuaSymbol) -> &'static str {
    symbols().resolve(&symbol.0)
}

impl LuaSymbol {
    pub fn as_str(self) -> &'static str {
        resolve_lua_symbol(self)
    }
}

impl fmt::Debug for LuaSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LuaSymbol").field(&self.as_str()).finish()
    }
}

impl fmt::Display for LuaSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for LuaSymbol {
    fn from(value: &str) -> Self {
        intern_lua_symbol(value)
    }
}

impl Serialize for LuaSymbol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

fn symbols() -> &'static ThreadedRodeo {
    LUA_SYMBOLS.get_or_init(ThreadedRodeo::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_equal_text_to_equal_symbols() {
        let a = intern_lua_symbol("Player.name");
        let b = intern_lua_symbol("Player.name");

        assert_eq!(a, b);
    }

    #[test]
    fn interns_unequal_text_to_unequal_symbols() {
        let a = intern_lua_symbol("Player.name");
        let b = intern_lua_symbol("Player.level");

        assert_ne!(a, b);
    }

    #[test]
    fn resolves_symbol_to_original_text() {
        let symbol = intern_lua_symbol("Player.name");

        assert_eq!(resolve_lua_symbol(symbol), "Player.name");
        assert_eq!(symbol.as_str(), "Player.name");
    }

    #[test]
    fn formats_display_as_original_text() {
        let symbol = intern_lua_symbol("Player.name");

        assert_eq!(symbol.to_string(), "Player.name");
    }

    #[test]
    fn formats_debug_with_original_text() {
        let symbol = intern_lua_symbol("Player.name");

        assert_eq!(format!("{:?}", symbol), "LuaSymbol(\"Player.name\")");
    }

    #[test]
    fn interns_from_str() {
        let symbol = LuaSymbol::from("Player.name");

        assert_eq!(symbol.as_str(), "Player.name");
    }

    #[test]
    fn serializes_as_original_json_string() {
        let symbol = intern_lua_symbol("Player.name");
        let json = serde_json::to_string(&symbol).unwrap();

        assert_eq!(json, "\"Player.name\"");
    }
}
