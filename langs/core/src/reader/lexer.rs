use crate::span::BytePos;

/// A token produced by the tokenizer, carrying its byte offset in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start: BytePos,
}

impl Token {
    pub fn new(text: String, start: BytePos) -> Self {
        Token { text, start }
    }

    /// Byte offset of the first byte after this token.
    pub fn end(&self) -> BytePos {
        BytePos(self.start.0 + self.text.len())
    }
}

/// Tokenize the input string into a sequence of tokens with byte positions.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some(&(pos, c)) = chars.peek() {
        let start = BytePos(pos);
        match c {
            '(' | ')' | '[' | ']' | '{' | '}' | '\'' => {
                tokens.push(Token::new(c.to_string(), start));
                chars.next();
            }
            // Dispatch macro: #( `#{` `#\` etc.
            '#' => {
                chars.next(); // consume the '#'
                match chars.peek().map(|&(_, c)| c) {
                    Some('(') | Some('{') | Some('\\') => {
                        // Emit '#' as a separate token, the reader handles the dispatch
                        tokens.push(Token::new("#".to_string(), start));
                    }
                    _ => {
                        // Treat as an atom (e.g., #t, #_foo, etc.)
                        let mut atom = "#".to_string();
                        while let Some(&(_, c)) = chars.peek() {
                            if c.is_whitespace() || "()[]{}'\"".contains(c) || c == ';' {
                                break;
                            }
                            atom.push(chars.next().unwrap().1);
                        }
                        tokens.push(Token::new(atom, start));
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                s.push(chars.next().unwrap().1); // opening "
                while let Some(&(_, c)) = chars.peek() {
                    match c {
                        '\\' => {
                            s.push(chars.next().unwrap().1);
                            if let Some((_, next_c)) = chars.next() {
                                s.push(next_c);
                            }
                        }
                        '"' => {
                            s.push(chars.next().unwrap().1);
                            break;
                        }
                        _ => {
                            s.push(chars.next().unwrap().1);
                        }
                    }
                }
                tokens.push(Token::new(s, start));
            }
            _ if c.is_whitespace() => {
                chars.next();
            }
            ';' => {
                // Comment, skip until newline
                while let Some((_, c)) = chars.next() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {
                let mut atom = String::new();
                while let Some(&(_, c)) = chars.peek() {
                    if c.is_whitespace() || "()[]{}'\"#".contains(c) || c == ';' {
                        break;
                    }
                    atom.push(chars.next().unwrap().1);
                }
                if !atom.is_empty() {
                    tokens.push(Token::new(atom, start));
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
        let toks = tokenize("(a b)");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].text, "(");
        assert_eq!(toks[1].text, "a");
        assert_eq!(toks[2].text, "b");
        assert_eq!(toks[3].text, ")");
        // Byte positions
        assert_eq!(toks[0].start, BytePos(0));
        assert_eq!(toks[1].start, BytePos(1));
        assert_eq!(toks[2].start, BytePos(3));
        assert_eq!(toks[3].start, BytePos(4));
    }

    #[test]
    fn test_tokenize_vectors() {
        let toks = tokenize("[1 2]");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].text, "[");
        assert_eq!(toks[1].text, "1");
        assert_eq!(toks[2].text, "2");
        assert_eq!(toks[3].text, "]");
    }

    #[test]
    fn test_tokenize_maps() {
        let toks = tokenize("{:a 1}");
        assert_eq!(toks.len(), 4);
        assert_eq!(toks[0].text, "{");
        assert_eq!(toks[1].text, ":a");
        assert_eq!(toks[2].text, "1");
        assert_eq!(toks[3].text, "}");
    }

    #[test]
    fn test_tokenize_quote() {
        let toks = tokenize("'x");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].text, "'");
        assert_eq!(toks[1].text, "x");
    }

    #[test]
    fn test_tokenize_string() {
        let toks = tokenize(r#""hello world""#);
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].text, r#""hello world""#);
    }

    #[test]
    fn test_tokenize_comment() {
        let toks = tokenize("a ; comment\n b");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].text, "a");
        assert_eq!(toks[1].text, "b");
    }

    #[test]
    fn test_tokenize_nested() {
        let toks = tokenize("(+ 1 (* 2 3))");
        assert_eq!(toks.len(), 9);
        let texts: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["(", "+", "1", "(", "*", "2", "3", ")", ")"]);
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
        assert_eq!(toks[0].text, "(");
        assert_eq!(toks[1000].text, "1");
        assert_eq!(toks[2000].text, ")");
    }
}
