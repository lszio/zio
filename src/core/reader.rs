use thiserror::Error;
use crate::core::value::Value;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ReaderError {
    #[error("Unexpected EOF")]
    UnexpectedEOF,
    #[error("Unexpected token: {0}")]
    UnexpectedToken(String),
}

pub fn read(input: &str) -> Result<Value, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    read_from_tokens(&mut tokens)
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            '(' | ')' | '[' | ']' | '{' | '}' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '"' => {
                let mut s = String::new();
                s.push(chars.next().unwrap()); // opening "
                while let Some(&c) = chars.peek() {
                    match c {
                        '\\' => {
                            s.push(chars.next().unwrap());
                            if let Some(next_c) = chars.next() {
                                s.push(next_c);
                            }
                        }
                        '"' => {
                            s.push(chars.next().unwrap());
                            break;
                        }
                        _ => {
                            s.push(chars.next().unwrap());
                        }
                    }
                }
                tokens.push(s);
            }
            _ if c.is_whitespace() => {
                chars.next();
            }
            ';' => {
                // Comment, skip until newline
                while let Some(c) = chars.next() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {
                let mut atom = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || "()[]{}".contains(c) || c == '"' || c == ';' {
                        break;
                    }
                    atom.push(chars.next().unwrap());
                }
                if !atom.is_empty() {
                    tokens.push(atom);
                }
            }
        }
    }
    tokens
}

fn read_from_tokens(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
) -> Result<Value, ReaderError> {
    let mut stack: Vec<(String, im::Vector<Value>)> = Vec::new();

    while let Some(token) = tokens.next() {
        match token.as_str() {
            "(" | "[" | "{" => {
                stack.push((token, im::Vector::new()));
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
                    "(" => Value::List(items),
                    "[" => Value::Vector(items),
                    "{" => {
                        if items.len() % 2 != 0 {
                            return Err(ReaderError::UnexpectedToken(
                                "Map must have even number of elements".to_string(),
                            ));
                        }
                        let mut map = im::HashMap::new();
                        let mut iter = items.into_iter();
                        while let Some(key) = iter.next() {
                            let val = iter.next().unwrap();
                            map.insert(key, val);
                        }
                        Value::Map(map)
                    }
                    _ => unreachable!(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(val);
                } else {
                    return Ok(val);
                }
            }
            _ => {
                let val = parse_atom(&token);
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(val);
                } else {
                    return Ok(val);
                }
            }
        }
    }

    if !stack.is_empty() {
        Err(ReaderError::UnexpectedEOF)
    } else {
        Err(ReaderError::UnexpectedEOF) // Should have returned an atom or popped the last collection
    }
}

fn parse_atom(token: &str) -> Value {
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
        Value::String(unescaped)
    } else if token.starts_with(':') {
        Value::Keyword(token[1..].to_string())
    } else {
        match token {
            "nil" => Value::Nil,
            "true" => Value::Boolean(true),
            "false" => Value::Boolean(false),
            _ => {
                if let Ok(i) = token.parse::<i64>() {
                    Value::Integer(i)
                } else if let Ok(f) = token.parse::<f64>() {
                    Value::Float(f)
                } else {
                    Value::Symbol(token.to_string())
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
        assert_eq!(read("42"), Ok(Value::Integer(42)));
    }

    #[test]
    fn test_read_symbol() {
        assert_eq!(read("foo"), Ok(Value::Symbol("foo".into())));
    }

    #[test]
    fn test_read_nested_list() {
        use im::vector;
        assert_eq!(
            read("(+ 1 (* 2 3))"),
            Ok(Value::List(vector![
                Value::Symbol("+".into()),
                Value::Integer(1),
                Value::List(vector![
                    Value::Symbol("*".into()),
                    Value::Integer(2),
                    Value::Integer(3)
                ])
            ]))
        );
    }

    #[test]
    fn test_read_float() {
        assert_eq!(read("3.14"), Ok(Value::Float(3.14)));
    }

    #[test]
    fn test_read_nil() {
        assert_eq!(read("nil"), Ok(Value::Nil));
    }

    #[test]
    fn test_read_boolean() {
        assert_eq!(read("true"), Ok(Value::Boolean(true)));
        assert_eq!(read("false"), Ok(Value::Boolean(false)));
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
        assert_eq!(read("\"hello (world)\""), Ok(Value::String("hello (world)".into())));
    }

    #[test]
    fn test_read_keyword() {
        // Current implementation treats :foo as a symbol
        assert_eq!(read(":foo"), Ok(Value::Keyword("foo".into())));
    }

    #[test]
    fn test_robust_tokenizer() {
        // (1)2 should be read as (1) followed by 2, but read() only reads one Value.
        // If we read (1)2, it should probably be an error or just read (1).
        // Standard Lisps usually stop at the first complete expression.
        assert_eq!(read("(1)2"), Ok(Value::List(im::vector![Value::Integer(1)])));
    }

    #[test]
    fn test_read_vector() {
        use im::vector;
        assert_eq!(
            read("[1 2 3]"),
            Ok(Value::Vector(vector![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3)
            ]))
        );
    }

    #[test]
    fn test_read_map() {
        use im::hashmap;
        assert_eq!(
            read("{:a 1 :b 2}"),
            Ok(Value::Map(hashmap! {
                Value::Keyword("a".into()) => Value::Integer(1),
                Value::Keyword("b".into()) => Value::Integer(2)
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
}
