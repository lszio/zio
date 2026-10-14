; Query locals are presentation hints. Sequential let* initializer visibility,
; quotes, module exports and exact shadowing are handled by zio-lsp's analyzer.
(list) @local.scope
(list . (symbol) @_def . (symbol) @local.definition
  (#any-of? @_def "def" "defn" "defmacro" "defclass" "defgeneric"))
(list . (symbol) @_fn . (vector (symbol) @local.definition)
  (#any-of? @_fn "fn" "lambda"))
(list . (symbol) @_defn . (symbol) . (vector (symbol) @local.definition)
  (#any-of? @_defn "defn" "defmacro"))
(list . (symbol) @_fn . (list (symbol) @local.definition)
  (#any-of? @_fn "fn" "lambda"))
(list . (symbol) @_defn . (symbol) . (list (symbol) @local.definition)
  (#any-of? @_defn "defn" "defmacro"))
(list . (symbol) @_let . (list (list . (symbol) @local.definition))
  (#any-of? @_let "let" "let*" "loop"))
(symbol) @local.reference
