/// A Lisp symbol, optionally qualified with a namespace.
///
/// In source code, symbols are written as:
/// - `foo` → Symbol { name: "foo", ns: None }
/// - `math/pi` → Symbol { name: "pi", ns: Some("math") }
/// - `/` → Symbol { name: "/", ns: None }  (a bare slash is a symbol name)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub name: String,
    pub ns: Option<String>,
}

impl Symbol {
    pub fn new(name: impl Into<String>) -> Self {
        Symbol {
            name: name.into(),
            ns: None,
        }
    }

    pub fn qualified(ns: impl Into<String>, name: impl Into<String>) -> Self {
        Symbol {
            name: name.into(),
            ns: Some(ns.into()),
        }
    }

    /// Parse a symbol from a string, recognizing `ns/name` syntax.
    pub fn parse(s: &str) -> Self {
        if let Some(slash_pos) = s.find('/') {
            let before = &s[..slash_pos];
            let after = &s[slash_pos + 1..];
            // A lone `/` is a valid symbol name, not a qualifier
            if before.is_empty() || after.is_empty() {
                return Symbol::new(s);
            }
            Symbol {
                name: after.to_string(),
                ns: Some(before.to_string()),
            }
        } else {
            Symbol::new(s)
        }
    }

    /// Full display: "name" or "ns/name".
    pub fn full_name(&self) -> String {
        match &self.ns {
            Some(ns) => format!("{}/{}", ns, self.name),
            None => self.name.clone(),
        }
    }

    /// True if this symbol has a namespace qualifier.
    pub fn is_qualified(&self) -> bool {
        self.ns.is_some()
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.full_name())
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Symbol::parse(s)
    }
}

impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Symbol::parse(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_symbol() {
        let s = Symbol::new("foo");
        assert_eq!(s.name, "foo");
        assert_eq!(s.ns, None);
        assert_eq!(s.to_string(), "foo");
    }

    #[test]
    fn test_qualified_symbol() {
        let s = Symbol::qualified("math", "pi");
        assert_eq!(s.name, "pi");
        assert_eq!(s.ns, Some("math".into()));
        assert_eq!(s.to_string(), "math/pi");
    }

    #[test]
    fn test_parse_simple() {
        let s = Symbol::parse("foo");
        assert_eq!(s.name, "foo");
        assert!(s.ns.is_none());
    }

    #[test]
    fn test_parse_qualified() {
        let s = Symbol::parse("math/pi");
        assert_eq!(s.name, "pi");
        assert_eq!(s.ns, Some("math".into()));
    }

    #[test]
    fn test_parse_bare_slash() {
        // A bare `/` is a symbol named "/"
        let s = Symbol::parse("/");
        assert_eq!(s.name, "/");
        assert!(s.ns.is_none());
    }

    #[test]
    fn test_parse_trailing_slash() {
        // `foo/` — nothing after slash, treat as symbol "foo/"
        let s = Symbol::parse("foo/");
        assert_eq!(s.name, "foo/");
        assert!(s.ns.is_none());
    }

    #[test]
    fn test_parse_leading_slash() {
        // `/foo` — nothing before slash, treat as symbol "/foo"
        let s = Symbol::parse("/foo");
        assert_eq!(s.name, "/foo");
        assert!(s.ns.is_none());
    }

    #[test]
    fn test_display() {
        assert_eq!(Symbol::new("x").to_string(), "x");
        assert_eq!(Symbol::qualified("core", "println").to_string(), "core/println");
    }

    #[test]
    fn test_hash_eq() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Symbol::new("x"));
        set.insert(Symbol::new("x"));
        assert_eq!(set.len(), 1);
        set.insert(Symbol::qualified("ns", "x"));
        assert_eq!(set.len(), 2);
    }
}