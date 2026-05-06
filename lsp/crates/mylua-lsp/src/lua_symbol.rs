use std::fmt;
use std::ops::Deref;
use std::sync::OnceLock;
use std::num::NonZeroUsize;

use lasso::{Capacity, Spur, ThreadedRodeo};
use serde::{Serialize, Serializer};

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LuaSymbol(Spur);

static LUA_SYMBOLS: OnceLock<ThreadedRodeo> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LuaSymbolStats {
    pub symbol_count: usize,
    pub string_bytes: usize,
    pub arena_bytes: usize,
}

pub fn intern_lua_symbol(text: &str) -> LuaSymbol {
    LuaSymbol(symbols().get_or_intern(text))
}

pub fn get_lua_symbol(text: &str) -> Option<LuaSymbol> {
    symbols().get(text).map(LuaSymbol)
}

pub fn resolve_lua_symbol(symbol: LuaSymbol) -> &'static str {
    symbols().resolve(&symbol.0)
}

pub fn lua_symbol_stats() -> LuaSymbolStats {
    let symbols = symbols();
    LuaSymbolStats {
        symbol_count: symbols.len(),
        string_bytes: symbols.strings().map(str::len).sum(),
        arena_bytes: symbols.current_memory_usage(),
    }
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

impl From<String> for LuaSymbol {
    fn from(value: String) -> Self {
        intern_lua_symbol(&value)
    }
}

impl PartialEq<&str> for LuaSymbol {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<LuaSymbol> for &str {
    fn eq(&self, other: &LuaSymbol) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<str> for LuaSymbol {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<LuaSymbol> for str {
    fn eq(&self, other: &LuaSymbol) -> bool {
        self == other.as_str()
    }
}

impl Deref for LuaSymbol {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
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
    LUA_SYMBOLS.get_or_init(|| {
        ThreadedRodeo::with_capacity(
            Capacity::new(128_0000, NonZeroUsize::new(32 * 1024 * 1024).unwrap())
        )
    })
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
    fn gets_existing_symbol_without_interning_missing_text() {
        let symbol = intern_lua_symbol("Player.name");

        assert_eq!(get_lua_symbol("Player.name"), Some(symbol));
        assert_eq!(get_lua_symbol("Player.missing"), None);
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

    #[test]
    fn reports_symbol_pool_stats() {
        let before = lua_symbol_stats();
        intern_lua_symbol("__lua_symbol_stats_test_a__");
        intern_lua_symbol("__lua_symbol_stats_test_b__");
        let after = lua_symbol_stats();

        assert!(after.symbol_count >= before.symbol_count + 2);
        assert!(after.string_bytes >= before.string_bytes + "__lua_symbol_stats_test_a__".len());
        assert!(after.arena_bytes >= after.string_bytes);
    }
}
