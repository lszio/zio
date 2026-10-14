(comment) @comment
(string) @string
(escape_sequence) @string.escape
(character) @character
(number) @number
(boolean) @boolean
(nil) @constant.builtin
(keyword) @constant
(symbol) @variable

(list . (symbol) @function.call)
(list . (symbol) @keyword
  (#any-of? @keyword "def" "defn" "defmacro" "fn" "lambda" "if" "do" "let" "let*" "loop" "recur" "quote" "and" "or" "cond" "set!" "module" "require" "export" "try" "catch" "finally" "throw" "defclass" "defgeneric" "defmethod"))
(list . (symbol) @_def . (symbol) @function
  (#any-of? @_def "defn" "defmacro" "defgeneric"))
(list . (symbol) @_class . (symbol) @type
  (#eq? @_class "defclass"))
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["'" "#"] @punctuation.special
