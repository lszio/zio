use im::{HashMap, Vector};
use std::hash::{Hash, Hasher};

use crate::span::Span;

/// Pure syntax tree — the output of the reader, the input to the evaluator.
/// Unlike `Value`, `Sexp` contains no runtime types (Function, Macro, etc.).
/// Each variant (except Nil and Boolean) carries an optional source span
/// for error reporting. ADR-004.
#[derive(Debug, Clone, PartialEq)]
pub enum Sexp {
    Nil,
    Boolean(bool),
    Integer(i64, Option<Span>),
    Float(f64, Option<Span>),
    String(String, Option<Span>),
    Symbol(String, Option<Span>),
    Keyword(String, Option<Span>),
    List(Vector<Sexp>, Option<Span>),
    Vector(Vector<Sexp>, Option<Span>),
    Map(HashMap<Sexp, Sexp>, Option<Span>),
    Char(char, Option<Span>),
}

impl Eq for Sexp {}

impl Hash for Sexp {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Span is excluded from hash — same value at different positions is equal.
        std::mem::discriminant(self).hash(state);
        match self {
            Sexp::Nil => {}
            Sexp::Boolean(b) => b.hash(state),
            Sexp::Integer(i, _) => i.hash(state),
            Sexp::Float(f, _) => f.to_bits().hash(state),
            Sexp::String(s, _) => s.hash(state),
            Sexp::Symbol(s, _) => s.hash(state),
            Sexp::Keyword(k, _) => k.hash(state),
            Sexp::List(l, _) => l.hash(state),
            Sexp::Vector(v, _) => v.hash(state),
            Sexp::Map(m, _) => m.hash(state),
            Sexp::Char(c, _) => c.hash(state),
        }
    }
}

impl std::fmt::Display for Sexp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Sexp::Nil => write!(f, "nil"),
            Sexp::Boolean(b) => write!(f, "{b}"),
            Sexp::Integer(i, _) => write!(f, "{i}"),
            Sexp::Float(fl, _) => write!(f, "{fl}"),
            Sexp::String(s, _) => write!(f, "{s:?}"),
            Sexp::Symbol(s, _) => write!(f, "{s}"),
            Sexp::Keyword(k, _) => write!(f, ":{k}"),
            Sexp::List(l, _) => {
                write!(f, "(")?;
                for (i, val) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, ")")
            }
            Sexp::Vector(v, _) => {
                write!(f, "[")?;
                for (i, val) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, "]")
            }
            Sexp::Map(m, _) => {
                write!(f, "{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k} {v}")?;
                }
                write!(f, "}}")
            }
            Sexp::Char(c, _) => write!(f, "#\\{c}"),
        }
    }
}

impl Sexp {
    /// Returns a human-readable type name for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Sexp::Nil => "nil",
            Sexp::Boolean(_) => "boolean",
            Sexp::Integer(_, _) | Sexp::Float(_, _) => "number",
            Sexp::String(_, _) => "string",
            Sexp::Symbol(_, _) => "symbol",
            Sexp::Keyword(_, _) => "keyword",
            Sexp::List(_, _) => "list",
            Sexp::Vector(_, _) => "vector",
            Sexp::Map(_, _) => "map",
            Sexp::Char(_, _) => "character",
        }
    }

    /// Returns the source span for this node, if available.
    pub fn span(&self) -> Option<Span> {
        match self {
            Sexp::Nil | Sexp::Boolean(_) => None,
            Sexp::Integer(_, s) | Sexp::Float(_, s) | Sexp::String(_, s)
            | Sexp::Symbol(_, s) | Sexp::Keyword(_, s) | Sexp::List(_, s)
            | Sexp::Vector(_, s) | Sexp::Map(_, s) | Sexp::Char(_, s) => *s,
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
