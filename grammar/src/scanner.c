#include "tree_sitter/parser.h"
#include <string.h>
#include <stdbool.h>

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

/* Unified word scanner: handles ALL tokens starting with [a-zA-Z_].
   Reads the full identifier at the current position into a buffer,
   then matches it against the keyword table.

   - If it matches a keyword at column 0 with a top variant: emit TOP_WORD_*.
   - If it matches a keyword at other columns (or no top variant): emit WORD_*.
   - If it does not match any keyword: emit IDENTIFIER.

   This ensures the external scanner fully owns all identifier-like tokens,
   so tree-sitter never falls back to the grammar's built-in identifier regex
   or inline keyword strings. */
static bool scan_word(TSLexer *lexer) {
  if (lexer->eof(lexer)) return false;
  int32_t first = lexer->lookahead;
  if (!((first >= 'a' && first <= 'z') || (first >= 'A' && first <= 'Z') || first == '_')) return false;

  lexer->mark_end(lexer);

  /* Read the full identifier into a buffer.
     We use a fixed-size buffer; identifiers longer than the buffer
     are definitely not keywords and will be emitted as IDENTIFIER.
     The buffer is large enough for any Lua keyword (max 8 chars). */
  char buf[64];
  int len = 0;
  while (!lexer->eof(lexer) && is_identifier_char(lexer->lookahead)) {
    if (len < 63) {
      buf[len++] = (char)lexer->lookahead;
    }
    advance(lexer);
  }
  buf[len] = '\0';

  lexer->mark_end(lexer);

  /* Look up in keyword table (only lowercase-starting words can be keywords) */
  if (first >= 'a' && first <= 'z') {
    bool at_col0 = (lexer->get_column(lexer) == len); /* column after reading == len means started at col 0 */
    for (int i = 0; i < keyword_table_size; i++) {
      if (strcmp(buf, keyword_table[i].keyword) == 0) {
        if (at_col0 && keyword_table[i].top_token >= 0) {
          lexer->result_symbol = keyword_table[i].top_token;
        } else {
          lexer->result_symbol = keyword_table[i].normal_token;
        }
        return true;
      }
    }
  }

  /* Not a keyword — emit as IDENTIFIER */
  lexer->result_symbol = IDENTIFIER;
  return true;
}

static bool scan_long_bracket_content(TSLexer *lexer) {
  uint8_t level = 0;
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
      uint8_t close_level = 0;
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
  uint8_t level = 0;
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
      uint8_t close_level = 0;
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
            if (!lexer->eof(lexer)) advance(lexer);
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

/* Try to scan a comment. If it's a --- line AND EMMY_LINE is valid, returns false
   so the caller can try emmy_line instead. */
static bool scan_comment(TSLexer *lexer, bool emmy_valid) {
  if (lexer->lookahead != '-') return false;
  advance(lexer);
  if (lexer->lookahead != '-') return false;
  advance(lexer);

  /* Check for --- (emmy doc comment) */
  if (lexer->lookahead == '-' && emmy_valid) {
    return false; /* Let caller handle as EMMY_LINE */
  }

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

void *tree_sitter_lua_external_scanner_create(void) {
  return NULL;
}

void tree_sitter_lua_external_scanner_destroy(void *payload) {
  (void)payload;
}

unsigned tree_sitter_lua_external_scanner_serialize(void *payload, char *buffer) {
  (void)payload;
  (void)buffer;
  return 0;
}

void tree_sitter_lua_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  (void)payload;
  (void)buffer;
  (void)length;
}

bool tree_sitter_lua_external_scanner_scan(
  void *payload,
  TSLexer *lexer,
  const bool *valid_symbols
) {
  (void)payload;

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
      if (scan_word(lexer)) {
        return true;
      }
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
      if (scan_comment(lexer, false)) {
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
