#include "tree_sitter/parser.h"
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>
#include <assert.h>

enum TokenType {
  LONG_STRING_CONTENT,
  SHEBANG,
  SHORT_STRING_CONTENT_DOUBLE,
  SHORT_STRING_CONTENT_SINGLE,
  COMMENT,
  EMMY_LINE,
  // top-level keywords (column 0 only)
  TOP_WORD_IF,
  TOP_WORD_WHILE,
  TOP_WORD_REPEAT,
  TOP_WORD_FOR,
  TOP_WORD_FUNCTION,
  TOP_WORD_GOTO,
  TOP_WORD_DO,
  TOP_WORD_LOCAL,
  // normal keywords (any column)
  WORD_END,
  WORD_LOCAL,
  WORD_IF,
  WORD_THEN,
  WORD_ELSEIF,
  WORD_ELSE,
  WORD_WHILE,
  WORD_DO,
  WORD_REPEAT,
  WORD_UNTIL,
  WORD_FOR,
  WORD_IN,
  WORD_FUNCTION,
  WORD_GOTO,
  WORD_RETURN,
  WORD_BREAK,
  // expression-level keywords
  WORD_AND,
  WORD_OR,
  WORD_NOT,
  WORD_NIL,
  WORD_TRUE,
  WORD_FALSE,
  // identifier (non-keyword)
  IDENTIFIER,
};

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }
static void skip_ws(TSLexer *lexer) { lexer->advance(lexer, true); }

/* Global default for top_keyword_disabled. Set by the LSP host via
   `mylua_set_top_keyword_default_disabled()` before any parser is
   created. Each scanner instance reads this value once in `create`
   and `deserialize(length==0)`. Individual files can still override
   with `---#enable top_keyword` / `---#disable top_keyword`.
   Default: true (top-level keyword splitting OFF). */
static bool g_top_keyword_default_disabled = true;

/* Public setter — called from Rust via FFI. */
void mylua_set_top_keyword_default_disabled(bool value) {
  g_top_keyword_default_disabled = value;
}

/* Scanner state: persisted across incremental parses via serialize/deserialize.
   Currently tracks whether top-level keyword emission is disabled via
   the ---#disable top_keyword directive. */
typedef struct {
  bool top_keyword_disabled;
} ScannerState;

static bool is_identifier_char(int32_t c) {
  return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
         (c >= '0' && c <= '9') || c == '_';
}

/* Keyword entry: maps a keyword string to its normal and top-level token types.
   top_token == -1 means this keyword has no top-level variant. */
typedef struct {
  const char *keyword;
  int normal_token;
  int top_token;
} KeywordEntry;

static const KeywordEntry keyword_table[] = {
  {"and",      WORD_AND,      -1},
  {"break",    WORD_BREAK,    -1},
  {"do",       WORD_DO,       TOP_WORD_DO},
  {"else",     WORD_ELSE,     -1},
  {"elseif",   WORD_ELSEIF,   -1},
  {"end",      WORD_END,      -1},
  {"false",    WORD_FALSE,    -1},
  {"for",      WORD_FOR,      TOP_WORD_FOR},
  {"function", WORD_FUNCTION, TOP_WORD_FUNCTION},
  {"goto",     WORD_GOTO,     TOP_WORD_GOTO},
  {"if",       WORD_IF,       TOP_WORD_IF},
  {"in",       WORD_IN,       -1},
  {"local",    WORD_LOCAL,    TOP_WORD_LOCAL},
  {"nil",      WORD_NIL,      -1},
  {"not",      WORD_NOT,      -1},
  {"or",       WORD_OR,       -1},
  {"repeat",   WORD_REPEAT,   TOP_WORD_REPEAT},
  {"return",   WORD_RETURN,   -1},
  {"then",     WORD_THEN,     -1},
  {"true",     WORD_TRUE,     -1},
  {"until",    WORD_UNTIL,    -1},
  {"while",    WORD_WHILE,    TOP_WORD_WHILE},
};

static const int keyword_table_size = sizeof(keyword_table) / sizeof(keyword_table[0]);

/* First-character index into keyword_table.
   keyword_index['x' - 'a'] = { start, count } where start is the index
   of the first keyword starting with 'x' and count is how many there are.
   This turns O(22) linear scan into O(1-3) lookups per word. */
typedef struct {
  uint8_t start;
  uint8_t count;
} KeywordIndex;

static const KeywordIndex keyword_index[26] = {
  /* a */ {0,  1},
  /* b */ {1,  1},
  /* c */ {0,  0},
  /* d */ {2,  1},
  /* e */ {3,  3},
  /* f */ {6,  3},
  /* g */ {9,  1},
  /* h */ {0,  0},
  /* i */ {10, 2},
  /* j */ {0,  0},
  /* k */ {0,  0},
  /* l */ {12, 1},
  /* m */ {0,  0},
  /* n */ {13, 2},
  /* o */ {15, 1},
  /* p */ {0,  0},
  /* q */ {0,  0},
  /* r */ {16, 2},
  /* s */ {0,  0},
  /* t */ {18, 2},
  /* u */ {20, 1},
  /* v */ {0,  0},
  /* w */ {21, 1},
  /* x */ {0,  0},
  /* y */ {0,  0},
  /* z */ {0,  0},
};

/* Unified word scanner: handles ALL tokens starting with [a-zA-Z_].
   Reads the full identifier at the current position into a buffer,
   then matches it against the keyword table.

   - If it matches a keyword at column 0 with a top variant: emit TOP_WORD_*.
   - If it matches a keyword at other columns (or no top variant): emit WORD_*.
   - If it does not match any keyword: emit IDENTIFIER.

   This ensures the external scanner fully owns all identifier-like tokens,
   so tree-sitter never falls back to the grammar's built-in identifier regex
   or inline keyword strings. */
/* Max length of any Lua keyword ("function" = 8 chars) */
#define MAX_KEYWORD_LEN 8

/* Precondition: lookahead is [a-zA-Z_] (caller has already checked).
   Always returns true — emits a keyword or IDENTIFIER token. */
static bool scan_word(TSLexer *lexer, ScannerState *state) {
  int32_t first = lexer->lookahead;
  assert(!lexer->eof(lexer));
  assert((first >= 'a' && first <= 'z') || (first >= 'A' && first <= 'Z') || first == '_');

  lexer->mark_end(lexer);

  /* Phase 1: Read up to MAX_KEYWORD_LEN characters into buf.
     This is enough to determine if the token is a keyword. */
  char buf[MAX_KEYWORD_LEN + 1];
  int len = 0;
  while (!lexer->eof(lexer) && is_identifier_char(lexer->lookahead) && len < MAX_KEYWORD_LEN) {
    buf[len++] = (char)lexer->lookahead;
    advance(lexer);
  }
  buf[len] = '\0';

  /* Check if the word continues beyond MAX_KEYWORD_LEN chars.
     If so, it's definitely not a keyword — skip to IDENTIFIER path. */
  bool word_complete = lexer->eof(lexer) || !is_identifier_char(lexer->lookahead);

  /* Phase 2: If the word is complete (≤ MAX_KEYWORD_LEN) and starts with
     a lowercase letter, try matching it against the keyword table.
     Uses first-character index to narrow the search to 1-3 candidates. */
  if (word_complete && first >= 'a' && first <= 'z') {
    const KeywordIndex *idx = &keyword_index[first - 'a'];
    for (int i = idx->start, end = idx->start + idx->count; i < end; i++) {
      if (strcmp(buf, keyword_table[i].keyword) == 0) {
        lexer->mark_end(lexer);
        /* Defer get_column to here: only called on keyword match,
           and only matters when the keyword has a top variant. */
        if (keyword_table[i].top_token >= 0 &&
            lexer->get_column(lexer) == len &&
            !state->top_keyword_disabled) {
          lexer->result_symbol = keyword_table[i].top_token;
        } else {
          lexer->result_symbol = keyword_table[i].normal_token;
        }
        return true;
      }
    }
  }

  /* Phase 3: Not a keyword — consume remaining identifier characters
     and emit as IDENTIFIER. This handles arbitrarily long identifiers
     without needing a large buffer. */
  while (!lexer->eof(lexer) && is_identifier_char(lexer->lookahead)) {
    advance(lexer);
  }

  lexer->mark_end(lexer);
  lexer->result_symbol = IDENTIFIER;
  return true;
}

static bool scan_long_bracket_content(TSLexer *lexer) {
  uint16_t level = 0;
  while (lexer->lookahead == '=') {
    level++;
    advance(lexer);
  }
  if (lexer->lookahead != '[') return false;
  advance(lexer);

  for (;;) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == ']') {
      advance(lexer);
      uint16_t close_level = 0;
      while (lexer->lookahead == '=' && close_level < level) {
        close_level++;
        advance(lexer);
      }
      if (close_level == level && lexer->lookahead == ']') {
        advance(lexer);
        return true;
      }
    } else {
      advance(lexer);
    }
  }
}

static bool scan_long_string_external(TSLexer *lexer) {
  uint16_t level = 0;
  while (lexer->lookahead == '=') {
    level++;
    advance(lexer);
  }
  if (lexer->lookahead != '[') return false;
  advance(lexer);

  for (;;) {
    if (lexer->eof(lexer)) return false;
    if (lexer->lookahead == ']') {
      advance(lexer);
      uint16_t close_level = 0;
      while (lexer->lookahead == '=' && close_level < level) {
        close_level++;
        advance(lexer);
      }
      if (close_level == level && lexer->lookahead == ']') {
        lexer->result_symbol = LONG_STRING_CONTENT;
        return true;
      }
    } else {
      advance(lexer);
    }
  }
}

static bool scan_short_string_content(TSLexer *lexer, char quote) {
  bool has_content = false;
  for (;;) {
    if (lexer->eof(lexer)) {
      lexer->result_symbol = (quote == '"')
        ? SHORT_STRING_CONTENT_DOUBLE
        : SHORT_STRING_CONTENT_SINGLE;
      return has_content;
    }
    int32_t c = lexer->lookahead;
    if (c == quote || c == '\n' || c == '\r') {
      lexer->result_symbol = (quote == '"')
        ? SHORT_STRING_CONTENT_DOUBLE
        : SHORT_STRING_CONTENT_SINGLE;
      return has_content;
    }
    if (c == '\\') {
      has_content = true;
      advance(lexer);
      if (lexer->eof(lexer)) return true;
      c = lexer->lookahead;
      switch (c) {
        case 'a': case 'b': case 'f': case 'n': case 'r':
        case 't': case 'v': case '\\': case '\'': case '"':
          advance(lexer);
          break;
        case '\r':
          advance(lexer);
          if (lexer->lookahead == '\n') advance(lexer);
          break;
        case '\n':
          advance(lexer);
          break;
        case 'x':
          advance(lexer);
          for (int i = 0; i < 2; i++) {
            if (!lexer->eof(lexer) && (
                (lexer->lookahead >= '0' && lexer->lookahead <= '9') ||
                (lexer->lookahead >= 'a' && lexer->lookahead <= 'f') ||
                (lexer->lookahead >= 'A' && lexer->lookahead <= 'F'))) {
              advance(lexer);
            } else {
              break;
            }
          }
          break;
        case 'u':
          advance(lexer);
          if (lexer->lookahead == '{') {
            advance(lexer);
            while (!lexer->eof(lexer) && lexer->lookahead != '}') {
              advance(lexer);
            }
            if (lexer->lookahead == '}') advance(lexer);
          }
          break;
        case 'z':
          advance(lexer);
          while (!lexer->eof(lexer) &&
                 (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
                  lexer->lookahead == '\n' || lexer->lookahead == '\r' ||
                  lexer->lookahead == '\f' || lexer->lookahead == '\v')) {
            advance(lexer);
          }
          break;
        default:
          if (c >= '0' && c <= '9') {
            advance(lexer);
            for (int i = 0; i < 2; i++) {
              if (!lexer->eof(lexer) && lexer->lookahead >= '0' && lexer->lookahead <= '9') {
                advance(lexer);
              } else {
                break;
              }
            }
          } else {
            advance(lexer);
          }
          break;
      }
    } else {
      has_content = true;
      advance(lexer);
    }
  }
}

/* Scan a plain comment (not emmy). Called only when EMMY_LINE is not valid. */
static bool scan_comment(TSLexer *lexer) {
  if (lexer->lookahead != '-') return false;
  advance(lexer);
  if (lexer->lookahead != '-') return false;
  advance(lexer);

  /* Try long comment: --[=*[ ... ]=*] */
  if (lexer->lookahead == '[') {
    advance(lexer);
    if (scan_long_bracket_content(lexer)) {
      lexer->result_symbol = COMMENT;
      return true;
    }
    while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
      advance(lexer);
    }
    lexer->result_symbol = COMMENT;
    return true;
  }

  /* Short comment (including --- when emmy not valid): consume to EOL */
  while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
    advance(lexer);
  }
  lexer->result_symbol = COMMENT;
  return true;
}

/* Check if the emmy line content (after ---) matches a directive.
   Called with the lexer positioned right after the third dash.
   Peeks ahead to detect #disable/#enable top_keyword directives.
   Returns true if a directive was found and processed.
   Note: The caller always consumes to EOL after calling this function,
   so partial advances on non-matching directives are harmless. */
static bool check_directive(TSLexer *lexer, ScannerState *state) {
  /* Expected pattern: #disable top_keyword  or  #enable top_keyword
     The lexer is positioned after '---', so we expect optional spaces
     then '#disable' or '#enable', then ' top_keyword'. */

  /* Skip optional spaces after --- */
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    advance(lexer);
  }

  if (lexer->lookahead != '#') return false;
  advance(lexer); /* consume '#' */

  /* Try to match 'disable' or 'enable' */
  const char *disable_str = "disable";
  const char *enable_str = "enable";
  char cmd_buf[8]; /* max("disable","enable") = 7 chars + NUL */
  int cmd_len = 0;

  while (cmd_len < 7 && !lexer->eof(lexer) &&
         lexer->lookahead >= 'a' && lexer->lookahead <= 'z') {
    cmd_buf[cmd_len++] = (char)lexer->lookahead;
    advance(lexer);
  }
  cmd_buf[cmd_len] = '\0';

  bool is_disable = (strcmp(cmd_buf, disable_str) == 0);
  bool is_enable  = (strcmp(cmd_buf, enable_str) == 0);
  if (!is_disable && !is_enable) return false;

  /* Expect at least one space */
  if (lexer->lookahead != ' ' && lexer->lookahead != '\t') return false;
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    advance(lexer);
  }

  /* Match 'top_keyword' */
  const char *target = "top_keyword";
  int target_len = 11;
  for (int i = 0; i < target_len; i++) {
    if (lexer->eof(lexer) || lexer->lookahead != target[i]) return false;
    advance(lexer);
  }

  /* Ensure the token ends here (EOL, EOF, or whitespace) */
  if (!lexer->eof(lexer) && lexer->lookahead != '\n' &&
      lexer->lookahead != '\r' && lexer->lookahead != ' ' &&
      lexer->lookahead != '\t') {
    return false;
  }

  /* Apply the directive */
  state->top_keyword_disabled = is_disable;
  return true;
}

void *tree_sitter_lua_external_scanner_create(void) {
  ScannerState *state = calloc(1, sizeof(ScannerState));
  state->top_keyword_disabled = g_top_keyword_default_disabled;
  return state;
}

void tree_sitter_lua_external_scanner_destroy(void *payload) {
  free(payload);
}

unsigned tree_sitter_lua_external_scanner_serialize(void *payload, char *buffer) {
  ScannerState *state = (ScannerState *)payload;
  buffer[0] = state->top_keyword_disabled ? 1 : 0;
  return 1;
}

void tree_sitter_lua_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  ScannerState *state = (ScannerState *)payload;
  if (length >= 1) {
    state->top_keyword_disabled = (buffer[0] != 0);
  } else {
    state->top_keyword_disabled = g_top_keyword_default_disabled;
  }
}

bool tree_sitter_lua_external_scanner_scan(
  void *payload,
  TSLexer *lexer,
  const bool *valid_symbols
) {
  ScannerState *state = (ScannerState *)payload;

  /* Shebang: only at the very start of the file */
  if (valid_symbols[SHEBANG] && lexer->get_column(lexer) == 0) {
    if (lexer->lookahead == '#') {
      advance(lexer);
      if (lexer->lookahead == '!') {
        advance(lexer);
        while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
          advance(lexer);
        }
        lexer->result_symbol = SHEBANG;
        return true;
      }
      return false;
    }
  }

  /* Skip whitespace */
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    skip_ws(lexer);
  }

  /* Short string content (check BEFORE the comment/emmy `-` branch):
     inside a `"..."` / `'...'` string literal, a leading `-` is just
     a string character. */
  if (valid_symbols[SHORT_STRING_CONTENT_DOUBLE] && lexer->lookahead != '"') {
    return scan_short_string_content(lexer, '"');
  }
  if (valid_symbols[SHORT_STRING_CONTENT_SINGLE] && lexer->lookahead != '\'') {
    return scan_short_string_content(lexer, '\'');
  }

  /* Word scanning (keywords + identifiers, unconditional).
     All tokens starting with [a-zA-Z_] are owned by the external scanner.
     Keywords emit WORD_* or TOP_WORD_*, non-keywords emit IDENTIFIER.
     Must come after short-string content to avoid scanning keywords
     inside strings. */
  {
    int32_t c = lexer->lookahead;
    if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_') {
      scan_word(lexer, state);
      return true;
    }
  }

  /* EmmyLua line or Comment.
     Only enter this branch when the parser is actually expecting a
     comment or emmy line at the current state. */
  if (lexer->lookahead == '-' && (valid_symbols[EMMY_LINE] || valid_symbols[COMMENT])) {
    /* Peek ahead: is this --- ? */
    lexer->mark_end(lexer);

    /* Check if --- and EMMY_LINE is valid */
    if (valid_symbols[EMMY_LINE]) {
      /* Peek: need at least three dashes */
      advance(lexer);
      if (lexer->lookahead == '-') {
        advance(lexer);
        if (lexer->lookahead == '-') {
          /* This is ---... : emit as EMMY_LINE.
             We've consumed '--', now the third '-' is lookahead.
             Reset and re-scan cleanly. */
          lexer->mark_end(lexer); /* back to after '--' */
          /* Actually let's just consume the rest as emmy */
          advance(lexer); /* consume third '-' */
          /* Check for ---#disable/#enable top_keyword directive */
          check_directive(lexer, state);
          while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
            advance(lexer);
          }
          lexer->result_symbol = EMMY_LINE;
          lexer->mark_end(lexer);
          return true;
        }
        /* Consumed '--' (regular comment start, not emmy). Fall through
           to the COMMENT branch below to finish scanning the comment. */
        if (valid_symbols[COMMENT]) {
          /* We already consumed '--', finish as comment */
          if (lexer->lookahead == '[') {
            advance(lexer);
            if (scan_long_bracket_content(lexer)) {
              lexer->result_symbol = COMMENT;
              lexer->mark_end(lexer);
              return true;
            }
            while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
              advance(lexer);
            }
            lexer->result_symbol = COMMENT;
            lexer->mark_end(lexer);
            return true;
          }
          while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
            advance(lexer);
          }
          lexer->result_symbol = COMMENT;
          lexer->mark_end(lexer);
          return true;
        }
        return false;
      }
      /* Only one `-` consumed (no second dash) — this is the minus
         operator, not a comment. A single `-` must NEVER be classified
         as COMMENT; Lua requires `--` to start a comment.
         Return false so the parser's built-in `-` token rule can claim
         this character as the binary/unary minus operator. */
      return false;
    }

    /* EMMY_LINE not valid, try as plain COMMENT */
    if (valid_symbols[COMMENT]) {
      if (scan_comment(lexer)) {
        lexer->mark_end(lexer);
        return true;
      }
    }
    return false;
  }

  /* Long string content */
  if (valid_symbols[LONG_STRING_CONTENT]) {
    return scan_long_string_external(lexer);
  }

  /* Short-string content is handled at the top of this function,
     before the comment/emmy `-` branch, so a string starting with `-`
     scans correctly. */

  return false;
}
