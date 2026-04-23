//! EmmyLua annotation parser — recursive-descent implementation.
//!
//! Parses `---` comment text into structured `EmmyAnnotation` values with
//! fully parsed `EmmyType` ASTs (no raw `type_text` strings).
//!
//! Grammar reference: `grammar/emmy.bnf`.

use std::fmt;

// ===========================================================================
// AST types (emmy.bnf Part 4 → Rust)
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum EmmyType {
    Named { name: String, generics: Vec<EmmyType> },
    Union(Vec<EmmyType>),
    Optional(Box<EmmyType>),
    Array(Box<EmmyType>),
    Function { params: Vec<EmmyFunParam>, returns: Vec<EmmyType> },
    Table(Vec<EmmyTableField>),
    Literal(String),
    Variadic(Box<EmmyType>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmmyFunParam {
    pub name: Option<String>,
    pub type_expr: EmmyType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmmyTableField {
    pub key: EmmyTableFieldKey,
    pub value: EmmyType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmmyTableFieldKey {
    Name(String),
    IndexType(EmmyType),
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenericParam {
    pub name: String,
    pub constraint: Option<EmmyType>,
}

// ===========================================================================
// Display trait for EmmyType (hover / debug)
// ===========================================================================

impl fmt::Display for EmmyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named { name, generics } => {
                write!(f, "{}", name)?;
                if !generics.is_empty() {
                    write!(f, "<")?;
                    for (i, g) in generics.iter().enumerate() {
                        if i > 0 { write!(f, ", ")?; }
                        write!(f, "{}", g)?;
                    }
                    write!(f, ">")?;
                }
                Ok(())
            }
            Self::Union(types) => {
                for (i, t) in types.iter().enumerate() {
                    if i > 0 { write!(f, "|")?; }
                    write!(f, "{}", t)?;
                }
                Ok(())
            }
            Self::Optional(inner) => write!(f, "{}?", inner),
            Self::Array(inner) => {
                match inner.as_ref() {
                    EmmyType::Union(_) | EmmyType::Optional(_) => write!(f, "({})[]", inner),
                    _ => write!(f, "{}[]", inner),
                }
            }
            Self::Function { params, returns } => {
                write!(f, "fun(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    write!(f, "{}", p)?;
                }
                write!(f, ")")?;
                if !returns.is_empty() {
                    write!(f, ":")?;
                    for (i, r) in returns.iter().enumerate() {
                        if i > 0 { write!(f, ", ")?; }
                        write!(f, "{}", r)?;
                    }
                }
                Ok(())
            }
            Self::Table(fields) => {
                write!(f, "{{")?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 { write!(f, ", ")?; }
                    match &field.key {
                        EmmyTableFieldKey::Name(n) => write!(f, "{}: {}", n, field.value)?,
                        EmmyTableFieldKey::IndexType(k) => write!(f, "[{}]: {}", k, field.value)?,
                    }
                }
                write!(f, "}}")
            }
            Self::Literal(s) => write!(f, "{}", s),
            Self::Variadic(inner) => write!(f, "...{}", inner),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl fmt::Display for EmmyFunParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref name) = self.name {
            write!(f, "{}: {}", name, self.type_expr)
        } else {
            write!(f, "{}", self.type_expr)
        }
    }
}

// ===========================================================================
// Annotations (emmy.bnf Part 3)
// ===========================================================================

#[derive(Debug, Clone)]
pub enum EmmyAnnotation {
    Class { name: String, parents: Vec<String>, desc: String },
    Field { visibility: Option<String>, name: String, type_expr: EmmyType, desc: String },
    Param { name: String, optional: bool, type_expr: EmmyType, desc: String },
    Return { return_types: Vec<EmmyType>, name: Option<String>, desc: String },
    Type { type_expr: EmmyType, desc: String },
    Alias { name: String, type_expr: EmmyType },
    Generic { params: Vec<GenericParam> },
    Overload { fun_type: EmmyType },
    Vararg { type_expr: EmmyType },
    Deprecated { desc: String },
    Async,
    Nodiscard,
    Enum { name: String },
    See { path: String },
    Diagnostic { text: String },
    /// `---@meta [name]` — marks the file as a stub/definition file
    /// per Lua-LS convention. The optional `name` is the logical
    /// module the stub represents.
    Meta { name: Option<String> },
    Other { tag: String, text: String },
}

// ===========================================================================
// Tokenizer
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Name(String),
    Number(String),
    StringLit(String),
    At,
    Pipe,
    Question,
    Colon,
    Comma,
    Dot,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    LAngle,
    RAngle,
    ArraySuffix,
    Ellipsis,
    Hash,
    Eof,
}

struct Tokenizer {
    /// Original source text (owned). Needed so `rest_as_string` can return a
    /// raw slice that preserves non-ASCII chars (e.g. Chinese), emoji, and
    /// original whitespace — the token stream alone throws these away.
    source: String,
    tokens: Vec<Token>,
    /// Parallel to `tokens`: byte range `[start, end)` of each token inside
    /// `source`. The trailing `Token::Eof` entry is `(consumable_end, consumable_end)`.
    spans: Vec<(usize, usize)>,
    pos: usize,
    /// Byte offset right after the most recently advanced token. Bumped by
    /// `advance()` from `spans[pos].1`; used by `rest_as_string` as the start
    /// of the "everything after what we've consumed" slice.
    cursor: usize,
    /// Byte offset where tokenization stopped. Equals `source.len()` unless a
    /// trailing `--` Lua comment was hit, in which case it is the index of
    /// the first `-`. `rest_as_string` never reads past this.
    consumable_end: usize,
}

fn tokenize(input: &str) -> (Vec<Token>, Vec<(usize, usize)>, usize) {
    let mut tokens = Vec::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let consumable_end;

    loop {
        if i >= len {
            consumable_end = len;
            break;
        }
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => { i += 1; }
            b'@' => { tokens.push(Token::At); spans.push((i, i + 1)); i += 1; }
            b'|' => { tokens.push(Token::Pipe); spans.push((i, i + 1)); i += 1; }
            b'?' => { tokens.push(Token::Question); spans.push((i, i + 1)); i += 1; }
            b':' => { tokens.push(Token::Colon); spans.push((i, i + 1)); i += 1; }
            b',' => { tokens.push(Token::Comma); spans.push((i, i + 1)); i += 1; }
            b'(' => { tokens.push(Token::LParen); spans.push((i, i + 1)); i += 1; }
            b')' => { tokens.push(Token::RParen); spans.push((i, i + 1)); i += 1; }
            b'{' => { tokens.push(Token::LBrace); spans.push((i, i + 1)); i += 1; }
            b'}' => { tokens.push(Token::RBrace); spans.push((i, i + 1)); i += 1; }
            b'<' => { tokens.push(Token::LAngle); spans.push((i, i + 1)); i += 1; }
            b'>' => { tokens.push(Token::RAngle); spans.push((i, i + 1)); i += 1; }
            b'#' => { tokens.push(Token::Hash); spans.push((i, i + 1)); i += 1; }
            b'[' => {
                if i + 1 < len && bytes[i + 1] == b']' {
                    tokens.push(Token::ArraySuffix);
                    spans.push((i, i + 2));
                    i += 2;
                } else {
                    tokens.push(Token::LBracket);
                    spans.push((i, i + 1));
                    i += 1;
                }
            }
            b']' => { tokens.push(Token::RBracket); spans.push((i, i + 1)); i += 1; }
            b'.' => {
                if i + 2 < len && bytes[i + 1] == b'.' && bytes[i + 2] == b'.' {
                    tokens.push(Token::Ellipsis);
                    spans.push((i, i + 3));
                    i += 3;
                } else {
                    tokens.push(Token::Dot);
                    spans.push((i, i + 1));
                    i += 1;
                }
            }
            b'"' | b'\'' => {
                let quote = b;
                let start = i;
                i += 1;
                while i < len && bytes[i] != quote {
                    if bytes[i] == b'\\' && i + 1 < len { i += 1; }
                    i += 1;
                }
                if i < len { i += 1; } // closing quote
                let s = std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string();
                tokens.push(Token::StringLit(s));
                spans.push((start, i));
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                // Rest of line is a trailing Lua comment. Stop tokenizing AND
                // treat everything past here as outside the annotation body
                // (so `rest_as_string` won't leak `-- foo` into a desc).
                consumable_end = i;
                break;
            }
            b'0'..=b'9' => {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'+' || bytes[i] == b'-' || bytes[i] == b'_') {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string();
                tokens.push(Token::Number(s));
                spans.push((start, i));
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let s = std::str::from_utf8(&bytes[start..i]).unwrap_or("").to_string();
                tokens.push(Token::Name(s));
                spans.push((start, i));
            }
            _ => { i += 1; } // skip unknown bytes (e.g. UTF-8 continuation of CJK chars)
        }
    }

    tokens.push(Token::Eof);
    spans.push((consumable_end, consumable_end));
    (tokens, spans, consumable_end)
}

impl Tokenizer {
    fn new(input: &str) -> Self {
        let (tokens, spans, consumable_end) = tokenize(input);
        Self {
            source: input.to_string(),
            tokens,
            spans,
            pos: 0,
            cursor: 0,
            consumable_end,
        }
    }

    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&Token::Eof)
    }

    fn peek_at(&self, offset: usize) -> &Token {
        self.tokens.get(self.pos + offset).unwrap_or(&Token::Eof)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens.get(self.pos).cloned().unwrap_or(Token::Eof);
        if self.pos < self.tokens.len() {
            if let Some(&(_, end)) = self.spans.get(self.pos) {
                self.cursor = end;
            }
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, expected: &Token) -> bool {
        if self.peek() == expected {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_name(&mut self) -> Option<String> {
        if let Token::Name(_) = self.peek() {
            if let Token::Name(s) = self.advance() { Some(s) } else { None }
        } else {
            None
        }
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Token::Eof)
    }

    /// Consume everything after the last advanced token as a raw description
    /// string. Uses the original source slice (not a token rejoin) so that:
    ///   - non-ASCII chars (Chinese, emoji, ...) survive
    ///   - original whitespace is preserved (single trim at each end only)
    ///   - an optional leading `@` or `#` — EmmyLua's conventional separator
    ///     between the type and the description (`---@field x string @ desc`)
    ///     — is stripped once.
    fn rest_as_string(&mut self) -> String {
        let start = self.cursor.min(self.consumable_end);
        let end = self.consumable_end;
        let raw = self.source.get(start..end).unwrap_or("");
        let trimmed = raw.trim();
        let after_sep = trimmed
            .strip_prefix('@')
            .or_else(|| trimmed.strip_prefix('#'))
            .unwrap_or(trimmed);
        // Advance internal state so subsequent `at_eof` / `peek` calls behave.
        self.pos = self.tokens.len().saturating_sub(1);
        self.cursor = end;
        after_sep.trim().to_string()
    }
}

// ===========================================================================
// Type expression parser (emmy.bnf Part 4)
// ===========================================================================

/// Parse a complete type expression from a tokenizer.
fn parse_type_expr(tz: &mut Tokenizer) -> EmmyType {
    parse_union_type(tz)
}

/// `emmy_union_type ::= emmy_optional_type { '|' emmy_optional_type }`
fn parse_union_type(tz: &mut Tokenizer) -> EmmyType {
    let first = parse_optional_type(tz);
    let mut types = vec![first];
    while tz.eat(&Token::Pipe) {
        types.push(parse_optional_type(tz));
    }
    if types.len() == 1 {
        types.into_iter().next().unwrap()
    } else {
        EmmyType::Union(types)
    }
}

/// `emmy_optional_type ::= emmy_array_type [ '?' ]`
fn parse_optional_type(tz: &mut Tokenizer) -> EmmyType {
    let inner = parse_array_type(tz);
    if tz.eat(&Token::Question) {
        EmmyType::Optional(Box::new(inner))
    } else {
        inner
    }
}

/// `emmy_array_type ::= emmy_atom_type { '[]' }`
fn parse_array_type(tz: &mut Tokenizer) -> EmmyType {
    let mut inner = parse_atom_type(tz);
    while tz.eat(&Token::ArraySuffix) {
        inner = EmmyType::Array(Box::new(inner));
    }
    inner
}

/// `emmy_atom_type ::= literal | name_type | fun_type | table_type | paren_type | variadic_type`
fn parse_atom_type(tz: &mut Tokenizer) -> EmmyType {
    match tz.peek() {
        Token::StringLit(_) => {
            if let Token::StringLit(s) = tz.advance() { EmmyType::Literal(s) } else { EmmyType::Unknown }
        }
        Token::Number(_) => {
            if let Token::Number(s) = tz.advance() { EmmyType::Literal(s) } else { EmmyType::Unknown }
        }
        Token::Name(s) if s == "fun" => {
            parse_fun_type(tz)
        }
        Token::Name(s) if s == "true" || s == "false" || s == "nil" => {
            let lit = s.clone();
            tz.advance();
            EmmyType::Literal(lit)
        }
        Token::Name(_) => {
            parse_name_type(tz)
        }
        Token::LBrace => {
            parse_table_type(tz)
        }
        Token::LParen => {
            tz.advance();
            let inner = parse_type_expr(tz);
            tz.eat(&Token::RParen);
            inner
        }
        Token::Ellipsis => {
            tz.advance();
            if tz.at_eof() || matches!(tz.peek(), Token::RParen | Token::Comma | Token::Pipe) {
                EmmyType::Variadic(Box::new(EmmyType::Named { name: "any".to_string(), generics: vec![] }))
            } else {
                let inner = parse_type_expr(tz);
                EmmyType::Variadic(Box::new(inner))
            }
        }
        _ => EmmyType::Unknown,
    }
}

/// `emmy_name_type ::= Name [ '<' emmy_type_list '>' ]`
fn parse_name_type(tz: &mut Tokenizer) -> EmmyType {
    let name = match tz.eat_name() {
        Some(n) => n,
        None => return EmmyType::Unknown,
    };
    let generics = if tz.eat(&Token::LAngle) {
        let mut types = vec![parse_type_expr(tz)];
        while tz.eat(&Token::Comma) {
            types.push(parse_type_expr(tz));
        }
        tz.eat(&Token::RAngle);
        types
    } else {
        vec![]
    };
    EmmyType::Named { name, generics }
}

/// `emmy_fun_type ::= 'fun' '(' [ param_list ] ')' [ ':' type_list ]`
fn parse_fun_type(tz: &mut Tokenizer) -> EmmyType {
    tz.advance(); // consume 'fun'
    if !tz.eat(&Token::LParen) {
        return EmmyType::Function { params: vec![], returns: vec![] };
    }
    let mut params = Vec::new();
    if !matches!(tz.peek(), Token::RParen) {
        params.push(parse_fun_param(tz));
        while tz.eat(&Token::Comma) {
            params.push(parse_fun_param(tz));
        }
    }
    tz.eat(&Token::RParen);

    let returns = if tz.eat(&Token::Colon) {
        parse_type_list(tz)
    } else {
        vec![]
    };
    EmmyType::Function { params, returns }
}

/// Parse a single function parameter.
/// Disambiguate `Name ':' type_expr` (named) vs `type_expr` (positional)
/// and `'...' [':' type_expr]` (variadic).
fn parse_fun_param(tz: &mut Tokenizer) -> EmmyFunParam {
    // Variadic param: `... [: type_expr]`
    if matches!(tz.peek(), Token::Ellipsis) {
        tz.advance();
        let type_expr = if tz.eat(&Token::Colon) {
            parse_type_expr(tz)
        } else {
            EmmyType::Named { name: "any".to_string(), generics: vec![] }
        };
        return EmmyFunParam {
            name: Some("...".to_string()),
            type_expr: EmmyType::Variadic(Box::new(type_expr)),
        };
    }
    // Named param: `Name ':' type_expr` — 1-token lookahead
    if let Token::Name(_) = tz.peek() {
        if matches!(tz.peek_at(1), Token::Colon) {
            let name = tz.eat_name().unwrap();
            tz.advance(); // colon
            let type_expr = parse_type_expr(tz);
            return EmmyFunParam { name: Some(name), type_expr };
        }
    }
    // Positional: just a type_expr
    let type_expr = parse_type_expr(tz);
    EmmyFunParam { name: None, type_expr }
}

/// `emmy_table_type ::= '{' [ field_list ] '}'`
fn parse_table_type(tz: &mut Tokenizer) -> EmmyType {
    tz.advance(); // consume '{'
    let mut fields = Vec::new();
    while !matches!(tz.peek(), Token::RBrace | Token::Eof) {
        if let Some(field) = parse_table_field(tz) {
            fields.push(field);
        } else {
            break;
        }
        // Allow trailing comma/semicolon
        if !tz.eat(&Token::Comma) {
            // also accept no separator before '}'
            break;
        }
    }
    tz.eat(&Token::RBrace);
    EmmyType::Table(fields)
}

/// Parse a single table field: `Name ':' type` or `'[' type ']' ':' type`
fn parse_table_field(tz: &mut Tokenizer) -> Option<EmmyTableField> {
    match tz.peek() {
        Token::LBracket => {
            tz.advance();
            let key_type = parse_type_expr(tz);
            tz.eat(&Token::RBracket);
            tz.eat(&Token::Colon);
            let value = parse_type_expr(tz);
            Some(EmmyTableField { key: EmmyTableFieldKey::IndexType(key_type), value })
        }
        Token::Name(_) => {
            // Need lookahead to distinguish `Name ':'` from a type expression
            if matches!(tz.peek_at(1), Token::Colon) {
                let name = tz.eat_name().unwrap();
                tz.advance(); // colon
                let value = parse_type_expr(tz);
                Some(EmmyTableField { key: EmmyTableFieldKey::Name(name), value })
            } else {
                None // not a valid table field
            }
        }
        _ => None,
    }
}

/// Parse a comma-separated list of type expressions.
fn parse_type_list(tz: &mut Tokenizer) -> Vec<EmmyType> {
    let mut types = vec![parse_type_expr(tz)];
    while tz.eat(&Token::Comma) {
        types.push(parse_type_expr(tz));
    }
    types
}

// ===========================================================================
// Annotation-level parser (emmy.bnf Part 3)
// ===========================================================================

pub fn parse_emmy_comments(comment_text: &str) -> Vec<EmmyAnnotation> {
    let mut annotations = Vec::new();

    for line in comment_text.lines() {
        let line = line.trim();
        let content = if let Some(rest) = line.strip_prefix("---") {
            rest.trim()
        } else if let Some(rest) = line.strip_prefix("--") {
            rest.trim()
        } else {
            continue;
        };

        if let Some(rest) = content.strip_prefix('@') {
            if let Some(ann) = parse_annotation_line(rest) {
                annotations.push(ann);
            }
        }
    }

    annotations
}

fn parse_annotation_line(text: &str) -> Option<EmmyAnnotation> {
    let mut tz = Tokenizer::new(text);
    let tag = tz.eat_name()?;
    match tag.as_str() {
        "class" => parse_ann_class(&mut tz),
        "field" => parse_ann_field(&mut tz),
        "param" => parse_ann_param(&mut tz),
        "return" => parse_ann_return(&mut tz),
        "type" => parse_ann_type(&mut tz),
        "alias" => parse_ann_alias(&mut tz),
        "generic" => parse_ann_generic(&mut tz),
        "overload" => parse_ann_overload(&mut tz),
        "vararg" => {
            let type_expr = parse_type_expr(&mut tz);
            Some(EmmyAnnotation::Vararg { type_expr })
        }
        "deprecated" => Some(EmmyAnnotation::Deprecated { desc: tz.rest_as_string() }),
        "async" => Some(EmmyAnnotation::Async),
        "nodiscard" => Some(EmmyAnnotation::Nodiscard),
        "enum" => {
            let name = tz.eat_name().unwrap_or_default();
            Some(EmmyAnnotation::Enum { name })
        }
        "see" => Some(EmmyAnnotation::See { path: tz.rest_as_string() }),
        "diagnostic" => Some(EmmyAnnotation::Diagnostic { text: tz.rest_as_string() }),
        "meta" => {
            // `---@meta` or `---@meta <module_name>`. Anything after
            // the first name token is ignored (no semantic role).
            let name = tz.eat_name();
            Some(EmmyAnnotation::Meta { name })
        }
        _ => Some(EmmyAnnotation::Other { tag, text: tz.rest_as_string() }),
    }
}

/// `@class Name [ ':' Parent { ',' Parent } ] [ desc ]`
fn parse_ann_class(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let name = tz.eat_name()?;
    let parents = if tz.eat(&Token::Colon) {
        let mut ps = Vec::new();
        if let Some(p) = tz.eat_name() { ps.push(p); }
        while tz.eat(&Token::Comma) {
            if let Some(p) = tz.eat_name() { ps.push(p); }
        }
        ps
    } else {
        vec![]
    };
    let desc = tz.rest_as_string();
    Some(EmmyAnnotation::Class { name, parents, desc })
}

/// `@field [visibility] field_key type_expr [desc]`
fn parse_ann_field(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let visibility = match tz.peek() {
        Token::Name(s) if matches!(s.as_str(), "public" | "protected" | "private" | "package") => {
            let v = s.clone();
            tz.advance();
            Some(v)
        }
        _ => None,
    };

    // field_key: Name or `[type_expr]`
    let name = match tz.peek() {
        Token::LBracket => {
            tz.advance();
            let key_type = parse_type_expr(tz);
            tz.eat(&Token::RBracket);
            format!("[{}]", key_type)
        }
        Token::Name(_) => tz.eat_name().unwrap_or_default(),
        _ => return None,
    };

    let type_expr = parse_type_expr(tz);
    let desc = tz.rest_as_string();
    Some(EmmyAnnotation::Field { visibility, name, type_expr, desc })
}

/// `@param param_name ['?'] type_expr [desc]`
fn parse_ann_param(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let name = if matches!(tz.peek(), Token::Ellipsis) {
        tz.advance();
        "...".to_string()
    } else {
        tz.eat_name()?
    };
    let optional = tz.eat(&Token::Question);
    let type_expr = parse_type_expr(tz);
    let desc = tz.rest_as_string();
    Some(EmmyAnnotation::Param { name, optional, type_expr, desc })
}

/// `@return type_list [Name] [desc]`
fn parse_ann_return(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let return_types = parse_type_list(tz);

    // After the type list, check for an optional return name.
    // A Name here is the return name if it's not followed by more type syntax.
    let name = if let Token::Name(ref s) = tz.peek() {
        if !is_type_start_keyword(s) {
            let n = s.clone();
            tz.advance();
            Some(n)
        } else {
            None
        }
    } else {
        None
    };

    let desc = tz.rest_as_string();
    Some(EmmyAnnotation::Return { return_types, name, desc })
}

/// `@type type_expr [desc]`
fn parse_ann_type(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let type_expr = parse_type_expr(tz);
    let desc = tz.rest_as_string();
    Some(EmmyAnnotation::Type { type_expr, desc })
}

/// `@alias Name type_expr`
fn parse_ann_alias(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let name = tz.eat_name()?;
    let type_expr = parse_type_expr(tz);
    Some(EmmyAnnotation::Alias { name, type_expr })
}

/// `@generic Name [':' type_expr] { ',' Name [':' type_expr] }`
fn parse_ann_generic(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let mut params = Vec::new();
    if let Some(name) = tz.eat_name() {
        let constraint = if tz.eat(&Token::Colon) {
            Some(parse_type_expr(tz))
        } else {
            None
        };
        params.push(GenericParam { name, constraint });
        while tz.eat(&Token::Comma) {
            if let Some(name) = tz.eat_name() {
                let constraint = if tz.eat(&Token::Colon) {
                    Some(parse_type_expr(tz))
                } else {
                    None
                };
                params.push(GenericParam { name, constraint });
            }
        }
    }
    Some(EmmyAnnotation::Generic { params })
}

/// `@overload fun_type`
fn parse_ann_overload(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let fun_type = parse_fun_type(tz);
    Some(EmmyAnnotation::Overload { fun_type })
}

fn is_type_start_keyword(s: &str) -> bool {
    matches!(s, "fun" | "nil" | "true" | "false" | "string" | "number"
        | "boolean" | "integer" | "table" | "function" | "any" | "thread" | "userdata")
}

// ===========================================================================
// Tree-sitter comment collection
// ===========================================================================

/// Extract text content from a Lua block comment (`--[[ ... ]]` or `--[=[ ... ]=]`).
fn extract_block_comment_content(text: &str) -> Option<String> {
    let rest = text.strip_prefix("--[")?;
    let eq_count = rest.chars().take_while(|c| *c == '=').count();
    let rest = &rest[eq_count..];
    let rest = rest.strip_prefix('[')?;
    let mut closing = String::from("]");
    for _ in 0..eq_count {
        closing.push('=');
    }
    closing.push(']');
    let content = rest.strip_suffix(&closing)?;
    Some(content.to_string())
}

/// Collect comment lines immediately before a given node.
///
/// Collects all consecutive preceding comment siblings:
/// - `emmy_comment` nodes (`---@xxx` lines)
/// - `---` line comments (Emmy doc lines)
/// - `--[[ ... ]]` block comments (content converted to `--- line` format)
/// - `--` plain line comments (kept as-is for doc display)
pub fn collect_preceding_comments<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
) -> Vec<String> {
    let mut comments = Vec::new();
    let mut sibling = node.prev_sibling();
    // Track the start row of the "next" node (initially the target node itself).
    // If the previous comment's end row is not adjacent (i.e. there is a blank
    // line in between), we stop collecting — the comment block is not contiguous.
    let mut next_start_row = node.start_position().row;

    while let Some(prev) = sibling {
        let prev_end_row = prev.end_position().row;
        // If there is a blank line gap between this comment and the node/comment
        // that follows it, the comment block is not contiguous — stop.
        if next_start_row > prev_end_row + 1 {
            break;
        }
        match prev.kind() {
            "emmy_comment" => {
                let mut lines = Vec::new();
                for i in 0..prev.named_child_count() {
                    if let Some(line_node) = prev.named_child(i as u32) {
                        if line_node.kind() == "emmy_line" {
                            lines.push(line_node.utf8_text(source).unwrap_or("").to_string());
                        }
                    }
                }
                comments.extend(lines.into_iter().rev());
                next_start_row = prev.start_position().row;
                sibling = prev.prev_sibling();
                continue;
            }
            "comment" => {
                let text = prev.utf8_text(source).unwrap_or("");
                if text.starts_with("---") {
                    comments.push(text.to_string());
                    next_start_row = prev.start_position().row;
                    sibling = prev.prev_sibling();
                    continue;
                }
                if let Some(content) = extract_block_comment_content(text) {
                    let block_lines: Vec<String> = content
                        .lines()
                        .map(|l| l.trim())
                        .filter(|l| !l.is_empty())
                        .map(|l| format!("--- {}", l))
                        .collect();
                    comments.extend(block_lines.into_iter().rev());
                    next_start_row = prev.start_position().row;
                    sibling = prev.prev_sibling();
                    continue;
                }
                if text.starts_with("--") {
                    // Check if this comment is actually a trailing comment
                    // of a prior sibling (i.e. on the same line as the
                    // previous non-comment node). If so, it belongs to that
                    // statement, not to the node we are collecting for.
                    if let Some(before) = prev.prev_sibling() {
                        if before.end_position().row == prev.start_position().row {
                            // This comment sits on the same line as `before`,
                            // so it is a trailing comment — stop collecting.
                            break;
                        }
                    }
                    comments.push(text.to_string());
                    next_start_row = prev.start_position().row;
                    sibling = prev.prev_sibling();
                    continue;
                }
            }
            _ => {}
        }
        break;
    }

    comments.reverse();
    comments
}

/// Collect a trailing (end-of-line) comment on the same line as `node`.
///
/// Looks at the next sibling of `node`; if it is a `comment` node whose
/// start row equals `node`'s end row, its text is returned (stripped of
/// the leading `--` prefix and trimmed). This captures patterns like:
///
/// ```lua
/// local x = foo()   -- some doc
/// ```
pub fn collect_trailing_comment<'a>(
    node: tree_sitter::Node<'a>,
    source: &'a [u8],
) -> Option<String> {
    let stmt_end_row = node.end_position().row;
    let next = node.next_sibling()?;
    if next.start_position().row != stmt_end_row {
        return None;
    }
    match next.kind() {
        "comment" => {
            let text = next.utf8_text(source).unwrap_or("");
            // Strip the `--` prefix (but not `---` Emmy lines — those are
            // preceding-comment material, not trailing doc).
            if text.starts_with("---") {
                return None;
            }
            if let Some(rest) = text.strip_prefix("--") {
                let trimmed = rest.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            None
        }
        _ => None,
    }
}

// ===========================================================================
// Hover formatting
// ===========================================================================

/// Format EmmyLua annotations as Markdown for Hover display.
pub fn format_annotations_markdown(annotations: &[EmmyAnnotation]) -> String {
    let mut parts = Vec::new();

    for ann in annotations {
        match ann {
            EmmyAnnotation::Param { name, type_expr, desc, optional } => {
                let opt = if *optional { "?" } else { "" };
                let mut s = format!("@param `{}{}` `{}`", name, opt, type_expr);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Return { return_types, name, desc } => {
                let types_str = return_types.iter()
                    .map(|t| format!("{}", t))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut s = format!("@return `{}`", types_str);
                if let Some(n) = name {
                    s.push_str(&format!(" `{}`", n));
                }
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Type { type_expr, desc } => {
                let mut s = format!("@type `{}`", type_expr);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Class { name, parents, desc } => {
                let mut s = format!("@class `{}`", name);
                if !parents.is_empty() {
                    s.push_str(&format!(" : {}", parents.join(", ")));
                }
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Field { name, type_expr, desc, .. } => {
                let mut s = format!("@field `{}` `{}`", name, type_expr);
                if !desc.is_empty() {
                    s.push_str(&format!(" — {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Deprecated { desc } => {
                let mut s = "@deprecated".to_string();
                if !desc.is_empty() {
                    s.push_str(&format!(" {}", desc));
                }
                parts.push(s);
            }
            EmmyAnnotation::Overload { fun_type } => {
                parts.push(format!("@overload `{}`", fun_type));
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

// ===========================================================================
// EmmyType → TypeFact conversion (replaces emmy_type_text_to_fact)
// ===========================================================================

use crate::table_shape::TableShapeId;
use crate::type_system::{FunctionSignature, KnownType, ParamInfo, TypeFact};

/// Convert a parsed `EmmyType` AST into a `TypeFact`.
pub fn emmy_type_to_fact(ty: &EmmyType) -> TypeFact {
    match ty {
        EmmyType::Named { name, generics } => {
            let base = match name.as_str() {
                "nil" => return TypeFact::Known(KnownType::Nil),
                "boolean" => return TypeFact::Known(KnownType::Boolean),
                "number" => return TypeFact::Known(KnownType::Number),
                "integer" => return TypeFact::Known(KnownType::Integer),
                "string" => return TypeFact::Known(KnownType::String),
                "any" => return TypeFact::Unknown,
                _ => name.clone(),
            };
            if generics.is_empty() {
                if base == "table" {
                    TypeFact::Known(KnownType::Table(TableShapeId(u32::MAX)))
                } else if base == "function" {
                    TypeFact::Known(KnownType::Function(FunctionSignature {
                        params: Vec::new(),
                        returns: Vec::new(),
                    }))
                } else {
                    TypeFact::Known(KnownType::EmmyType(base))
                }
            } else {
                let generic_facts: Vec<TypeFact> = generics.iter().map(emmy_type_to_fact).collect();
                TypeFact::Known(KnownType::EmmyGeneric(base, generic_facts))
            }
        }
        EmmyType::Union(types) => {
            let parts: Vec<TypeFact> = types.iter().map(emmy_type_to_fact).collect();
            if parts.len() == 1 {
                parts.into_iter().next().unwrap()
            } else {
                TypeFact::Union(parts)
            }
        }
        EmmyType::Optional(inner) => {
            TypeFact::Union(vec![
                emmy_type_to_fact(inner),
                TypeFact::Known(KnownType::Nil),
            ])
        }
        EmmyType::Array(_) => {
            TypeFact::Known(KnownType::Table(TableShapeId(u32::MAX)))
        }
        EmmyType::Function { params, returns } => {
            let param_infos: Vec<ParamInfo> = params.iter().map(|p| ParamInfo {
                name: p.name.clone().unwrap_or_default(),
                type_fact: emmy_type_to_fact(&p.type_expr),
            }).collect();
            let ret_facts: Vec<TypeFact> = returns.iter().map(emmy_type_to_fact).collect();
            TypeFact::Known(KnownType::Function(FunctionSignature {
                params: param_infos,
                returns: ret_facts,
            }))
        }
        EmmyType::Table(_) => {
            TypeFact::Known(KnownType::Table(TableShapeId(u32::MAX)))
        }
        EmmyType::Literal(s) => match s.as_str() {
            "nil" => TypeFact::Known(KnownType::Nil),
            "true" | "false" => TypeFact::Known(KnownType::Boolean),
            _ if s.starts_with('"') || s.starts_with('\'') => TypeFact::Known(KnownType::String),
            _ if s.as_bytes().first().is_some_and(|b| b.is_ascii_digit()) => TypeFact::Known(KnownType::Number),
            _ => TypeFact::Unknown,
        },
        EmmyType::Variadic(inner) => emmy_type_to_fact(inner),
        EmmyType::Unknown => TypeFact::Unknown,
    }
}

/// Parse a type expression from a raw string (convenience wrapper).
pub fn parse_type_from_str(input: &str) -> EmmyType {
    let mut tz = Tokenizer::new(input);
    parse_type_expr(&mut tz)
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Type expression parsing --

    #[test]
    fn parse_simple_named() {
        assert_eq!(
            parse_type_from_str("string"),
            EmmyType::Named { name: "string".into(), generics: vec![] }
        );
    }

    #[test]
    fn parse_union() {
        let ty = parse_type_from_str("string|number");
        assert_eq!(ty, EmmyType::Union(vec![
            EmmyType::Named { name: "string".into(), generics: vec![] },
            EmmyType::Named { name: "number".into(), generics: vec![] },
        ]));
    }

    #[test]
    fn parse_union_three() {
        let ty = parse_type_from_str("string|number|nil");
        match ty {
            EmmyType::Union(types) => assert_eq!(types.len(), 3),
            _ => panic!("expected union, got {:?}", ty),
        }
    }

    #[test]
    fn parse_optional() {
        let ty = parse_type_from_str("string?");
        assert_eq!(ty, EmmyType::Optional(Box::new(
            EmmyType::Named { name: "string".into(), generics: vec![] }
        )));
    }

    #[test]
    fn parse_array() {
        let ty = parse_type_from_str("string[]");
        assert_eq!(ty, EmmyType::Array(Box::new(
            EmmyType::Named { name: "string".into(), generics: vec![] }
        )));
    }

    #[test]
    fn parse_array_of_array() {
        let ty = parse_type_from_str("string[][]");
        assert_eq!(ty, EmmyType::Array(Box::new(EmmyType::Array(Box::new(
            EmmyType::Named { name: "string".into(), generics: vec![] }
        )))));
    }

    #[test]
    fn parse_generic() {
        let ty = parse_type_from_str("table<string, number>");
        assert_eq!(ty, EmmyType::Named {
            name: "table".into(),
            generics: vec![
                EmmyType::Named { name: "string".into(), generics: vec![] },
                EmmyType::Named { name: "number".into(), generics: vec![] },
            ],
        });
    }

    #[test]
    fn parse_fun_empty() {
        let ty = parse_type_from_str("fun()");
        assert_eq!(ty, EmmyType::Function { params: vec![], returns: vec![] });
    }

    #[test]
    fn parse_fun_with_params_and_return() {
        let ty = parse_type_from_str("fun(x: string): number");
        match ty {
            EmmyType::Function { params, returns } => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, Some("x".into()));
                assert_eq!(returns.len(), 1);
            }
            _ => panic!("expected function type"),
        }
    }

    #[test]
    fn parse_fun_nested_union_param() {
        let ty = parse_type_from_str("fun(x: string|number): boolean");
        match ty {
            EmmyType::Function { params, returns } => {
                assert_eq!(params.len(), 1);
                assert!(matches!(params[0].type_expr, EmmyType::Union(_)));
                assert_eq!(returns.len(), 1);
            }
            _ => panic!("expected function type"),
        }
    }

    #[test]
    fn parse_table_type() {
        let ty = parse_type_from_str("{name: string, age: number}");
        match ty {
            EmmyType::Table(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].key, EmmyTableFieldKey::Name("name".into()));
                assert_eq!(fields[1].key, EmmyTableFieldKey::Name("age".into()));
            }
            _ => panic!("expected table type"),
        }
    }

    #[test]
    fn parse_table_index_type() {
        let ty = parse_type_from_str("{[string]: number}");
        match ty {
            EmmyType::Table(fields) => {
                assert_eq!(fields.len(), 1);
                assert!(matches!(fields[0].key, EmmyTableFieldKey::IndexType(_)));
            }
            _ => panic!("expected table type"),
        }
    }

    #[test]
    fn parse_paren_grouping() {
        let ty = parse_type_from_str("(string|number)[]");
        match ty {
            EmmyType::Array(inner) => {
                assert!(matches!(*inner, EmmyType::Union(_)));
            }
            _ => panic!("expected array of union"),
        }
    }

    #[test]
    fn parse_variadic() {
        let ty = parse_type_from_str("...string");
        assert_eq!(ty, EmmyType::Variadic(Box::new(
            EmmyType::Named { name: "string".into(), generics: vec![] }
        )));
    }

    #[test]
    fn parse_literal_types() {
        assert_eq!(parse_type_from_str("nil"), EmmyType::Literal("nil".into()));
        assert_eq!(parse_type_from_str("true"), EmmyType::Literal("true".into()));
        assert_eq!(parse_type_from_str("false"), EmmyType::Literal("false".into()));
    }

    #[test]
    fn parse_string_literal_type() {
        let ty = parse_type_from_str("\"hello\"");
        assert_eq!(ty, EmmyType::Literal("\"hello\"".into()));
    }

    #[test]
    fn parse_unknown_on_empty() {
        assert_eq!(parse_type_from_str(""), EmmyType::Unknown);
    }

    // -- Display --

    #[test]
    fn display_named() {
        assert_eq!(format!("{}", parse_type_from_str("string")), "string");
    }

    #[test]
    fn display_generic() {
        assert_eq!(format!("{}", parse_type_from_str("table<string, number>")), "table<string, number>");
    }

    #[test]
    fn display_union() {
        assert_eq!(format!("{}", parse_type_from_str("string|number")), "string|number");
    }

    #[test]
    fn display_optional() {
        assert_eq!(format!("{}", parse_type_from_str("string?")), "string?");
    }

    #[test]
    fn display_array() {
        assert_eq!(format!("{}", parse_type_from_str("string[]")), "string[]");
    }

    #[test]
    fn display_fun() {
        assert_eq!(
            format!("{}", parse_type_from_str("fun(x: string): number")),
            "fun(x: string):number"
        );
    }

    #[test]
    fn display_table() {
        assert_eq!(
            format!("{}", parse_type_from_str("{name: string, age: number}")),
            "{name: string, age: number}"
        );
    }

    // -- Full annotation parsing --

    #[test]
    fn annotation_param() {
        let anns = parse_emmy_comments("---@param name string|number some desc");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Param { name, type_expr, desc, .. } => {
                assert_eq!(name, "name");
                assert!(matches!(type_expr, EmmyType::Union(_)));
                assert_eq!(desc, "some desc");
            }
            _ => panic!("expected Param"),
        }
    }

    #[test]
    fn annotation_return() {
        let anns = parse_emmy_comments("---@return string|nil");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Return { return_types, .. } => {
                assert_eq!(return_types.len(), 1);
                assert!(matches!(&return_types[0], EmmyType::Union(_)));
            }
            _ => panic!("expected Return"),
        }
    }

    #[test]
    fn annotation_class_with_parent() {
        let anns = parse_emmy_comments("---@class Foo : Bar, Baz");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Class { name, parents, .. } => {
                assert_eq!(name, "Foo");
                assert_eq!(parents, &["Bar", "Baz"]);
            }
            _ => panic!("expected Class"),
        }
    }

    #[test]
    fn annotation_field_with_type() {
        let anns = parse_emmy_comments("---@field name string some desc");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { name, type_expr, desc, .. } => {
                assert_eq!(name, "name");
                assert_eq!(*type_expr, EmmyType::Named { name: "string".into(), generics: vec![] });
                assert_eq!(desc, "some desc");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_type() {
        let anns = parse_emmy_comments("---@type table<string, number>");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Type { type_expr, .. } => {
                assert!(matches!(type_expr, EmmyType::Named { name, generics } if name == "table" && generics.len() == 2));
            }
            _ => panic!("expected Type"),
        }
    }

    #[test]
    fn annotation_generic() {
        let anns = parse_emmy_comments("---@generic T: table, K");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Generic { params } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "T");
                assert!(params[0].constraint.is_some());
                assert_eq!(params[1].name, "K");
                assert!(params[1].constraint.is_none());
            }
            _ => panic!("expected Generic"),
        }
    }

    #[test]
    fn error_recovery_malformed() {
        // Should not panic; produces some annotation (possibly with Unknown type)
        let anns = parse_emmy_comments("---@param ???");
        assert!(anns.is_empty() || matches!(&anns[0], EmmyAnnotation::Param { .. }));
    }

    #[test]
    fn error_recovery_empty_type() {
        let anns = parse_emmy_comments("---@type");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Type { type_expr, .. } => {
                assert_eq!(*type_expr, EmmyType::Unknown);
            }
            _ => panic!("expected Type"),
        }
    }

    // -- Annotation: variadic param --

    #[test]
    fn annotation_param_variadic() {
        let anns = parse_emmy_comments("---@param ... number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Param { name, type_expr, .. } => {
                assert_eq!(name, "...");
                assert_eq!(*type_expr, EmmyType::Named { name: "number".into(), generics: vec![] });
            }
            _ => panic!("expected Param"),
        }
    }

    // -- Annotation: field with bracket key --

    #[test]
    fn annotation_field_bracket_key() {
        let anns = parse_emmy_comments("---@field [string] number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { name, type_expr, .. } => {
                assert_eq!(name, "[string]");
                assert_eq!(*type_expr, EmmyType::Named { name: "number".into(), generics: vec![] });
            }
            _ => panic!("expected Field"),
        }
    }

    // -- Annotation: field with visibility --

    #[test]
    fn annotation_field_visibility() {
        let anns = parse_emmy_comments("---@field public name string");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { visibility, name, type_expr, .. } => {
                assert_eq!(visibility.as_deref(), Some("public"));
                assert_eq!(name, "name");
                assert_eq!(*type_expr, EmmyType::Named { name: "string".into(), generics: vec![] });
            }
            _ => panic!("expected Field"),
        }
    }

    // -- Annotation: overload --

    #[test]
    fn annotation_overload() {
        let anns = parse_emmy_comments("---@overload fun(x: string): number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Overload { fun_type } => {
                match fun_type {
                    EmmyType::Function { params, returns } => {
                        assert_eq!(params.len(), 1);
                        assert_eq!(returns.len(), 1);
                    }
                    _ => panic!("expected function type in overload"),
                }
            }
            _ => panic!("expected Overload"),
        }
    }

    // -- Annotation: multi-return --

    #[test]
    fn annotation_return_multiple() {
        let anns = parse_emmy_comments("---@return string, number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Return { return_types, .. } => {
                assert_eq!(return_types.len(), 2);
                assert_eq!(return_types[0], EmmyType::Named { name: "string".into(), generics: vec![] });
                assert_eq!(return_types[1], EmmyType::Named { name: "number".into(), generics: vec![] });
            }
            _ => panic!("expected Return"),
        }
    }

    // -- Annotation: param optional marker --

    #[test]
    fn annotation_param_optional() {
        let anns = parse_emmy_comments("---@param name? string");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Param { name, optional, type_expr, .. } => {
                assert_eq!(name, "name");
                assert!(*optional);
                assert_eq!(*type_expr, EmmyType::Named { name: "string".into(), generics: vec![] });
            }
            _ => panic!("expected Param"),
        }
    }

    // -- Annotation: alias --

    #[test]
    fn annotation_alias() {
        let anns = parse_emmy_comments("---@alias MyType string|number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Alias { name, type_expr } => {
                assert_eq!(name, "MyType");
                assert!(matches!(type_expr, EmmyType::Union(_)));
            }
            _ => panic!("expected Alias"),
        }
    }

    // -- Annotation: deprecated / async / nodiscard --

    #[test]
    fn annotation_deprecated() {
        let anns = parse_emmy_comments("---@deprecated Use newFunc instead");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Deprecated { desc } => {
                assert_eq!(desc, "Use newFunc instead");
            }
            _ => panic!("expected Deprecated"),
        }
    }

    #[test]
    fn annotation_async() {
        let anns = parse_emmy_comments("---@async");
        assert_eq!(anns.len(), 1);
        assert!(matches!(&anns[0], EmmyAnnotation::Async));
    }

    #[test]
    fn annotation_nodiscard() {
        let anns = parse_emmy_comments("---@nodiscard");
        assert_eq!(anns.len(), 1);
        assert!(matches!(&anns[0], EmmyAnnotation::Nodiscard));
    }

    // -- Type: number literal --

    #[test]
    fn parse_number_literal() {
        let ty = parse_type_from_str("42");
        assert_eq!(ty, EmmyType::Literal("42".into()));
    }

    // -- Type: fun with multiple params --

    #[test]
    fn parse_fun_multi_params() {
        let ty = parse_type_from_str("fun(a: string, b: number): boolean");
        match ty {
            EmmyType::Function { params, returns } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, Some("a".into()));
                assert_eq!(params[1].name, Some("b".into()));
                assert_eq!(returns.len(), 1);
            }
            _ => panic!("expected function type"),
        }
    }

    // -- Type: fun with variadic param --

    #[test]
    fn parse_fun_variadic_param() {
        let ty = parse_type_from_str("fun(...: string)");
        match ty {
            EmmyType::Function { params, .. } => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, Some("...".into()));
                assert!(matches!(params[0].type_expr, EmmyType::Variadic(_)));
            }
            _ => panic!("expected function type"),
        }
    }

    // -- Type: fun with positional (unnamed) params --

    #[test]
    fn parse_fun_positional_params() {
        let ty = parse_type_from_str("fun(string, number): boolean");
        match ty {
            EmmyType::Function { params, returns } => {
                assert_eq!(params.len(), 2);
                assert!(params[0].name.is_none());
                assert!(params[1].name.is_none());
                assert_eq!(returns.len(), 1);
            }
            _ => panic!("expected function type"),
        }
    }

    // -- Multi-line annotation block --

    #[test]
    fn multi_line_annotation_block() {
        let text = "---@class Foo\n---@field x number\n---@field y string";
        let anns = parse_emmy_comments(text);
        assert_eq!(anns.len(), 3);
        assert!(matches!(&anns[0], EmmyAnnotation::Class { name, .. } if name == "Foo"));
        assert!(matches!(&anns[1], EmmyAnnotation::Field { name, .. } if name == "x"));
        assert!(matches!(&anns[2], EmmyAnnotation::Field { name, .. } if name == "y"));
    }

    // -- Type to fact: string literal --

    #[test]
    fn type_to_fact_string_literal() {
        let ty = parse_type_from_str("\"hello\"");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Known(KnownType::String));
    }

    // -- emmy_type_to_fact --

    #[test]
    fn type_to_fact_simple() {
        let ty = parse_type_from_str("string");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Known(KnownType::String));
    }

    #[test]
    fn type_to_fact_union() {
        let ty = parse_type_from_str("string|number");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Union(vec![
            TypeFact::Known(KnownType::String),
            TypeFact::Known(KnownType::Number),
        ]));
    }

    #[test]
    fn type_to_fact_optional() {
        let ty = parse_type_from_str("string?");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Union(vec![
            TypeFact::Known(KnownType::String),
            TypeFact::Known(KnownType::Nil),
        ]));
    }

    #[test]
    fn type_to_fact_number_literal() {
        let ty = parse_type_from_str("42");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Known(KnownType::Number));
    }

    #[test]
    fn type_to_fact_emmy_named() {
        let ty = parse_type_from_str("MyClass");
        let fact = emmy_type_to_fact(&ty);
        assert_eq!(fact, TypeFact::Known(KnownType::EmmyType("MyClass".into())));
    }

    #[test]
    fn type_to_fact_function() {
        let ty = parse_type_from_str("fun(x: string): number");
        let fact = emmy_type_to_fact(&ty);
        match fact {
            TypeFact::Known(KnownType::Function(sig)) => {
                assert_eq!(sig.params.len(), 1);
                assert_eq!(sig.returns.len(), 1);
            }
            _ => panic!("expected function type fact"),
        }
    }

    // -- Block comment content extraction --

    #[test]
    fn extract_block_comment_simple() {
        let content = extract_block_comment_content("--[[ hello world ]]");
        assert_eq!(content, Some(" hello world ".to_string()));
    }

    #[test]
    fn extract_block_comment_multiline() {
        let text = "--[[\nMisc System Library\n]]";
        let content = extract_block_comment_content(text).unwrap();
        let lines: Vec<&str> = content.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["Misc System Library"]);
    }

    #[test]
    fn extract_block_comment_with_equals() {
        let content = extract_block_comment_content("--[=[ some text ]=]");
        assert_eq!(content, Some(" some text ".to_string()));
    }

    #[test]
    fn extract_block_comment_double_equals() {
        let content = extract_block_comment_content("--[==[ text ]==]");
        assert_eq!(content, Some(" text ".to_string()));
    }

    #[test]
    fn extract_block_comment_not_block() {
        assert_eq!(extract_block_comment_content("-- regular comment"), None);
        assert_eq!(extract_block_comment_content("--- emmy comment"), None);
    }

    // -- Description capture: non-ASCII + separators --

    #[test]
    fn annotation_field_cjk_desc_with_at_separator() {
        // Regression: previously the tokenizer skipped non-ASCII bytes and
        // `rest_as_string` rejoined tokens with spaces, so CJK chars were
        // lost and the literal `@` separator leaked into the desc.
        let anns = parse_emmy_comments("---@field enabled boolean @ 是否启用审计 11");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { name, desc, .. } => {
                assert_eq!(name, "enabled");
                assert_eq!(desc, "是否启用审计 11");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_field_cjk_desc_no_separator() {
        let anns = parse_emmy_comments("---@field name string 名字字段");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { desc, .. } => {
                assert_eq!(desc, "名字字段");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_field_hash_separator() {
        // EmmyLua / some tools also accept `#` as desc separator.
        let anns = parse_emmy_comments("---@field x number # user id");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { desc, .. } => {
                assert_eq!(desc, "user id");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_field_second_at_preserved() {
        // Only ONE leading separator should be stripped; a literal `@` inside
        // the desc body must survive.
        let anns = parse_emmy_comments("---@field x string @ email like a@b.com");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { desc, .. } => {
                assert_eq!(desc, "email like a@b.com");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_param_cjk_desc() {
        let anns = parse_emmy_comments("---@param action string @ 动作名称");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Param { name, desc, .. } => {
                assert_eq!(name, "action");
                assert_eq!(desc, "动作名称");
            }
            _ => panic!("expected Param"),
        }
    }

    #[test]
    fn annotation_type_emoji_desc() {
        let anns = parse_emmy_comments("---@type number @ count 🚀 rockets");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Type { desc, .. } => {
                assert_eq!(desc, "count 🚀 rockets");
            }
            _ => panic!("expected Type"),
        }
    }

    #[test]
    fn annotation_field_empty_desc() {
        let anns = parse_emmy_comments("---@field x number");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { desc, .. } => {
                assert_eq!(desc, "");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_field_desc_stops_at_trailing_lua_comment() {
        // A trailing `-- ...` inside the same line should be treated as a Lua
        // comment and excluded from the desc.
        let anns = parse_emmy_comments("---@field x string foo -- trailing");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Field { desc, .. } => {
                assert_eq!(desc, "foo");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn annotation_class_cjk_desc() {
        let anns = parse_emmy_comments("---@class Audit @ 审计 helper：全局 class");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::Class { name, desc, .. } => {
                assert_eq!(name, "Audit");
                assert_eq!(desc, "审计 helper：全局 class");
            }
            _ => panic!("expected Class"),
        }
    }
}
