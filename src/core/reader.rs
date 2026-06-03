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
    input
        .replace('(', " ( ")
        .replace(')', " ) ")
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

fn read_from_tokens(
    tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
) -> Result<Value, ReaderError> {
    let token = tokens.next().ok_or(ReaderError::UnexpectedEOF)?;

    match token.as_str() {
        "(" => {
            let mut list = im::Vector::new();
            while tokens.peek().map(|s| s.as_str()) != Some(")") {
                list.push_back(read_from_tokens(tokens)?);
            }
            tokens.next(); // Consume ")"
            Ok(Value::List(list))
        }
        ")" => Err(ReaderError::UnexpectedToken(")".to_string())),
        _ => Ok(parse_atom(&token)),
    }
}

fn parse_atom(token: &str) -> Value {
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
}
