use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

fn main() {
    // Capture `rustc --version` at compile time so logger.rs can embed it.
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output();
    let version = match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    };
    println!("cargo:rustc-env=RUSTC_VERSION={}", version);

    generate_syntax_kind_constants();
}

fn generate_syntax_kind_constants() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let parser_c = Path::new(&manifest_dir).join("../../../grammar/src/parser.c");
    let parser = fs::read_to_string(&parser_c).expect("failed to read grammar/src/parser.c");

    let enum_ids = parse_symbol_enum(&parser);
    let symbol_names = parse_symbol_names(&parser);
    let symbol_map = parse_symbol_map(&parser);

    let mut public_kinds = BTreeMap::<u16, String>::new();
    for public_symbol in symbol_map.values() {
        let Some(&id) = enum_ids.get(public_symbol) else {
            continue;
        };
        let Some(name) = symbol_names.get(public_symbol) else {
            continue;
        };
        if is_internal_kind_name(name) {
            continue;
        }
        public_kinds.entry(id).or_insert_with(|| name.clone());
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let out_file = Path::new(&out_dir).join("syntax_kind_generated.rs");
    fs::write(out_file, render_syntax_kind_constants(&public_kinds))
        .expect("failed to write generated syntax kind constants");

    println!("cargo:rerun-if-changed={}", parser_c.display());
}

fn parse_symbol_enum(parser: &str) -> HashMap<String, u16> {
    let mut ids = HashMap::from([("ts_builtin_sym_end".to_string(), 0)]);
    let mut in_enum = false;
    let mut next_id = 0u16;

    for line in parser.lines() {
        let trimmed = line.trim();
        if !in_enum {
            if trimmed.starts_with("enum ts_symbol_identifiers") {
                in_enum = true;
            }
            continue;
        }
        if trimmed.starts_with("};") {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }

        let item = trimmed.trim_end_matches(',').trim();
        if let Some((name, value)) = item.split_once('=') {
            let name = name.trim();
            if let Ok(value) = value.trim().parse::<u16>() {
                ids.insert(name.to_string(), value);
                next_id = value.saturating_add(1);
            }
        } else {
            ids.insert(item.to_string(), next_id);
            next_id = next_id.saturating_add(1);
        }
    }

    ids
}

fn parse_symbol_names(parser: &str) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let mut in_array = false;

    for line in parser.lines() {
        let trimmed = line.trim();
        if !in_array {
            if trimmed.starts_with("static const char * const ts_symbol_names[]") {
                in_array = true;
            }
            continue;
        }
        if trimmed.starts_with("};") {
            break;
        }

        let Some((key, rhs)) = parse_index_assignment(trimmed) else {
            continue;
        };
        let Some(value) = parse_c_string(rhs) else {
            continue;
        };
        names.insert(key, value);
    }

    names
}

fn parse_symbol_map(parser: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut in_array = false;

    for line in parser.lines() {
        let trimmed = line.trim();
        if !in_array {
            if trimmed.starts_with("static const TSSymbol ts_symbol_map[]") {
                in_array = true;
            }
            continue;
        }
        if trimmed.starts_with("};") {
            break;
        }

        let Some((key, rhs)) = parse_index_assignment(trimmed) else {
            continue;
        };
        map.insert(key, rhs.trim_end_matches(',').trim().to_string());
    }

    map
}

fn parse_index_assignment(line: &str) -> Option<(String, &str)> {
    let lb = line.find('[')?;
    let rb = line[lb + 1..].find(']')? + lb + 1;
    let eq = line[rb + 1..].find('=')? + rb + 1;
    Some((line[lb + 1..rb].trim().to_string(), line[eq + 1..].trim()))
}

fn parse_c_string(value: &str) -> Option<String> {
    let mut chars = value[value.find('"')? + 1..].chars();
    let mut out = String::new();

    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => {
                let escaped = chars.next()?;
                out.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
            }
            other => out.push(other),
        }
    }

    None
}

fn is_internal_kind_name(name: &str) -> bool {
    name.starts_with('_') || name.ends_with("_repeat1")
}

fn render_syntax_kind_constants(kinds: &BTreeMap<u16, String>) -> String {
    let mut used_names = HashSet::new();
    let mut entries = Vec::new();

    for (&id, name) in kinds {
        let mut const_name = const_name_for_kind(name);
        if const_name.is_empty() {
            const_name = format!("KIND_{id}");
        }
        if !used_names.insert(const_name.clone()) {
            const_name = format!("{const_name}_{id}");
            used_names.insert(const_name.clone());
        }
        entries.push((id, name.as_str(), const_name));
    }

    let mut out = String::new();
    out.push_str("// This file is generated by build.rs from grammar/src/parser.c.\n");
    out.push_str("// Do not edit by hand.\n\n");
    out.push_str("use super::SyntaxKind;\n\n");

    for (id, _name, const_name) in &entries {
        out.push_str(&format!(
            "pub const {const_name}: SyntaxKind = SyntaxKind::new({id});\n"
        ));
    }

    out.push_str("\npub const ALL: &[(SyntaxKind, &str)] = &[\n");
    for (_id, name, const_name) in &entries {
        out.push_str(&format!("    ({const_name}, {name:?}),\n"));
    }
    out.push_str("];\n\n");

    out.push_str("#[inline]\n");
    out.push_str("pub fn name(kind: SyntaxKind) -> Option<&'static str> {\n");
    out.push_str("    match kind.id() {\n");
    for (id, name, _const_name) in &entries {
        out.push_str(&format!("        {id} => Some({name:?}),\n"));
    }
    out.push_str("        _ => None,\n");
    out.push_str("    }\n");
    out.push_str("}\n");

    out
}

fn const_name_for_kind(kind: &str) -> String {
    match kind {
        ";" => return "SEMI".to_string(),
        "=" => return "EQ".to_string(),
        "::" => return "COLON_COLON".to_string(),
        "," => return "COMMA".to_string(),
        "." => return "DOT".to_string(),
        ":" => return "COLON".to_string(),
        "[" => return "LBRACK".to_string(),
        "]" => return "RBRACK".to_string(),
        "<" => return "LT".to_string(),
        ">" => return "GT".to_string(),
        "<=" => return "LT_EQ".to_string(),
        ">=" => return "GT_EQ".to_string(),
        "==" => return "EQ_EQ".to_string(),
        "~=" => return "TILDE_EQ".to_string(),
        "|" => return "PIPE".to_string(),
        "~" => return "TILDE".to_string(),
        "&" => return "AMP".to_string(),
        "<<" => return "LT_LT".to_string(),
        ">>" => return "GT_GT".to_string(),
        ".." => return "DOT_DOT".to_string(),
        "+" => return "PLUS".to_string(),
        "-" => return "DASH".to_string(),
        "*" => return "STAR".to_string(),
        "/" => return "SLASH".to_string(),
        "//" => return "SLASH_SLASH".to_string(),
        "%" => return "PERCENT".to_string(),
        "^" => return "CARET".to_string(),
        "#" => return "POUND".to_string(),
        "..." => return "DOT_DOT_DOT".to_string(),
        "(" => return "LPAREN".to_string(),
        ")" => return "RPAREN".to_string(),
        "{" => return "LBRACE".to_string(),
        "}" => return "RBRACE".to_string(),
        "\"" => return "DQUOTE".to_string(),
        "'" => return "SQUOTE".to_string(),
        _ => {}
    }

    let mut out = String::new();
    let mut prev_underscore = false;
    for ch in kind.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }

    let out = out.trim_matches('_');
    if out.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        format!("K_{out}")
    } else {
        out.to_string()
    }
}
