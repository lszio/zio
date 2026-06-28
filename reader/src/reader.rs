use im::{vector, Vector};

use zio_core::error::ReaderError;
use zio_core::sexp::Sexp;
use crate::lexer::tokenize;

/// Read a single S-expression from the input string.
pub fn read(input: &str) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens)?;
    Ok(sexp)
}

/// Read from tokens using an explicit stack.
/// Returns (Sexp, bool) where bool indicates if more tokens were available.
fn read_from_tokens(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
) -> Result<(Sexp, bool), ReaderError> {
    let mut stack: Vec<(String, Vector<Sexp>)> = Vec::new();

    while let Some(token) = tokens.next() {
        match token.as_str() {
            "'" => {
                // Reader macro: 'x → (quote x)
                let (inner, _) = read_from_tokens(tokens)?;
                let quoted = Sexp::List(vector![Sexp::Symbol("quote".into()), inner]);
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(quoted);
                } else {
                    return Ok((quoted, tokens.peek().is_some()));
                }
            }
            "#" => {
                // Dispatch reader macro: #( → vector, #{ → set
                match tokens.next().as_deref() {
                    Some("(") => {
                        // #(1 2 3) → vector [1 2 3]
                        // Manually read items until matching ")"
                        let mut items = Vector::new();
                        loop {
                            match tokens.peek().map(|s| s.as_str()) {
                                Some(")") => {
                                    tokens.next();
                                    break;
                                }
                                None => return Err(ReaderError::UnexpectedEOF),
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens)?;
                                    items.push_back(inner);
                                }
                            }
                        }
                        let vec = Sexp::Vector(items);
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(vec);
                        } else {
                            return Ok((vec, tokens.peek().is_some()));
                        }
                    }
                    Some("{") => {
                        // #{a b c} → (set a b c)
                        // Manually read items until matching "}"
                        let mut items = Vector::new();
                        items.push_back(Sexp::Symbol("set".into()));
                        loop {
                            match tokens.peek().map(|s| s.as_str()) {
                                Some("}") => {
                                    tokens.next();
                                    break;
                                }
                                None => return Err(ReaderError::UnexpectedEOF),
                                _ => {
                                    let (inner, _) = read_from_tokens(tokens)?;
                                    items.push_back(inner);
                                }
                            }
                        }
                        let sexp = Sexp::List(items);
                        if let Some(parent) = stack.last_mut() {
                            parent.1.push_back(sexp);
                        } else {
                            return Ok((sexp, tokens.peek().is_some()));
                        }
                    }
                    Some(other) => {
                        return Err(ReaderError::UnexpectedToken(other.to_string()));
                    }
                    None => {
                        return Err(ReaderError::UnexpectedEOF);
                    }
                }
            }
            "(" | "[" | "{" => {
                stack.push((token, Vector::new()));
            }
            ")" | "]" | "}" => {
                let (open, items) = stack
                    .pop()
                    .ok_or_else(|| ReaderError::UnexpectedToken(token.clone()))?;
                if (open == "(" && token != ")")
                    || (open == "[" && token != "]")
                    || (open == "{" && token != "}")
                {
                    return Err(ReaderError::UnexpectedToken(token));
                }
                let val = match open.as_str() {
                    "(" => Sexp::List(items),
                    "[" => Sexp::Vector(items),
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
                        Sexp::Map(map)
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
                let val = parse_atom(&token);
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

fn parse_atom(token: &str) -> Sexp {
    if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        // Basic unescaping for common characters
        let s = &token[1..token.len() - 1];
        let mut unescaped = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next_c) = chars.next() {
                    match next_c {
                        'n' => unescaped.push('\n'),
                        'r' => unescaped.push('\r'),
                        't' => unescaped.push('\t'),
                        '\\' => unescaped.push('\\'),
                        '"' => unescaped.push('"'),
                        _ => unescaped.push(next_c),
                    }
                }
            } else {
                unescaped.push(c);
            }
        }
        Sexp::String(unescaped)
    } else if token.starts_with(':') {
        Sexp::Keyword(token[1..].to_string())
    } else {
        match token {
            "nil" => Sexp::Nil,
            "true" => Sexp::Boolean(true),
            "false" => Sexp::Boolean(false),
            _ => {
                if let Ok(i) = token.parse::<i64>() {
                    Sexp::Integer(i)
                } else if let Ok(f) = token.parse::<f64>() {
                    Sexp::Float(f)
                } else {
                    Sexp::Symbol(token.to_string())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_integer() {
        assert_eq!(read("42"), Ok(Sexp::Integer(42)));
    }

    #[test]
    fn test_read_symbol() {
        assert_eq!(read("foo"), Ok(Sexp::Symbol("foo".into())));
    }

    #[test]
    fn test_read_nested_list() {
        assert_eq!(
            read("(+ 1 (* 2 3))"),
            Ok(Sexp::List(vector![
                Sexp::Symbol("+".into()),
                Sexp::Integer(1),
                Sexp::List(vector![
                    Sexp::Symbol("*".into()),
                    Sexp::Integer(2),
                    Sexp::Integer(3)
                ])
            ]))
        );
    }

    #[test]
    fn test_read_float() {
        assert_eq!(read("3.14"), Ok(Sexp::Float(3.14)));
    }

    #[test]
    fn test_read_nil() {
        assert_eq!(read("nil"), Ok(Sexp::Nil));
    }

    #[test]
    fn test_read_boolean() {
        assert_eq!(read("true"), Ok(Sexp::Boolean(true)));
        assert_eq!(read("false"), Ok(Sexp::Boolean(false)));
    }

    #[test]
    fn test_unexpected_eof() {
        assert_eq!(read("("), Err(ReaderError::UnexpectedEOF));
    }

    #[test]
    fn test_unexpected_token() {
        assert_eq!(
            read(")"),
            Err(ReaderError::UnexpectedToken(")".to_string()))
        );
    }

    #[test]
    fn test_read_string() {
        assert_eq!(
            read("\"hello (world)\""),
            Ok(Sexp::String("hello (world)".into()))
        );
    }

    #[test]
    fn test_read_keyword() {
        assert_eq!(read(":foo"), Ok(Sexp::Keyword("foo".into())));
    }

    #[test]
    fn test_read_vector() {
        assert_eq!(
            read("[1 2 3]"),
            Ok(Sexp::Vector(vector![
                Sexp::Integer(1),
                Sexp::Integer(2),
                Sexp::Integer(3)
            ]))
        );
    }

    #[test]
    fn test_read_map() {
        use im::hashmap;
        assert_eq!(
            read("{:a 1 :b 2}"),
            Ok(Sexp::Map(hashmap! {
                Sexp::Keyword("a".into()) => Sexp::Integer(1),
                Sexp::Keyword("b".into()) => Sexp::Integer(2)
            }))
        );
    }

    #[test]
    fn test_deep_recursion() {
        let mut deep = String::new();
        for _ in 0..1000 {
            deep.push_str("(");
        }
        deep.push_str("1");
        for _ in 0..1000 {
            deep.push_str(")");
        }
        assert!(read(&deep).is_ok());
    }

    #[test]
    fn test_quote_reader_macro() {
        assert_eq!(
            read("'42"),
            Ok(Sexp::List(vector![
                Sexp::Symbol("quote".into()),
                Sexp::Integer(42)
            ]))
        );
    }

    #[test]
    fn test_quote_list() {
        assert_eq!(
            read("'(1 2 3)"),
            Ok(Sexp::List(vector![
                Sexp::Symbol("quote".into()),
                Sexp::List(vector![
                    Sexp::Integer(1),
                    Sexp::Integer(2),
                    Sexp::Integer(3)
                ])
            ]))
        );
    }

    #[test]
    fn test_quote_nested() {
        assert_eq!(
            read("''x"),
            Ok(Sexp::List(vector![
                Sexp::Symbol("quote".into()),
                Sexp::List(vector![
                    Sexp::Symbol("quote".into()),
                    Sexp::Symbol("x".into())
                ])
            ]))
        );
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(read(""), Ok(Sexp::Nil));
        assert_eq!(read("   "), Ok(Sexp::Nil));
    }

    #[test]
    fn test_hash_paren_vector() {
        assert_eq!(
            read("#(1 2 3)"),
            Ok(Sexp::Vector(vector![
                Sexp::Integer(1),
                Sexp::Integer(2),
                Sexp::Integer(3)
            ]))
        );
    }

    #[test]
    fn test_hash_paren_empty() {
        assert_eq!(
            read("#()"),
            Ok(Sexp::Vector(Vector::new()))
        );
    }

    #[test]
    fn test_hash_brace_set() {
        // #{a b c} → (set a b c)
        assert_eq!(
            read("#{a b c}"),
            Ok(Sexp::List(vector![
                Sexp::Symbol("set".into()),
                Sexp::Symbol("a".into()),
                Sexp::Symbol("b".into()),
                Sexp::Symbol("c".into())
            ]))
        );
    }
    #[test]
    fn test_read_core_stdlib_do_wrapped() {
        let content = include_str!("../../cli/stdlib/zio/core.zio");
        let wrapped = format!("(do\n{content}\n)");
        let result = read(&wrapped);
        assert!(result.is_ok(), "failed to parse do-wrapped stdlib: {:?}", result.err());
        let sexp = result.unwrap();
        match &sexp {
            Sexp::List(v) => {
                assert!(!v.is_empty(), "empty list from do wrapper");
                assert_eq!(v[0], Sexp::Symbol("do".into()));
            }
            other => panic!("expected list (do ...), got {:?}", other),
        }
    }
}
