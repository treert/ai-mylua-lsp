#include "tree_sitter/parser.h"
#include <string.h>
#include <stdbool.h>

enum TokenType {
  LONG_STRING_CONTENT,
  SHEBANG,
  SHORT_STRING_CONTENT_DOUBLE,
  SHORT_STRING_CONTENT_SINGLE,
  COMMENT,
  COL0_BLOCK_END,
  EMMY_LINE,
};

static void advance(TSLexer *lexer) { lexer->advance(lexer, false); }
static void skip_ws(TSLexer *lexer) { lexer->advance(lexer, true); }

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

/* Scan a --- line. Returns true if this is an emmy doc comment (---...).
   Consumes '---' + rest of line. */
static bool scan_emmy_line(TSLexer *lexer) {
  /* We're positioned at the first '-'. Consume '---'. */
  advance(lexer); /* - */
  advance(lexer); /* - */
  advance(lexer); /* - */
  /* Consume rest of line */
  while (!lexer->eof(lexer) && lexer->lookahead != '\n' && lexer->lookahead != '\r') {
    advance(lexer);
  }
  lexer->result_symbol = EMMY_LINE;
  lexer->mark_end(lexer);
  return true;
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

  /* Column-0 block end */
  if (valid_symbols[COL0_BLOCK_END] && lexer->get_column(lexer) == 0) {
    int32_t c = lexer->lookahead;
    bool is_stmt_start = (c >= 'a' && c <= 'z')
                      || (c >= 'A' && c <= 'Z')
                      || c == '_'
                      || c == ':';
    if (is_stmt_start) {
      lexer->result_symbol = COL0_BLOCK_END;
      return true;
    }
  }

  /* Skip whitespace */
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' ||
         lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    skip_ws(lexer);
  }

  /* EmmyLua line or Comment */
  if (lexer->lookahead == '-') {
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
      }
      /* Not ---; fall through. We consumed 1-2 dashes already.
         Continue as regular comment. */
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

  /* Short string content */
  if (valid_symbols[SHORT_STRING_CONTENT_DOUBLE] && lexer->lookahead != '"') {
    return scan_short_string_content(lexer, '"');
  }
  if (valid_symbols[SHORT_STRING_CONTENT_SINGLE] && lexer->lookahead != '\'') {
    return scan_short_string_content(lexer, '\'');
  }

  return false;
}
