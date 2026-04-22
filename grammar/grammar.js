/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Lua operator precedence (lowest to highest, per Lua 5.4 §3.4.8)
const PREC = {
  OR: 1,
  AND: 2,
  COMPARE: 3,
  BIT_OR: 4,
  BIT_XOR: 5,
  BIT_AND: 6,
  SHIFT: 7,
  CONCAT: 8,
  ADD: 9,
  MUL: 10,
  UNARY: 11,
  POWER: 12,
};

// EmmyLua type expression precedence
const EMMY_PREC = {
  UNION: 1,
  OPTIONAL: 2,
  ARRAY: 3,
};

module.exports = grammar({
  name: 'lua',

  externals: $ => [
    $.long_string_content,
    $.shebang,
    $.short_string_content_double,
    $.short_string_content_single,
    $.comment,
    $.emmy_line,
    // top-level keywords (column 0 only)
    $.top_word_if,
    $.top_word_while,
    $.top_word_repeat,
    $.top_word_for,
    $.top_word_function,
    $.top_word_goto,
    $.top_word_do,
    $.top_word_local,
    // normal keywords (any column)
    $.word_end,
    $.word_local,
    $.word_if,
    $.word_then,
    $.word_elseif,
    $.word_else,
    $.word_while,
    $.word_do,
    $.word_repeat,
    $.word_until,
    $.word_for,
    $.word_in,
    $.word_function,
    $.word_goto,
    $.word_return,
    $.word_break,
    // expression-level keywords
    $.word_and,
    $.word_or,
    $.word_not,
    $.word_nil,
    $.word_true,
    $.word_false,
    // identifier (non-keyword)
    $.identifier,
  ],

  extras: $ => [
    /\s+/,
    $.comment,
  ],

  conflicts: $ => [
    [$.function_call_statement, $._prefix_expression],
    [$._primary_expression, $.function_call],
  ],

  supertypes: $ => [
    $._statement,
    $._expression,
  ],

  word: $ => $.identifier,

  rules: {
    // ========================================================================
    // 2.1  Program structure
    // ========================================================================

    source_file: $ => seq(
      optional($.shebang),
      optional($._top_block),
    ),

    _top_block: $ => choice(
      seq(repeat1(choice($._top_statement, $._statement)), optional($.return_statement)),
      $.return_statement,
    ),

    _block: $ => choice(
      seq(repeat1($._statement), optional($.return_statement)),
      $.return_statement,
    ),

    // ========================================================================
    // 2.2  Statements
    // ========================================================================

    _top_statement: $ => choice(
      alias($._top_goto_statement, $.goto_statement),
      alias($._top_do_statement, $.do_statement),
      alias($._top_while_statement, $.while_statement),
      alias($._top_repeat_statement, $.repeat_statement),
      alias($._top_if_statement, $.if_statement),
      alias($._top_for_numeric_statement, $.for_numeric_statement),
      alias($._top_for_generic_statement, $.for_generic_statement),
      alias($._top_function_declaration, $.function_declaration),
      alias($._top_local_function_declaration, $.local_function_declaration),
      alias($._top_local_declaration, $.local_declaration),
    ),

    _statement: $ => choice(
      ';',
      $.assignment_statement,
      $.function_call_statement,
      $.label_statement,
      $.break_statement,
      alias($._goto_statement, $.goto_statement),
      alias($._do_statement, $.do_statement),
      alias($._while_statement, $.while_statement),
      alias($._repeat_statement, $.repeat_statement),
      alias($._if_statement, $.if_statement),
      alias($._for_numeric_statement, $.for_numeric_statement),
      alias($._for_generic_statement, $.for_generic_statement),
      alias($._function_declaration, $.function_declaration),
      alias($._local_function_declaration, $.local_function_declaration),
      alias($._local_declaration, $.local_declaration),
      $.emmy_comment,
    ),

    emmy_comment: $ => prec.left(repeat1($.emmy_line)),

    assignment_statement: $ => seq(
      field('left', $.variable_list),
      '=',
      field('right', $.expression_list),
    ),

    function_call_statement: $ => $.function_call,

    label_statement: $ => seq('::', field('name', $.identifier), '::'),

    break_statement: $ => $.word_break,

    // -- goto (top / nested) --
    _top_goto_statement: $ => seq($.top_word_goto, field('name', $.identifier)),
    _goto_statement: $ => seq($.word_goto, field('name', $.identifier)),

    // -- do (top / nested) --
    _top_do_statement: $ => seq($.top_word_do, optional($._block), $.word_end),
    _do_statement: $ => seq($.word_do, optional($._block), $.word_end),

    // -- while (top / nested) --
    _top_while_statement: $ => seq(
      $.top_word_while, field('condition', $._expression),
      $.word_do, optional($._block), $.word_end,
    ),
    _while_statement: $ => seq(
      $.word_while, field('condition', $._expression),
      $.word_do, optional($._block), $.word_end,
    ),

    // -- repeat (top / nested) --
    _top_repeat_statement: $ => seq(
      $.top_word_repeat, optional($._block),
      $.word_until, field('condition', $._expression),
    ),
    _repeat_statement: $ => seq(
      $.word_repeat, optional($._block),
      $.word_until, field('condition', $._expression),
    ),

    // -- if (top / nested) --
    _top_if_statement: $ => seq(
      $.top_word_if, field('condition', $._expression), $.word_then, optional($._block),
      repeat($.elseif_clause),
      optional($.else_clause),
      $.word_end,
    ),
    _if_statement: $ => seq(
      $.word_if, field('condition', $._expression), $.word_then, optional($._block),
      repeat($.elseif_clause),
      optional($.else_clause),
      $.word_end,
    ),

    elseif_clause: $ => seq(
      $.word_elseif, field('condition', $._expression), $.word_then, optional($._block),
    ),

    else_clause: $ => seq($.word_else, optional($._block)),

    // -- for numeric (top / nested) --
    _top_for_numeric_statement: $ => seq(
      $.top_word_for, field('name', $.identifier), '=',
      field('start', $._expression), ',',
      field('stop', $._expression),
      optional(seq(',', field('step', $._expression))),
      $.word_do, optional($._block), $.word_end,
    ),
    _for_numeric_statement: $ => seq(
      $.word_for, field('name', $.identifier), '=',
      field('start', $._expression), ',',
      field('stop', $._expression),
      optional(seq(',', field('step', $._expression))),
      $.word_do, optional($._block), $.word_end,
    ),

    // -- for generic (top / nested) --
    _top_for_generic_statement: $ => seq(
      $.top_word_for, field('names', $.name_list),
      $.word_in, field('values', $.expression_list),
      $.word_do, optional($._block), $.word_end,
    ),
    _for_generic_statement: $ => seq(
      $.word_for, field('names', $.name_list),
      $.word_in, field('values', $.expression_list),
      $.word_do, optional($._block), $.word_end,
    ),

    // -- function declaration (top / nested) --
    _top_function_declaration: $ => seq(
      $.top_word_function, field('name', $.function_name), field('body', $.function_body),
    ),
    _function_declaration: $ => seq(
      $.word_function, field('name', $.function_name), field('body', $.function_body),
    ),

    // -- local function declaration (top / nested) --
    _top_local_function_declaration: $ => seq(
      $.top_word_local, $.word_function, field('name', $.identifier), field('body', $.function_body),
    ),
    _local_function_declaration: $ => seq(
      $.word_local, $.word_function, field('name', $.identifier), field('body', $.function_body),
    ),

    // -- local declaration (top / nested) --
    _top_local_declaration: $ => seq(
      $.top_word_local, field('names', $.attribute_name_list),
      optional(seq('=', field('values', $.expression_list))),
    ),
    _local_declaration: $ => seq(
      $.word_local, field('names', $.attribute_name_list),
      optional(seq('=', field('values', $.expression_list))),
    ),

    return_statement: $ => seq(
      $.word_return,
      optional($.expression_list),
      optional(';'),
    ),

    // ========================================================================
    // 2.3  Names and variables
    // ========================================================================

    function_name: $ => seq(
      $.identifier,
      repeat(seq('.', $.identifier)),
      optional(seq(':', field('method', $.identifier))),
    ),

    variable_list: $ => seq($.variable, repeat(seq(',', $.variable))),

    variable: $ => choice(
      $.identifier,
      seq(
        field('object', $._prefix_expression),
        '[',
        field('index', $._expression),
        ']',
      ),
      seq(
        field('object', $._prefix_expression),
        '.',
        field('field', $.identifier),
      ),
    ),

    name_list: $ => seq($.identifier, repeat(seq(',', $.identifier))),

    attribute_name_list: $ => seq(
      $.identifier, optional($.attribute),
      repeat(seq(',', $.identifier, optional($.attribute))),
    ),

    attribute: $ => seq('<', field('name', $.identifier), '>'),

    // ========================================================================
    // 2.4  Expressions
    // ========================================================================

    expression_list: $ => seq($._expression, repeat(seq(',', $._expression))),

    _expression: $ => choice(
      $.binary_expression,
      $.unary_expression,
      $._primary_expression,
    ),

    binary_expression: $ => choice(
      prec.left(PREC.OR,      seq(field('left', $._expression), field('operator', $.word_or),  field('right', $._expression))),
      prec.left(PREC.AND,     seq(field('left', $._expression), field('operator', $.word_and), field('right', $._expression))),
      prec.left(PREC.COMPARE, seq(field('left', $._expression), field('operator', choice('<', '<=', '>', '>=', '==', '~=')), field('right', $._expression))),
      prec.left(PREC.BIT_OR,  seq(field('left', $._expression), field('operator', '|'),   field('right', $._expression))),
      prec.left(PREC.BIT_XOR, seq(field('left', $._expression), field('operator', '~'),   field('right', $._expression))),
      prec.left(PREC.BIT_AND, seq(field('left', $._expression), field('operator', '&'),   field('right', $._expression))),
      prec.left(PREC.SHIFT,   seq(field('left', $._expression), field('operator', choice('<<', '>>')), field('right', $._expression))),
      prec.right(PREC.CONCAT, seq(field('left', $._expression), field('operator', '..'),  field('right', $._expression))),
      prec.left(PREC.ADD,     seq(field('left', $._expression), field('operator', choice('+', '-')),    field('right', $._expression))),
      prec.left(PREC.MUL,     seq(field('left', $._expression), field('operator', choice('*', '/', '//', '%')), field('right', $._expression))),
      prec.right(PREC.POWER,  seq(field('left', $._expression), field('operator', '^'),   field('right', $._expression))),
    ),

    unary_expression: $ => prec.left(PREC.UNARY, seq(
      field('operator', choice($.word_not, '#', '-', '~')),
      field('operand', $._expression),
    )),

    _primary_expression: $ => choice(
      $.nil,
      $.false,
      $.true,
      $.number,
      $.string,
      $.vararg_expression,
      $.function_definition,
      $._prefix_expression,
      $.table_constructor,
    ),

    nil: $ => $.word_nil,
    false: $ => $.word_false,
    true: $ => $.word_true,
    vararg_expression: _ => '...',

    // ========================================================================
    // 2.5  Prefix expressions and function calls
    // ========================================================================

    _prefix_expression: $ => choice(
      $.variable,
      $.function_call,
      $.parenthesized_expression,
    ),

    parenthesized_expression: $ => seq('(', $._expression, ')'),

    function_call: $ => choice(
      seq(
        field('callee', $._prefix_expression),
        field('arguments', $.arguments),
      ),
      seq(
        field('callee', $._prefix_expression),
        ':',
        field('method', $.identifier),
        field('arguments', $.arguments),
      ),
    ),

    arguments: $ => choice(
      seq('(', optional($.expression_list), ')'),
      $.table_constructor,
      $.string,
    ),

    // ========================================================================
    // 2.6  Function definitions
    // ========================================================================

    function_definition: $ => seq($.word_function, field('body', $.function_body)),

    function_body: $ => seq(
      field('parameters', $.parameter_list),
      optional($._block),
      $.word_end,
    ),

    parameter_list: $ => seq(
      '(',
      optional($._parameter_list_content),
      ')',
    ),

    _parameter_list_content: $ => choice(
      seq(
        $.identifier,
        repeat(seq(',', $.identifier)),
        optional(seq(',', '...')),
      ),
      '...',
    ),

    // ========================================================================
    // 2.7  Table constructors
    // ========================================================================

    table_constructor: $ => seq(
      '{',
      optional($.field_list),
      '}',
    ),

    field_list: $ => seq(
      $.field,
      repeat(seq($._field_separator, $.field)),
      optional($._field_separator),
    ),

    field: $ => choice(
      seq('[', field('key', $._expression), ']', '=', field('value', $._expression)),
      seq(field('key', $.identifier), '=', field('value', $._expression)),
      field('value', $._expression),
    ),

    _field_separator: _ => choice(',', ';'),

    // ========================================================================
    // Lexical: numbers, strings
    // ========================================================================

    number: _ => {
      const decimal_integer = /[0-9]+/;
      const hex_integer = /0[xX][0-9a-fA-F]+/;
      const decimal_float = choice(
        /[0-9]+\.[0-9]*([eE][+-]?[0-9]+)?/,
        /[0-9]+[eE][+-]?[0-9]+/,
        /\.[0-9]+([eE][+-]?[0-9]+)?/,
      );
      const hex_float = choice(
        /0[xX][0-9a-fA-F]+\.[0-9a-fA-F]*([pP][+-]?[0-9]+)?/,
        /0[xX][0-9a-fA-F]+[pP][+-]?[0-9]+/,
        /0[xX]\.[0-9a-fA-F]+([pP][+-]?[0-9]+)?/,
      );
      return token(choice(hex_float, hex_integer, decimal_float, decimal_integer));
    },

    string: $ => choice(
      $.short_string,
      $.long_string,
    ),

    short_string: $ => choice(
      seq('"', optional($.short_string_content_double), '"'),
      seq("'", optional($.short_string_content_single), "'"),
    ),

    long_string: $ => seq(
      '[',
      $.long_string_content,
      ']',
    ),

    // ========================================================================
    // Identifiers — handled by external scanner
    // ========================================================================
  },
});
