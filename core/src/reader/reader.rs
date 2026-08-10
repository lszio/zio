use im::{vector, Vector};

use crate::error::ReaderError;
use crate::sexp::Sexp;
use crate::span::{BytePos, SourceId, Span};
use crate::reader::lexer::{Token, tokenize};

/// Read a single S-expression from the input string.
/// Produces a Sexp with source spans attached (using SourceId::NONE,
/// which means spans are stripped for backward compat).
pub fn read(input: &str) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens, SourceId::NONE, input)?;
    Ok(strip_spans(sexp))
}

/// Read a single S-expression with a known source file.

/// Recursively strip all spans from a Sexp tree.
fn strip_spans(sexp: Sexp) -> Sexp {
    fn go(s: Sexp) -> Sexp {
        match s {
            Sexp::Nil => Sexp::Nil,
            Sexp::Boolean(b) => Sexp::Boolean(b),
            Sexp::Integer(i, _) => Sexp::Integer(i, None),
            Sexp::Float(f, _) => Sexp::Float(f, None),
            Sexp::String(s, _) => Sexp::String(s, None),
            Sexp::Symbol(sym, _) => Sexp::Symbol(sym, None),
            Sexp::Keyword(k, _) => Sexp::Keyword(k, None),
            Sexp::List(list, _) => Sexp::List(list.into_iter().map(go).collect(), None),
            Sexp::Vector(v, _) => Sexp::Vector(v.into_iter().map(go).collect(), None),
            Sexp::Map(m, _) => Sexp::Map(
                m.into_iter().map(|(k, v)| (go(k), go(v))).collect(),
                None,
            ),
            Sexp::Char(c, _) => Sexp::Char(c, None),
        }
    }
    go(sexp)
}
pub fn read_with_source(input: &str, source_id: SourceId) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens, source_id, input)?;
    Ok(sexp)
}

/// Read from tokens using an explicit stack.
/// Stack entries: (open_delim, items, start_pos_of_delim).
fn read_from_tokens(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>,
    source_id: SourceId,
    input: &str,
) -> Result<(Sexp, bool), ReaderError> {
    let mut stack: Vec<(String, Vector<Sexp>, BytePos)> = Vec::new();

    while let Some(token) = tokens.next() {
        let start = token.start;
        let end = token.end();

        match token.text.as_str() {
            "'" => {
                // Reader macro: 'x → (quote x)
                let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                let inner_span = inner.span().unwrap_or(Span::DUMMY);
                let sym = Sexp::Symbol("quote".into(), Some(Span {
                    source_id,
                    start,
                    end: start,
                    line: 1,
                    col: start.0 + 1,
                }));
                let quoted = Sexp::List(
                    vector![sym, inner],
                    Some(Span {
                        source_id,
                        start,
                        end: inner_span.end,
                        line: 1,
                        col: start.0 + 1,
                    }),
                );
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(quoted);
                } else {
                    return Ok((quoted, tokens.peek().is_some()));
                }
            }
            "#" => {
                // Dispatch reader macro: #( → vector, #{ → set
                match tokens.next() {
                    Some(tok) if tok.text == "(" => {
                        // #(1 2 3) → vector [1 2 3]
                        let list_start = start;
                        let mut items = Vector::new();
                        let mut last_end = tok.end();
                        loop {
                            match tokens.peek().map(|t| t.text.as_str()) {
                                Some(")") => {
                                    last_end = tokens.next().unwrap().end();
                                    break;
                                }
                                None => return Err(ReaderError::UnexpectedEOF),
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                                    last_end = inner.span().map(|s| s.end).unwrap_or(last_end);
                                    items.push_back(inner);
                                }
                            }
                        }
                        let vec = Sexp::Vector(items, Some(Span {
                            source_id,
                            start: list_start,
                            end: last_end,
                            line: 1,
                            col: list_start.0 + 1,
                        }));
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(vec);
                        } else {
                            return Ok((vec, tokens.peek().is_some()));
                        }
                    }
                    Some(tok) if tok.text == "{" => {
                        // #{a b c} → (set a b c)
                        let set_start = start;
                        let mut items = Vector::new();
                        items.push_back(Sexp::Symbol("set".into(), None));
                        let mut last_end = tok.end();
                        loop {
                            match tokens.peek().map(|t| t.text.as_str()) {
                                Some("}") => {
                                    last_end = tokens.next().unwrap().end();
                                    break;
                                }
                                None => return Err(ReaderError::UnexpectedEOF),
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                                    last_end = inner.span().map(|s| s.end).unwrap_or(last_end);
                                    items.push_back(inner);
                                }
                            }
                        }
                        let sexp = Sexp::List(items, Some(Span {
                            source_id,
                            start: set_start,
                            end: last_end,
                            line: 1,
                            col: set_start.0 + 1,
                        }));
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(sexp);
                        } else {
                            return Ok((sexp, tokens.peek().is_some()));
                        }
                    }
                    Some(tok) if tok.text.starts_with('\\') && tok.text.len() > 1 => {
                        // #\c → character literal
                        let name = &tok.text[1..];
                        let ch = match name {
                            "space" => ' ',
                            "newline" => '\n',
                            "tab" => '\t',
                            _ if name.len() == 1 => name.chars().next().unwrap(),
                            _ => return Err(ReaderError::UnexpectedToken(name.to_string())),
                        };
                        let ch_span = Some(Span {
                            source_id,
                            start,
                            end: tok.end(),
                            line: 1,
                            col: start.0 + 1,
                        });
                        let val = Sexp::Char(ch, ch_span);
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(val);
                        } else {
                            return Ok((val, tokens.peek().is_some()));
                        }
                    }
                    Some(other) => {
                        return Err(ReaderError::UnexpectedToken(other.text.clone()));
                    }
                    None => {
                        return Err(ReaderError::UnexpectedEOF);
                    }
                }
            }
            "(" | "[" | "{" => {
                stack.push((token.text.clone(), Vector::new(), start));
            }
            ")" | "]" | "}" => {
                let (open, items, open_start) = stack
                    .pop()
                    .ok_or_else(|| ReaderError::UnexpectedToken(token.text.clone()))?;
                if (open == "(" && token.text != ")")
                    || (open == "[" && token.text != "]")
                    || (open == "{" && token.text != "}")
                {
                    return Err(ReaderError::UnexpectedToken(token.text));
                }
                let span = Some(Span {
                    source_id,
                    start: open_start,
                    end,
                    line: 1,
                    col: open_start.0 + 1,
                });
                let val = match open.as_str() {
                    "(" => Sexp::List(items, span),
                    "[" => Sexp::Vector(items, span),
                    "{" => {
                        if items.len() % 2 != 0 {
                            return Err(ReaderError::OddMapElements);
                        }
                        let mut map = im::HashMap::new();
                        let mut iter = items.into_iter();
                        while let Some(key) = iter.next() {
                            let val = iter.next().unwrap();
                            map.insert(key, val);
                        }
                        Sexp::Map(map, span)
                    }
                    _ => unreachable!(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(val);
                } else {
                    return Ok((val, tokens.peek().is_some()));
                }
            }
            _ => {
                // Atom token — parse with position info
                let span = Some(Span {
                    source_id,
                    start,
                    end,
                    line: 1,
                    col: start.0 + 1,
                });
                let val = parse_atom(&token.text, span);
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(val);
                } else {
                    return Ok((val, tokens.peek().is_some()));
                }
            }
        }
    }

    if !stack.is_empty() {
        Err(ReaderError::UnexpectedEOF)
    } else {
        // Input was empty or all whitespace — return Nil
        Ok((Sexp::Nil, false))
    }
}

fn parse_atom(token: &str, span: Option<Span>) -> Sexp {
    if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        // Basic unescaping for common characters
        let s = &token[1..token.len() - 1];
        let mut unescaped = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => unescaped.push('\n'),
                    Some('t') => unescaped.push('\t'),
                    Some('r') => unescaped.push('\r'),
                    Some('\\') => unescaped.push('\\'),
                    Some('"') => unescaped.push('"'),
                    Some(c) => {
                        unescaped.push('\\');
                        unescaped.push(c);
                    }
                    None => unescaped.push('\\'),
                }
            } else {
                unescaped.push(c);
            }
        }
        Sexp::String(unescaped, span)
    } else if let Some(name) = token.strip_prefix(':') {
        Sexp::Keyword(name.to_string(), span)
    } else {
        match token {
            "nil" => Sexp::Nil,
            "true" => Sexp::Boolean(true),
            "false" => Sexp::Boolean(false),
            _ => {
                if let Ok(i) = token.parse::<i64>() {
                    Sexp::Integer(i, span)
                } else if let Ok(f) = token.parse::<f64>() {
                    Sexp::Float(f, span)
                } else {
                    Sexp::Symbol(token.to_string(), span)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_for_test(input: &str) -> Result<Sexp, ReaderError> {
        read(input)
    }

    #[test]
    fn read_with_source_preserves_root_byte_range() {
        let parsed = read_with_source("  (+ 1 2)", SourceId(7)).unwrap();
        let span = parsed.span().expect("root form should have a source span");

        assert_eq!(span.start, BytePos(2));
        assert_eq!(span.end, BytePos(9));
    }

    #[test]
    fn test_read_integer() {
        assert_eq!(parse_for_test("42"), Ok(Sexp::Integer(42, None)));
    }

    #[test]
    fn test_read_symbol() {
        assert_eq!(parse_for_test("foo"), Ok(Sexp::Symbol("foo".into(), None)));
    }

    #[test]
    fn test_read_nested_list() {
        assert_eq!(
            parse_for_test("(+ 1 (* 2 3))"),
            Ok(Sexp::List(
                vector![
                    Sexp::Symbol("+".into(), None),
                    Sexp::Integer(1, None),
                    Sexp::List(
                        vector![
                            Sexp::Symbol("*".into(), None),
                            Sexp::Integer(2, None),
                            Sexp::Integer(3, None),
                        ],
                        None
                    ),
                ],
                None
            ))
        );
    }

    #[test]
    fn test_read_float() {
        assert_eq!(parse_for_test("3.5"), Ok(Sexp::Float(3.5, None)));
    }

    #[test]
    fn test_read_nil() {
        assert_eq!(parse_for_test("nil"), Ok(Sexp::Nil));
    }

    #[test]
    fn test_read_boolean() {
        assert_eq!(parse_for_test("true"), Ok(Sexp::Boolean(true)));
        assert_eq!(parse_for_test("false"), Ok(Sexp::Boolean(false)));
    }

    #[test]
    fn test_unexpected_eof() {
        assert_eq!(parse_for_test("("), Err(ReaderError::UnexpectedEOF));
    }

    #[test]
    fn test_unexpected_token() {
        assert_eq!(
            parse_for_test(")"),
            Err(ReaderError::UnexpectedToken(")".to_string()))
        );
    }

    #[test]
    fn test_read_string() {
        assert_eq!(
            parse_for_test("\"hello (world)\""),
            Ok(Sexp::String("hello (world)".into(), None))
        );
    }

    #[test]
    fn test_read_keyword() {
        assert_eq!(parse_for_test(":foo"), Ok(Sexp::Keyword("foo".into(), None)));
    }

    #[test]
    fn test_read_vector() {
        assert_eq!(
            parse_for_test("[1 2 3]"),
            Ok(Sexp::Vector(
                vector![
                    Sexp::Integer(1, None),
                    Sexp::Integer(2, None),
                    Sexp::Integer(3, None),
                ],
                None
            ))
        );
    }

    #[test]
    fn test_read_map() {
        use im::hashmap;
        assert_eq!(
            parse_for_test("{:a 1 :b 2}"),
            Ok(Sexp::Map(
                hashmap! {
                    Sexp::Keyword("a".into(), None) => Sexp::Integer(1, None),
                    Sexp::Keyword("b".into(), None) => Sexp::Integer(2, None),
                },
                None
            ))
        );
    }

    #[test]
    fn test_deep_recursion() {
        let mut deep = String::new();
        for _ in 0..100 {
            deep.push_str("(");
        }
        deep.push_str("1");
        for _ in 0..100 {
            deep.push_str(")");
        }
        assert!(parse_for_test(&deep).is_ok());
    }

    #[test]
    fn test_quote_reader_macro() {
        assert_eq!(
            parse_for_test("'42"),
            Ok(Sexp::List(
                vector![
                    Sexp::Symbol("quote".into(), None),
                    Sexp::Integer(42, None),
                ],
                None
            ))
        );
    }

    #[test]
    fn test_quote_list() {
        assert_eq!(
            parse_for_test("'(1 2 3)"),
            Ok(Sexp::List(
                vector![
                    Sexp::Symbol("quote".into(), None),
                    Sexp::List(
                        vector![
                            Sexp::Integer(1, None),
                            Sexp::Integer(2, None),
                            Sexp::Integer(3, None),
                        ],
                        None
                    ),
                ],
                None
            ))
        );
    }

    #[test]
    fn test_quote_nested() {
        assert_eq!(
            parse_for_test("''x"),
            Ok(Sexp::List(
                vector![
                    Sexp::Symbol("quote".into(), None),
                    Sexp::List(
                        vector![
                            Sexp::Symbol("quote".into(), None),
                            Sexp::Symbol("x".into(), None),
                        ],
                        None
                    ),
                ],
                None
            ))
        );
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(parse_for_test(""), Ok(Sexp::Nil));
        assert_eq!(parse_for_test("   "), Ok(Sexp::Nil));
    }

    #[test]
    fn test_comment() {
        // read() only returns the first expression; comments separate tokens
        // so "a ; comment\n b" reads as two separate expressions.
        assert_eq!(
            parse_for_test("a ; comment\n b"),
            Ok(Sexp::Symbol("a".into(), None))
        );
    }

    #[test]
    fn test_hash_brace_set() {
        assert_eq!(
            parse_for_test("#{a b c}"),
            Ok(Sexp::List(
                vector![
                    Sexp::Symbol("set".into(), None),
                    Sexp::Symbol("a".into(), None),
                    Sexp::Symbol("b".into(), None),
                    Sexp::Symbol("c".into(), None),
                ],
                None
            ))
        );
    }
}
