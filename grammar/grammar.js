/**
 * @file CR grammar: C with coroutine and scope-exit extensions.
 * @license MIT
 */

/// <reference types="tree-sitter-cli/dsl" />
// @ts-check

const C = require('tree-sitter-c/grammar');
const { PREC } = C;

module.exports = grammar(C, {
  name: 'cr',

  rules: {
    _declaration_modifiers: ($, previous) => choice(
      previous,
      $.async_specifier,
    ),

    _non_case_statement: ($, previous) => choice(
      previous,
      $.defer_statement,
    ),

    _expression_not_binary: ($, previous) => choice(
      previous,
      $.await_expression,
      $.yield_expression,
    ),

    async_specifier: _ => '__async',

    await_expression: $ => prec.right(PREC.UNARY, seq(
      field('operator', choice('__await', '__awite')),
      field('argument', $.expression),
    )),

    yield_expression: $ => prec.right(PREC.ASSIGNMENT, seq(
      '__yield',
      optional(field('value', $.expression)),
    )),

    defer_statement: $ => seq(
      '__defer',
      field('call', $.call_expression),
      ';',
    ),
  },
});
