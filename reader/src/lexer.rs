/// Tokenizer: converts a source string into a flat vector of tokens.
/// Each token is a raw string: parentheses, brackets, braces, quote mark,
/// quoted strings (including delimiters), or atoms.

/// Tokenize the input string into a sequence of token strings.
pub fn tokenize(input: &str) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_empty() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn test_tokenize_parens() {
        assert_eq!(tokenize("(a b)"), vec!["(", "a", "b", ")"]);
    }

    #[test]
    fn test_tokenize_vectors() {
        assert_eq!(tokenize("[1 2]"), vec!["[", "1", "2", "]"]);
    }

    #[test]
    fn test_tokenize_maps() {
        assert_eq!(tokenize("{:a 1}"), vec!["{", ":a", "1", "}"]);
    }

    #[test]
    fn test_tokenize_quote() {
        assert_eq!(tokenize("'x"), vec!["'", "x"]);
    }

    #[test]
    fn test_tokenize_string() {
        let toks = tokenize(r#""hello world""#);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0], r#""hello world""#);
    }

    #[test]
    fn test_tokenize_comment() {
        assert_eq!(tokenize("a ; comment\n b"), vec!["a", "b"]);
    }

    #[test]
    fn test_tokenize_nested() {
        assert_eq!(
            tokenize("(+ 1 (* 2 3))"),
            vec!["(", "+", "1", "(", "*", "2", "3", ")", ")"]
        );
    }

    #[test]
    fn test_tokenize_deep() {
        let mut input = String::new();
        for _ in 0..1000 {
            input.push('(');
        }
        input.push('1');
        for _ in 0..1000 {
            input.push(')');
        }
        let toks = tokenize(&input);
        assert_eq!(toks.len(), 2001);
        assert_eq!(toks[0], "(");
        assert_eq!(toks[1000], "1");
        assert_eq!(toks[2000], ")");
    }
}