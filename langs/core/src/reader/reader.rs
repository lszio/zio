use im::{Vector, vector};

use crate::error::ReaderError;
use crate::reader::lexer::{Token, tokenize};
use crate::sexp::Sexp;
use crate::span::{BytePos, SourceId, Span};

/// Read a single S-expression from the input string.
/// Produces a Sexp with source spans attached (using SourceId::NONE,
/// which means spans are stripped for backward compat).
pub fn read(input: &str) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens, SourceId::NONE, input)?;
    if let Some(token) = tokens.peek() {
        return Err(
            ReaderError::UnexpectedToken(token.text.clone()).at(token_span(
                input,
                SourceId::NONE,
                token.start,
                token.end(),
            )),
        );
    }
    Ok(strip_spans(sexp))
}

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
            Sexp::Map(m, _) => {
                Sexp::Map(m.into_iter().map(|(k, v)| (go(k), go(v))).collect(), None)
            }
            Sexp::Char(c, _) => Sexp::Char(c, None),
        }
    }
    go(sexp)
}

/// Read a single S-expression with a known source file.
pub fn read_with_source(input: &str, source_id: SourceId) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens, source_id, input)?;
    if let Some(token) = tokens.peek() {
        return Err(
            ReaderError::UnexpectedToken(token.text.clone()).at(token_span(
                input,
                source_id,
                token.start,
                token.end(),
            )),
        );
    }
    Ok(sexp)
}

/// Read multiple top-level S-expressions from a multi-line source string.
pub fn read_program(input: &str) -> Result<Vec<Sexp>, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let mut results = Vec::new();
    while tokens.peek().is_some() {
        let (sexp, _) = read_from_tokens(&mut tokens, SourceId::NONE, input)?;
        results.push(strip_spans(sexp));
    }
    Ok(results)
}

/// Read multiple top-level S-expressions, keeping source spans attached
/// so runtime errors inside the program carry line/column information.
pub fn read_program_with_source(
    input: &str,
    source_id: SourceId,
) -> Result<Vec<Sexp>, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let mut results = Vec::new();
    while tokens.peek().is_some() {
        let (sexp, _) = read_from_tokens(&mut tokens, source_id, input)?;
        results.push(sexp);
    }
    Ok(results)
}

fn token_span(input: &str, source_id: SourceId, start: BytePos, end: BytePos) -> Span {
    let (line, col) = crate::span::line_col_of(input, &crate::span::line_starts_of(input), start.0);
    Span {
        source_id,
        start,
        end,
        line,
        col,
    }
}

/// Read from tokens using an explicit stack.
/// Stack entries: (open_delim, items, start_pos_of_delim).
fn read_from_tokens(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<Token>>,
    source_id: SourceId,
    input: &str,
) -> Result<(Sexp, bool), ReaderError> {
    let mut stack: Vec<(String, Vector<Sexp>, BytePos)> = Vec::new();

    // Precompute line starts once so every span gets a real line/column.
    let line_starts = crate::span::line_starts_of(input);
    let span_of = |start: BytePos, end: BytePos| {
        let (line, col) = crate::span::line_col_of(input, &line_starts, start.0);
        Span {
            source_id,
            start,
            end,
            line,
            col,
        }
    };

    while let Some(token) = tokens.next() {
        let start = token.start;
        let end = token.end();

        match token.text.as_str() {
            "'" => {
                if tokens.peek().is_none()
                    || tokens
                        .peek()
                        .is_some_and(|t| matches!(t.text.as_str(), ")" | "]" | "}"))
                {
                    return Err(ReaderError::MissingQuoteExpr.at(span_of(start, end)));
                }
                // Reader macro: 'x → (quote x)
                let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                let inner_end = tokens.peek().map_or(BytePos(input.len()), |t| t.start);
                let inner_span = inner.span().unwrap_or(span_of(start, inner_end));
                let sym = Sexp::Symbol("quote".into(), Some(span_of(start, start)));
                let quoted = Sexp::List(vector![sym, inner], Some(span_of(start, inner_span.end)));
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
                                None => {
                                    return Err(ReaderError::UnexpectedEOF
                                        .at(span_of(start, BytePos(input.len()))));
                                }
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                                    last_end = inner.span().map(|s| s.end).unwrap_or(last_end);
                                    items.push_back(inner);
                                }
                            }
                        }
                        let vec = Sexp::Vector(items, Some(span_of(list_start, last_end)));
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
                                None => {
                                    return Err(ReaderError::UnexpectedEOF
                                        .at(span_of(start, BytePos(input.len()))));
                                }
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens, source_id, input)?;
                                    last_end = inner.span().map(|s| s.end).unwrap_or(last_end);
                                    items.push_back(inner);
                                }
                            }
                        }
                        let sexp = Sexp::List(items, Some(span_of(set_start, last_end)));
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
                            _ if name.chars().count() == 1 => name.chars().next().unwrap(),
                            _ => {
                                return Err(ReaderError::UnexpectedToken(name.to_string())
                                    .at(span_of(start, tok.end())));
                            }
                        };
                        let ch_span = Some(span_of(start, tok.end()));
                        let val = Sexp::Char(ch, ch_span);
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(val);
                        } else {
                            return Ok((val, tokens.peek().is_some()));
                        }
                    }
                    Some(other) => {
                        return Err(ReaderError::UnexpectedToken(other.text.clone())
                            .at(span_of(start, other.end())));
                    }
                    None => {
                        return Err(ReaderError::UnexpectedEOF.at(span_of(start, end)));
                    }
                }
            }
            "(" | "[" | "{" => {
                stack.push((token.text.clone(), Vector::new(), start));
            }
            ")" | "]" | "}" => {
                let (open, items, open_start) = stack.pop().ok_or_else(|| {
                    ReaderError::UnexpectedToken(token.text.clone()).at(span_of(start, end))
                })?;
                if (open == "(" && token.text != ")")
                    || (open == "[" && token.text != "]")
                    || (open == "{" && token.text != "}")
                {
                    return Err(ReaderError::UnexpectedToken(token.text).at(span_of(start, end)));
                }
                let span = Some(span_of(open_start, end));
                let val = match open.as_str() {
                    "(" => Sexp::List(items, span),
                    "[" => Sexp::Vector(items, span),
                    "{" => {
                        if items.len() % 2 != 0 {
                            return Err(ReaderError::OddMapElements.at(span_of(open_start, end)));
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
                let span = Some(span_of(start, end));
                let val = parse_atom(&token.text, span).map_err(|e| e.at(span_of(start, end)))?;
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(val);
                } else {
                    return Ok((val, tokens.peek().is_some()));
                }
            }
        }
    }

    if !stack.is_empty() {
        Err(ReaderError::UnexpectedEOF.at(span_of(stack.last().unwrap().2, BytePos(input.len()))))
    } else {
        // Input was empty or all whitespace — return Nil
        Ok((Sexp::Nil, false))
    }
}

fn parse_atom(token: &str, span: Option<Span>) -> Result<Sexp, ReaderError> {
    Ok(if token.starts_with('"') {
        let escaped_last_quote = token.as_bytes()[..token.len().saturating_sub(1)]
            .iter()
            .rev()
            .take_while(|c| **c == b'\\')
            .count()
            % 2
            == 1;
        if token.len() < 2 || !token.ends_with('"') || escaped_last_quote {
            return Err(ReaderError::MalformedString("unterminated literal".into()));
        }
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
                    None => return Err(ReaderError::MalformedString("dangling escape".into())),
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
    })
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
        // A located error is the real contract: the reader reports where
        // the input ran out, not merely that it did.
        let error = parse_for_test("(").expect_err("unbalanced open should fail");
        assert!(
            error.span().is_some(),
            "reader error should carry a location"
        );
        assert!(
            matches!(error.root_cause(), ReaderError::UnexpectedEOF),
            "got {error:?}"
        );
    }

    #[test]
    fn test_unexpected_token() {
        let error = parse_for_test(")").expect_err("stray close should fail");
        assert!(
            error.span().is_some(),
            "reader error should carry a location"
        );
        assert!(
            matches!(error.root_cause(), ReaderError::UnexpectedToken(token) if token == ")"),
            "got {error:?}"
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
        assert_eq!(
            parse_for_test(":foo"),
            Ok(Sexp::Keyword("foo".into(), None))
        );
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
                vector![Sexp::Symbol("quote".into(), None), Sexp::Integer(42, None),],
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
        // `read` parses exactly one form and refuses trailing tokens, so
        // the input here is a comment between one symbol and end of
        // input. The comment must be consumed rather than surfaced as a
        // token. `read` strips spans, so the range check goes through
        // `read_with_source`, which is the path a real caller uses.
        let form = parse_for_test("a ; comment\n").expect("comment should be consumed");
        assert!(
            matches!(&form, Sexp::Symbol(name, _) if name == "a"),
            "got {form:?}"
        );

        let located = read_with_source("a ; comment\n", SourceId(3)).expect("located read");
        let span = located.span().expect("located form should carry a span");
        assert_eq!(
            span.end.0 - span.start.0,
            1,
            "symbol span must not include the comment"
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
