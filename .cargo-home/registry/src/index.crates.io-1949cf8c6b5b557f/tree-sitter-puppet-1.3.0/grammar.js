/**
 * @file Puppet grammar for tree-sitter
 * @author Amaan Qureshi <amaanq12@gmail.com>
 * @license MIT
 * @see {@link https://www.puppet.com/docs/puppet/6/puppet_language.html|Official Website}
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const PREC = {
  PARENTHESES: -1,
  ASSIGNMENT: 1,
  LOGICAL_OR: 2,
  LOGICAL_AND: 3,
  COMPARE: 4,
  EQUALITY: 5,
  SHIFT: 6,
  ADD: 7,
  MULTIPLY: 8,
  MATCH: 9,
  IN: 10,
  UNARY: 11,
  CALL: 12,
  MEMBER: 13,
};

module.exports = grammar({
  name: 'puppet',

  conflicts: $ => [
    [$.statement, $.expression],
    [$.statement, $.literal],
  ],

  extras: $ => [
    $.comment,
    /\s/,
  ],

  supertypes: $ => [
    $.expression,
    $.literal,
    $.statement,
    $.type,
  ],

  rules: {
    source_file: $ => repeat($.statement),

    statement: $ => prec.right(choice(
      $.class_definition,
      $.node_definition,
      $.defined_resource_type,
      $.type_declaration,
      $.assignment,
      $.relation,
      $.function_declaration,
      $.resource_declaration,
      $.resource_collector,
      $.resource_default,
      $.selector,
      $.require_statement,
      $.include_statement,
      $.tag_statement,
      $.if_statement,
      $.unless_statement,
      $.case_statement,
      $.iterator_statement,
      $.function_call,
      $.hash,
      $.variable,
      $.string,
      $._identifier,
      $.resource_reference,
    )),

    block: $ => seq('{', repeat($.statement), '}'),

    class_definition: $ => seq(
      'class',
      $._identifier,
      optional($.parameter_list),
      optional($.class_inherits),
      $.block,
    ),

    parameter_list: $ => seq(
      '(',
      optional(seq(
        sep1($.parameter, ','),
        optional(','),
      )),
      ')',
    ),

    parameter: $ => seq(
      optional($.type),
      $.variable,
      optional(seq('=', $.expression)),
    ),

    class_inherits: $ => seq('inherits', $._identifier),

    node_definition: $ => seq(
      'node',
      commaSep1($.node_name),
      $.block,
    ),

    node_name: $ => choice($.identifier, $.string, $.default),

    defined_resource_type: $ => seq(
      'define',
      $._identifier,
      optional($.parameter_list),
      $.block,
    ),

    type_declaration: $ => prec.right(seq(
      'type',
      $._identifier,
      '=',
      $.type,
    )),

    assignment: $ => seq($.variable, choice('=', '+='), $.expression),

    relation: $ => prec.left(seq(
      $.statement,
      choice('->', '~>'),
      $.statement,
    )),

    lambda: $ => seq(
      optional(seq('|', commaSep($.variable), '|')),
      $.block,
    ),

    function_declaration: $ => seq(
      'function',
      $._identifier,
      optional($.parameter_list),
      optional(seq('>>', $.type)),
      $.block,
    ),

    resource_declaration: $ => seq(
      optional(choice(
        field('virtual', '@'),
        field('exported', '@@'),
      )),
      field('type', $._identifier),
      '{',
      sep1($._resource_block, ';'),
      optional(';'),
      '}',
    ),

    _resource_block: $ => seq(
      field('title', $.expression),
      ':',
      optional(seq(
        commaSep1($.attribute),
        optional(','),
      )),
    ),

    attribute: $ => seq(
      field('name', choice($.identifier, $.variable, $.string, $.regex, $.number)),
      '=>',
      field('value', choice($.expression, $.selector)),
    ),

    require_statement: $ => seq('require', choice($._identifier, $.string)),

    include_statement: $ => prec(-1, seq(
      'include',
      commaSep1(prec(-1, choice(
        $._identifier,
        $.variable,
        $.string,
        $.function_call,
      ))),
    )),

    tag_statement: $ => seq(
      'tag',
      commaSep1(choice(
        $._identifier,
        $.variable,
        $.string,
        $.parenthesized_expression,
      )),
    ),

    if_statement: $ => seq(
      'if',
      $.expression,
      $.block,
      repeat($.elsif_statement),
      optional($.else_statement),
    ),

    elsif_statement: $ => seq('elsif', $.expression, $.block),

    else_statement: $ => seq('else', $.block),

    unless_statement: $ => seq(
      'unless',
      $.expression,
      $.block,
    ),

    case_statement: $ => seq(
      'case',
      $.expression,
      '{',
      repeat($.case_item),
      optional($.default_case),
      '}',
    ),

    case_item: $ => seq(
      commaSep1($.expression),
      ':',
      $.block,
    ),

    default_case: $ => seq(
      'default',
      ':',
      $.block,
    ),

    iterator_statement: $ => seq(
      field('iterator', $.expression),
      '|',
      commaSep($.variable),
      '|',
      $.block,
    ),

    resource_collector: $ => seq(
      $.identifier,
      choice('<<|', '<|'),
      optional($.search_expression),
      choice('|>>', '|>'),
    ),

    resource_default: $ => seq(
      field('type', choice($._identifier, $.resource_reference)),
      '{',
      optional(seq(
        commaSep1($.attribute),
        optional(','),
      )),
      '}',
    ),

    selector: $ => seq(
      choice($.assignment, $.variable),
      '?',
      '{',
      optional(seq(
        commaSep1($.attribute),
        optional(','),
      )),
      '}',
    ),

    type: $ => prec.right(choice(
      $.builtin_type,
      $.identifier,
      $.class_identifier,
      $.composite_type,
      $.attribute_type,
      $.array_type,
    )),

    builtin_type: _ => choice(
      'Boolean',
      'Integer',
      'Float',
      'String',
      'Array',
      'Hash',
      'Regexp',
      'Variant',
      'Data',
      'Undef',
      'Default',
      'File',
    ),

    composite_type: $ => seq(
      $.identifier,
      '[',
      sep1(choice($.type, $.string), ','),
      optional(','),
      ']',
    ),

    attribute_type: $ => seq(
      '{',
      optional(seq(
        sep1($.attribute_type_entry, ','),
        optional(','),
      )),
      '}',
    ),

    attribute_type_entry: $ => seq(
      field('name', choice($.identifier, $.string)),
      '=>',
      field('value', choice($.type, $.string, $.number)),
    ),

    array_type: $ => choice(
      seq($.type, '[', commaSep(choice($.type, $.number, $.default)), ']'),
      seq('[', ']'), // empty
    ),

    search_expression: $ => seq($.identifier, choice('==', '!='), $.literal),

    expression: $ => prec.right(choice(
      $.unary_expression,
      $.binary_expression,
      $.parenthesized_expression,
      $.function_call,
      $.field_expression,
      $.variable,
      $._identifier,
      $.array,
      $.hash,
      $.literal,
    )),

    unary_expression: $ => prec.right(PREC.UNARY, seq(
      field('operator', choice('!', '-', '*')),
      field('argument', $.expression),
    )),


    binary_expression: $ => {
      const table = [
        ['or', PREC.LOGICAL_OR],
        ['and', PREC.LOGICAL_AND],
        ['>', PREC.COMPARE],
        ['>=', PREC.COMPARE],
        ['<=', PREC.COMPARE],
        ['<', PREC.COMPARE],
        ['==', PREC.EQUALITY],
        ['!=', PREC.EQUALITY],
        ['<<', PREC.SHIFT],
        ['>>', PREC.SHIFT],
        ['+', PREC.ADD],
        ['-', PREC.ADD],
        ['*', PREC.MULTIPLY],
        ['/', PREC.MULTIPLY],
        ['%', PREC.MULTIPLY],
        ['=~', PREC.MATCH],
        ['!~', PREC.MATCH],
        ['in', PREC.IN],
      ];

      return choice(...table.map(([operator, precedence]) => {
        return prec.left(precedence, seq(
          field('left', $.expression),
          // @ts-ignore
          field('operator', operator),
          field('right', $.expression),
        ));
      }));
    },

    parenthesized_expression: $ => prec(PREC.PARENTHESES, seq('(', $.expression, ')')),

    function_call: $ => prec.left(PREC.CALL, seq(
      $.expression,
      '(',
      optional(seq(
        sep1($.expression, ','),
        optional(','),
      )),
      ')',
      optional($.lambda),
    )),

    field_expression: $ => prec(PREC.MEMBER, seq(
      $.expression,
      '.',
      $._identifier,
    )),

    variable: $ => seq('$', $._identifier),

    array: $ => seq(
      '[',
      optional(seq(
        sep1($.expression, ','),
        optional(','),
      )),
      ']',
    ),

    hash: $ => seq(
      '{',
      optional(seq(
        commaSep1($.attribute),
        optional(','),
      )),
      '}',
    ),

    literal: $ => prec.right(choice(
      $.resource_reference,
      $.number,
      $.float,
      $.string,
      $.regex,
      $.boolean,
      $.undef,
    )),

    resource_reference: $ => seq(
      choice($.identifier, $.variable, $.resource_reference),
      '[',
      commaSep1(choice($.identifier, $.variable, $.string)),
      ']',
    ),

    number: _ => {
      const decimal = /[1-9][0-9_]*/;
      const hex = /0x[0-9a-fA-F][0-9a-fA-F_]*/;

      const integer = choice('0', decimal);

      return token(choice(integer, hex));
    },

    float: _ => /-?\d+\.\d*(e-?\d+)?/,

    string: $ => choice(
      seq(
        '"',
        repeat(choice(
          alias($._double_string_content, $.string_content),
          $._escape_sequence,
          $.interpolation,
          $.variable,
        )),
        '"',
      ),
      seq(
        '\'',
        repeat(choice(
          alias($._single_string_content, $.string_content),
          $._escape_sequence,
        )),
        '\'',
      ),
    ),

    _double_string_content: _ => token.immediate(prec(1, /[^"$\\]+/)),

    _single_string_content: _ => token.immediate(prec(1, /[^'\\]+/)),

    _escape_sequence: $ => choice(
      prec(2, token.immediate(seq('\\', /[^abfnrtvxu'\"\\\?]/))),
      prec(1, $.escape_sequence),
    ),

    escape_sequence: _ => token.immediate(seq(
      '\\',
      choice(
        /[^xu0-7]/,
        /[0-7]{1,3}/,
        /x[0-9a-fA-F]{2}/,
        /u[0-9a-fA-F]{4}/,
        /u\{[0-9a-fA-F]+\}/,
        /U[0-9a-fA-F]{8}/,
      ),
    )),

    interpolation: $ => prec(1, seq('${', choice($.expression, $.if_statement), '}')),

    regex: _ => token(seq('/', /[^*]([^\/]|\\\/)*/, '/')),

    boolean: _ => choice('true', 'false'),

    undef: _ => 'undef',

    default: _ => 'default',

    _identifier: $ => prec.right(choice($.class_identifier, $.identifier)),

    class_identifier: $ => seq(choice('$', $.identifier), '::', sep1($.identifier, '::')),

    identifier: _ => /[a-zA-Z_][a-zA-Z0-9_]*/,

    comment: _ => token(prec(-1, choice(
      seq('#', /[^\r\n]*/),
      seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
    ))),
  },
});

/**
 * Creates a rule to optionally match one or more of the rules separated by a comma
 *
 * @param {Rule} rule
 *
 * @returns {ChoiceRule}
 */
function commaSep(rule) {
  return optional(commaSep1(rule));
}

/**
 * Creates a rule to match one or more of the rules separated by a comma
 *
 * @param {Rule} rule
 *
 * @returns {SeqRule}
 */
function commaSep1(rule) {
  return sep1(rule, ',');
}

/**
 * Creates a rule to match one or more occurrences of `rule` separated by `sep`
 *
 * @param {RegExp | Rule | string} rule
 *
 * @param {RegExp | Rule | string} sep
 *
 * @returns {SeqRule}
 */
function sep1(rule, sep) {
  return seq(rule, repeat(seq(sep, rule)));
}
