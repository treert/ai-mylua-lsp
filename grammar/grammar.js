/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

// Lua operator precedence (lowest to highest, per Lua 5.4 §3.4.8)
const PREC = {
  NIL_COALESCE: 1,
  OR: 2,
  AND: 3,
  COMPARE: 4,
  BIT_OR: 5,
  BIT_XOR: 6,
  BIT_AND: 7,
  SHIFT: 8,
  CONCAT: 9,
  ADD: 10,
  MUL: 11,
  UNARY: 12,
  POWER: 13,
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
    $.dollar_string_content_double,
    $.dollar_string_content_single,
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
    $.word_continue,
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
    [$._array_expression_list, $.field],
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
      $.continue_statement,
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

    function_call_statement: $ => choice(
      $.function_call,
      alias($._safe_function_call, $.function_call),
    ),

    label_statement: $ => seq('::', field('name', $._name_like_identifier), '::'),

    break_statement: $ => $.word_break,

    continue_statement: $ => $.word_continue,

    _name_like_identifier: $ => choice(
      $.identifier,
      $._keyword_identifier,
    ),

    _member_name: $ => $._name_like_identifier,

    _keyword_identifier: $ => choice(
      alias($.word_end, $.identifier),
      alias($.word_local, $.identifier),
      alias($.word_if, $.identifier),
      alias($.word_then, $.identifier),
      alias($.word_elseif, $.identifier),
      alias($.word_else, $.identifier),
      alias($.word_while, $.identifier),
      alias($.word_do, $.identifier),
      alias($.word_repeat, $.identifier),
      alias($.word_until, $.identifier),
      alias($.word_for, $.identifier),
      alias($.word_in, $.identifier),
      alias($.word_function, $.identifier),
      alias($.word_goto, $.identifier),
      alias($.word_return, $.identifier),
      alias($.word_break, $.identifier),
      alias($.word_continue, $.identifier),
      alias($.word_and, $.identifier),
      alias($.word_or, $.identifier),
      alias($.word_not, $.identifier),
      alias($.word_nil, $.identifier),
      alias($.word_true, $.identifier),
      alias($.word_false, $.identifier),
    ),

    // -- goto (top / nested) --
    _top_goto_statement: $ => seq($.top_word_goto, field('name', $._name_like_identifier)),
    _goto_statement: $ => seq($.word_goto, field('name', $._name_like_identifier)),

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
      repeat(seq('.', $._member_name)),
      optional(seq(':', field('method', $._member_name))),
    ),

    variable_list: $ => seq($.variable, repeat(seq(',', $.variable))),

    variable: $ => choice(
      $.identifier,
      seq(
        field('object', $._prefix_expression),
        '[',
        optional(field('index', $._expression)),
        ']',
      ),
      seq(
        field('object', $._prefix_expression),
        '.',
        field('field', $._member_name),
      ),
    ),

    _safe_variable: $ => choice(
      seq(
        field('object', $._prefix_expression),
        token.immediate('?['),
        optional(field('index', $._expression)),
        ']',
      ),
      seq(
        field('object', $._prefix_expression),
        token.immediate('?.'),
        field('field', $._member_name),
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
      prec.right(PREC.NIL_COALESCE, seq(field('left', $._expression), field('operator', '??'), field('right', $._expression))),
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
      $.dollar_string,
      $.vararg_expression,
      $.function_definition,
      $.dollar_function,
      $._prefix_expression,
      $.table_constructor,
      $.array_constructor,
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
      prec.dynamic(-10, alias($._safe_variable, $.variable)),
      prec.dynamic(-10, alias($._safe_function_call, $.function_call)),
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
        field('method', $._member_name),
        field('arguments', $.arguments),
      ),
    ),

    _safe_function_call: $ => choice(
      seq(
        field('callee', $._prefix_expression),
        token.immediate('?'),
        field('arguments', $.arguments),
      ),
      seq(
        field('callee', $._prefix_expression),
        token.immediate('?:'),
        field('method', $._member_name),
        field('arguments', $.arguments),
      ),
    ),

    arguments: $ => choice(
      seq('(', optional(alias($._argument_expression_list, $.expression_list)), ')'),
      $.table_constructor,
      $.string,
      $.dollar_string,
      $.dollar_function,
    ),

    _argument_expression_list: $ => seq(
      $._argument,
      repeat(seq(',', $._argument)),
      optional(','),
    ),

    _argument: $ => choice(
      $.named_argument,
      $.spread_argument,
      $._expression,
    ),

    named_argument: $ => seq(
      field('name', $.identifier),
      '=',
      field('value', $._expression),
    ),

    spread_argument: $ => seq(
      '*',
      field('value', $._expression),
    ),

    // ========================================================================
    // 2.6  Function definitions
    // ========================================================================

    function_definition: $ => seq($.word_function, field('body', $.function_body)),

    dollar_function: $ => seq('$', field('body', $.dollar_function_body)),

    dollar_function_body: $ => seq(
      optional(field('parameters', $.parameter_list)),
      '{',
      optional($._block),
      '}',
    ),

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
        optional(','),
      ),
      seq('...', optional(',')),
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

    array_constructor: $ => seq(
      '[',
      optional($._array_expression_list),
      ']',
    ),

    _array_expression_list: $ => seq(
      $._expression,
      repeat(seq($._field_separator, $._expression)),
      optional($._field_separator),
    ),

    field: $ => choice(
      seq('[', field('key', $._expression), ']', '=', field('value', $._expression)),
      seq(field('key', $._member_name), '=', field('value', $._expression)),
      field('value', $._expression),
    ),

    _field_separator: _ => choice(',', ';'),

    // ========================================================================
    // Lexical: numbers, strings
    // ========================================================================

    number: _ => {
      const decimal_integer = /[0-9][0-9_]*/;
      const hex_integer = /0[xX][0-9a-fA-F][0-9a-fA-F_]*/;
      const decimal_float = choice(
        /[0-9][0-9_]*\.[0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
        /[0-9][0-9_]*[eE][+-]?[0-9][0-9_]*/,
        /\.[0-9][0-9_]*([eE][+-]?[0-9][0-9_]*)?/,
      );
      const hex_float = choice(
        /0[xX][0-9a-fA-F][0-9a-fA-F_]*\.[0-9a-fA-F_]*([pP][+-]?[0-9][0-9_]*)?/,
        /0[xX][0-9a-fA-F][0-9a-fA-F_]*[pP][+-]?[0-9][0-9_]*/,
        /0[xX]\.[0-9a-fA-F][0-9a-fA-F_]*([pP][+-]?[0-9][0-9_]*)?/,
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

    dollar_string: $ => choice(
      seq('$"', repeat($._dollar_string_item_double), '"'),
      seq("$'", repeat($._dollar_string_item_single), "'"),
    ),

    _dollar_string_item_double: $ => choice(
      alias($.dollar_string_content_double, $.dollar_string_content),
      $.dollar_escape,
      $.dollar_name_interpolation,
      $.dollar_interpolation,
    ),

    _dollar_string_item_single: $ => choice(
      alias($.dollar_string_content_single, $.dollar_string_content),
      $.dollar_escape,
      $.dollar_name_interpolation,
      $.dollar_interpolation,
    ),

    dollar_escape: _ => '$$',

    dollar_name_interpolation: $ => seq(
      '$',
      field('name', $.identifier),
    ),

    dollar_interpolation: $ => seq(
      '${',
      field('value', $._expression),
      '}',
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
