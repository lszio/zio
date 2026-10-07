// This is Zio reader syntax, not Clojure: #(...) is a vector.
const atom = /[^\s()[\]{}'"#;]+/;
module.exports = grammar({
  name: 'zio',
  extras: $ => [/\s/, $.comment],
  rules: {
    source_file: $ => repeat($._form),
    _form: $ => choice($.list, $.vector, $.dispatch_vector, $.set, $.map,
      $.quote, $.string, $.character, $.number, $.boolean, $.nil, $.keyword, $.symbol),
    list: $ => seq('(', repeat($._form), ')'),
    vector: $ => seq('[', repeat($._form), ']'),
    dispatch_vector: $ => seq('#', '(', repeat($._form), ')'),
    set: $ => seq('#', '{', repeat($._form), '}'),
    // Pair validity is a reader diagnostic. Retain all forms while typing an odd map.
    map: $ => seq('{', repeat($._form), '}'),
    quote: $ => seq("'", $._form),
    string: $ => seq('"', repeat(choice($.escape_sequence, $.string_content)), '"'),
    // Unknown escapes are retained literally by the reader, not JSON/Clojure escapes.
    escape_sequence: _ => token.immediate(seq('\\', /[\s\S]/)),
    string_content: _ => token.immediate(/[^"\\]+/),
    character: _ => seq('#', token(seq('\\', choice('space', 'newline', 'tab', /[\s\S]/)))),
    number: _ => token(choice(
      /[+-]?[0-9]+/,
      /[+-]?(([0-9]+\.[0-9]*|\.[0-9]+)([eE][+-]?[0-9]+)?|[0-9]+[eE][+-]?[0-9]+)/,
      /[+-]?([iI][nN][fF]([iI][nN][iI][tT][yY])?|[nN][aA][nN])/
    )),
    boolean: _ => token(choice('true', 'false')),
    nil: _ => token('nil'),
    keyword: _ => token(seq(':', optional(atom))),
    // A symbol is a run of characters that are not whitespace,
    // delimiters, a quote, a semicolon or a backslash. The classes are
    // written as separate `choice` arms rather than one long class
    // because a literal `'` inside a tree-sitter regex ends the pattern
    // it is being embedded in.
    symbol: _ => token(choice(
      atom,
      seq('#', repeat1(choice(
        /[^\s()[\]{}";\\]/,
        /[0-9]/,
      ))),
    )),
    comment: _ => token(seq(';', /[^\n]*/)),
  },
});
