" syntax/zio.vim: baseline syntax highlighting for *.zio files.
" Provides a Vim regex fallback. Tree-sitter highlighting is preferred when
" contribs/tree-sitter-zio is generated and registered with nvim-treesitter.

if exists("g:current_syntax")
  finish
endif

syn match zioComment ";.*$"
syn match zioNumber "\<\(+\|-\)\=\(\(\d\+\(\.\d*\)\=\|\.\d\+\)\(e\|E\)\(+\|-\)\=\d\+\|\d\+\(\.\d*\)\=\|\.\d\+\|\<\inf\>\|-\<inf\>\|-\<infinity\>\|+\<infinity\>\|\<nan\>\)"
syn match zioBoolean "\<\%(true\|false\)\>"
syn match zioNil "\<nil\>"

syn region zioString start=/"/ end=/"/ contains=zioStringEscape
syn match zioStringEscape "\\\(space\|newline\|tab\|.\)" contained
syn match zioCharacter "#\\\(\<space\>\|\<newline\>\|\<tab\>\|.\)"

syn match zioKeyword ":\w*"
syn match zioDispatchVector "#("
syn match zioSetLiteral "#{"
syn match zioQuote "'"

syn match zioBracket "[(){}\[\]]"

syn match zioForm "[A-Za-z_\-+*/=<>!?][A-Za-z0-9_\-+*/=<>!?]*"

hi def link zioComment Comment
hi def link zioNumber Number
hi def link zioBoolean Boolean
hi def link zioNil Constant
hi def link zioString String
hi def link zioStringEscape SpecialChar
hi def link zioCharacter Character
hi def link zioKeyword Keyword
hi def link zioDispatchVector Special
hi def link zioSetLiteral Special
hi def link zioQuote Special
hi def link zioBracket Delimiter
hi def link zioForm Identifier

let g:current_syntax = "zio"
