//! Version-dependent Lua built-in names.
//!
//! Every Lua runtime (5.1, 5.2, 5.3, 5.4, LuaJIT) ships with a
//! slightly different set of global functions / library tables. Our
//! diagnostics and semantic-tokens modules need to know which names
//! are built-ins (so `print` / `bit32` / etc. don't trip
//! `undefinedGlobal` and get a `defaultLibrary` modifier).
//!
//! `builtins_for` is the single source of truth — both call sites
//! read from here. To add another runtime, add a new branch and
//! extend the `for_version` tests.

/// Names common to **every** supported Lua version.
const COMMON: &[&str] = &[
    // Basic functions
    "print", "type", "tostring", "tonumber", "error", "assert",
    "pcall", "xpcall", "pairs", "ipairs", "next", "select",
    "require", "dofile", "loadfile", "load", "rawget", "rawset",
    "rawequal", "rawlen", "setmetatable", "getmetatable",
    "collectgarbage",
    // Standard library tables
    "table", "string", "math", "io", "os", "debug", "coroutine",
    "package", "arg",
    // Reserved / global meta
    "_G", "_ENV", "_VERSION",
    // Literals / keywords that show up as identifiers in some AST
    // contexts and should never trip "undefined global".
    "self", "true", "false", "nil",
];

/// Added in Lua 5.2 (and kept in 5.3 / 5.4). Prior to 5.2, these
/// aren't available.
const V52_PLUS: &[&str] = &[];

/// Added in 5.3 (kept in 5.4) — adds `utf8` library.
const V53_PLUS: &[&str] = &["utf8"];

/// Lua 5.1 / 5.2 specific: `unpack` is a top-level function (moved
/// to `table.unpack` in 5.2+; kept alias in 5.2; removed in 5.3).
const V51_ONLY: &[&str] = &["unpack"];

/// Lua 5.2 only: `bit32` stdlib (removed in 5.3).
const V52_ONLY: &[&str] = &["bit32", "unpack"];

/// LuaJIT adds `bit`, `jit`, `ffi` and keeps `unpack`.
const LUAJIT_EXTRA: &[&str] = &["bit", "jit", "ffi", "unpack"];

/// Return the array of built-in identifiers for the given runtime
/// `version` string (as configured in `mylua.runtime.version`).
/// Recognized values: `"5.1"`, `"5.2"`, `"5.3"`, `"5.4"`, `"luajit"`
/// (case-insensitive). Any unrecognized string falls back to Lua 5.3
/// (matching the project baseline documented in `ai-readme.md`).
///
/// The returned set is produced fresh on each call from static
/// constants — this is a cold path (called once per diagnostics /
/// semantic-tokens invocation, not per-node), so avoiding a
/// lazy-static table keeps startup simple.
pub fn builtins_for(version: &str) -> Vec<&'static str> {
    let normalized = version.trim().to_ascii_lowercase();
    let mut out: Vec<&'static str> = COMMON.to_vec();
    match normalized.as_str() {
        "5.1" => {
            out.extend_from_slice(V51_ONLY);
        }
        "5.2" => {
            out.extend_from_slice(V52_PLUS);
            out.extend_from_slice(V52_ONLY);
        }
        "5.3" => {
            out.extend_from_slice(V52_PLUS);
            out.extend_from_slice(V53_PLUS);
        }
        "5.4" => {
            out.extend_from_slice(V52_PLUS);
            out.extend_from_slice(V53_PLUS);
        }
        "luajit" => {
            out.extend_from_slice(LUAJIT_EXTRA);
        }
        _ => {
            // Unknown version → default to 5.3 (project baseline).
            out.extend_from_slice(V52_PLUS);
            out.extend_from_slice(V53_PLUS);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Lua language keywords — shared by completion (to offer keyword items)
/// and rename (to reject renaming to a keyword).
pub const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end",
    "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return",
    "then", "true", "until", "while",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_symbols_present_everywhere() {
        for v in ["5.1", "5.2", "5.3", "5.4", "luajit"] {
            let b = builtins_for(v);
            assert!(b.contains(&"print"), "print missing for {}", v);
            assert!(b.contains(&"string"), "string missing for {}", v);
            assert!(b.contains(&"require"), "require missing for {}", v);
        }
    }

    #[test]
    fn utf8_added_in_53_plus() {
        assert!(!builtins_for("5.1").contains(&"utf8"));
        assert!(!builtins_for("5.2").contains(&"utf8"));
        assert!(builtins_for("5.3").contains(&"utf8"));
        assert!(builtins_for("5.4").contains(&"utf8"));
    }

    #[test]
    fn bit32_only_in_52() {
        assert!(!builtins_for("5.1").contains(&"bit32"));
        assert!(builtins_for("5.2").contains(&"bit32"));
        assert!(!builtins_for("5.3").contains(&"bit32"));
        assert!(!builtins_for("5.4").contains(&"bit32"));
    }

    #[test]
    fn unpack_only_in_51_52_and_luajit() {
        assert!(builtins_for("5.1").contains(&"unpack"));
        assert!(builtins_for("5.2").contains(&"unpack"));
        assert!(!builtins_for("5.3").contains(&"unpack"));
        assert!(!builtins_for("5.4").contains(&"unpack"));
        assert!(builtins_for("luajit").contains(&"unpack"));
    }

    #[test]
    fn luajit_adds_bit_jit_ffi() {
        let b = builtins_for("luajit");
        assert!(b.contains(&"bit"));
        assert!(b.contains(&"jit"));
        assert!(b.contains(&"ffi"));
    }

    #[test]
    fn unknown_version_falls_back_to_53() {
        let b = builtins_for("garbage");
        assert!(b.contains(&"utf8"));
        assert!(!b.contains(&"bit32"));
    }

    #[test]
    fn case_insensitive_and_trimmed() {
        let b1 = builtins_for("  5.3  ");
        let b2 = builtins_for("LUAJIT");
        assert!(b1.contains(&"utf8"));
        assert!(b2.contains(&"jit"));
    }
}
