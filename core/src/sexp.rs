use im::{HashMap, Vector};
use std::hash::{Hash, Hasher};

use crate::span::Span;

/// Pure syntax tree — the output of the reader, the input to the evaluator.
/// Unlike `Value`, `Sexp` contains no runtime types (Function, Macro, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum Sexp {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
    Keyword(String),
    List(Vector<Sexp>),
    Vector(Vector<Sexp>),
    Map(HashMap<Sexp, Sexp>),
}

impl Eq for Sexp {}

impl Hash for Sexp {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Sexp::Nil => {}
            Sexp::Boolean(b) => b.hash(state),
            Sexp::Integer(i) => i.hash(state),
            Sexp::Float(f) => f.to_bits().hash(state),
            Sexp::String(s) => s.hash(state),
            Sexp::Symbol(s) => s.hash(state),
            Sexp::Keyword(k) => k.hash(state),
            Sexp::List(l) => l.hash(state),
            Sexp::Vector(v) => v.hash(state),
            Sexp::Map(m) => m.hash(state),
        }
    }
}

impl std::fmt::Display for Sexp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sexp::Nil => write!(f, "nil"),
            Sexp::Boolean(b) => write!(f, "{b}"),
            Sexp::Integer(i) => write!(f, "{i}"),
            Sexp::Float(fl) => write!(f, "{fl}"),
            Sexp::String(s) => write!(f, "{s:?}"),
            Sexp::Symbol(s) => write!(f, "{s}"),
            Sexp::Keyword(k) => write!(f, ":{k}"),
            Sexp::List(l) => {
                write!(f, "(")?;
                for (i, val) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, ")")
            }
            Sexp::Vector(v) => {
                write!(f, "[")?;
                for (i, val) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, "]")
            }
            Sexp::Map(m) => {
                write!(f, "{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k} {v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl Sexp {
    /// Returns a human-readable type name for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Sexp::Nil => "nil",
            Sexp::Boolean(_) => "boolean",
            Sexp::Integer(_) | Sexp::Float(_) => "number",
            Sexp::String(_) => "string",
            Sexp::Symbol(_) => "symbol",
            Sexp::Keyword(_) => "keyword",
            Sexp::List(_) => "list",
            Sexp::Vector(_) => "vector",
            Sexp::Map(_) => "map",
        }
    }
}

/// Span-aware Sexp — an AST node with source location.
/// Use this when you need error messages with source locations.
/// Phase 2 goal: merge this into Sexp itself.
#[derive(Debug, Clone)]
pub struct SpannedSexp {
    pub node: Sexp,
    pub span: Span,
}

impl SpannedSexp {
    pub fn new(node: Sexp, span: Span) -> Self {
        SpannedSexp { node, span }
    }

    pub fn dummy(node: Sexp) -> Self {
        SpannedSexp { node, span: Span::DUMMY }
    }
}