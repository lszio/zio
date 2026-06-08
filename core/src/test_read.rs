/// Minimal S-expression reader for use in core crate tests.
/// This avoids a circular dev-dependency on zio-reader.
use crate::error::ReaderError;
use crate::sexp::Sexp;
use im::vector;
use im::Vector;

pub fn read(input: &str) -> Result<Sexp, ReaderError> {
    let tokens = tokenize(input);
    let mut tokens = tokens.into_iter().peekable();
    let (sexp, _) = read_from_tokens(&mut tokens)?;
    Ok(sexp)
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '(' | ')' | '[' | ']' | '{' | '}' | '\'' => {
                tokens.push(c.to_string());
                chars.next();
            }
            '"' => {
                let mut s = String::new();
                s.push(chars.next().unwrap());
                while let Some(&c) = chars.peek() {
                    match c {
                        '\\' => {
                            s.push(chars.next().unwrap());
                            if let Some(nc) = chars.next() {
                                s.push(nc);
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
                while let Some(c) = chars.next() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {
                let mut atom = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || "()[]{}'\"".contains(c) || c == ';' {
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
) -> Result<(Sexp, bool), ReaderError> {
    let mut stack: Vec<(String, Vector<Sexp>)> = Vec::new();
    while let Some(token) = tokens.next() {
        match token.as_str() {
            "'" => {
                let (inner, _) = read_from_tokens(tokens)?;
                let quoted = Sexp::List(vector![Sexp::Symbol("quote".into()), inner]);
                if let Some(parent) = stack.last_mut() {
                    parent.1.push_back(quoted);
                } else {
                    return Ok((quoted, tokens.peek().is_some()));
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
        Ok((Sexp::Nil, false))
    }
}

fn parse_atom(token: &str) -> Sexp {
    if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        let s = &token[1..token.len() - 1];
        let mut unescaped = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(nc) = chars.next() {
                    match nc {
                        'n' => unescaped.push('\n'),
                        'r' => unescaped.push('\r'),
                        't' => unescaped.push('\t'),
                        '\\' => unescaped.push('\\'),
                        '"' => unescaped.push('"'),
                        _ => unescaped.push(nc),
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