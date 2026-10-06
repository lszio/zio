//! A source-preserving syntax representation.
//!
//! `Sexp` already carries spans, but a `Value` does not, so code handed
//! to a macro — or quoted for later analysis — loses where it came from
//! and whether it was written, generated, or expanded. This module is the
//! bridge: a `SyntaxNode` is a `Value`-shaped tagged map
//! (`kind`/`value`/`children`/`span`/`origin`) that keeps all of it.
//!
//! The reverse direction is strict. Only a map with the exact shape comes
//! back as a node, so an ordinary list or a map that merely *claims* a
//! `:kind` is never mistaken for syntax.

use im::HashMap;
use im::Vector;

use crate::sexp::Sexp;
use crate::span::Span;
use crate::value::Value;

/// Where a node came from. Constant quoting, macro-generated code, and a
/// macro call site are three different facts and stay three different
/// facts through the bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Written by hand in a registered source.
    Source,
    /// Produced inside `(quote ...)`: this is data, not a call form.
    Quoted,
    /// Synthesized by a named expander with no call site.
    Generated { by: String },
    /// Expanded from a macro call; the call site is kept for diagnostics.
    MacroCall { macro_name: String, call_site: Option<Span> },
}

impl Origin {
    fn keyword(&self) -> &'static str {
        match self {
            Origin::Source => "source",
            Origin::Quoted => "quoted",
            Origin::Generated { .. } => "generated",
            Origin::MacroCall { .. } => "macro-call",
        }
    }

    fn to_value(self) -> Value {
        let mut map = HashMap::new();
        map.insert(Value::Keyword("by".into()), Value::String(self.detail()));
        map.insert(
            Value::Keyword("call-site".into()),
            self.call_site().map_or(Value::Nil, span_to_value),
        );
        map.insert(
            Value::Keyword("kind".into()),
            Value::Keyword(self.keyword().into()),
        );
        Value::Map(map)
    }

    fn detail(&self) -> String {
        match self {
            Origin::Source | Origin::Quoted => String::new(),
            Origin::Generated { by } => by.clone(),
            Origin::MacroCall { macro_name, .. } => macro_name.clone(),
        }
    }

    fn call_site(&self) -> Option<Span> {
        match self {
            Origin::MacroCall { call_site, .. } => *call_site,
            _ => None,
        }
    }

    /// Rebuild from a tagged map, or `None` when the shape is wrong.
    fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        let kind = match map.get(&Value::Keyword("kind".into()))? {
            Value::Keyword(k) => k.as_str(),
            _ => return None,
        };
        let detail = match map.get(&Value::Keyword("by".into())) {
            Some(Value::String(s)) => s.clone(),
            None | Some(Value::Nil) => String::new(),
            _ => return None,
        };
        let call_site = match map.get(&Value::Keyword("call-site".into())) {
            Some(Value::Map(_)) => span_from_value(map.get(&Value::Keyword("call-site".into()))?),
            _ => None,
        };
        Some(match kind {
            "source" => Origin::Source,
            "quoted" => Origin::Quoted,
            "generated" => Origin::Generated { by: detail },
            "macro-call" => Origin::MacroCall {
                macro_name: detail,
                call_site,
            },
            _ => return None,
        })
    }
}

fn span_to_value(span: Span) -> Value {
    let mut map = HashMap::new();
    map.insert(Value::Keyword("source".into()), Value::Integer(span.source_id.0 as i64));
    map.insert(Value::Keyword("start".into()), Value::Integer(span.start.0 as i64));
    map.insert(Value::Keyword("end".into()), Value::Integer(span.end.0 as i64));
    map.insert(Value::Keyword("line".into()), Value::Integer(span.line as i64));
    map.insert(Value::Keyword("col".into()), Value::Integer(span.col as i64));
    Value::Map(map)
}

fn span_from_value(value: &Value) -> Option<Span> {
    let map = match value {
        Value::Map(m) => m,
        _ => return None,
    };
    let field = |name: &str| -> Option<i64> {
        match map.get(&Value::Keyword(name.into()))? {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    };
    Some(Span::new(
        crate::span::SourceId(field("source")? as usize),
        crate::span::BytePos(field("start")? as usize),
        crate::span::BytePos(field("end")? as usize),
        field("line")? as usize,
        field("col")? as usize,
    ))
}

/// A syntax node: a form plus the provenance that a bare `Value` drops.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxNode {
    pub kind: NodeKind,
    pub value: Option<String>,
    pub children: Vector<SyntaxNode>,
    pub span: Option<Span>,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Nil,
    Boolean,
    Integer,
    Float,
    String,
    Symbol,
    Keyword,
    List,
    Vector,
    Map,
    Char,
}

impl NodeKind {
    fn keyword(&self) -> &'static str {
        match self {
            NodeKind::Nil => "nil",
            NodeKind::Boolean => "boolean",
            NodeKind::Integer => "integer",
            NodeKind::Float => "float",
            NodeKind::String => "string",
            NodeKind::Symbol => "symbol",
            NodeKind::Keyword => "keyword",
            NodeKind::List => "list",
            NodeKind::Vector => "vector",
            NodeKind::Map => "map",
            NodeKind::Char => "char",
        }
    }

    fn from_keyword(k: &str) -> Option<Self> {
        Some(match k {
            "nil" => NodeKind::Nil,
            "boolean" => NodeKind::Boolean,
            "integer" => NodeKind::Integer,
            "float" => NodeKind::Float,
            "string" => NodeKind::String,
            "symbol" => NodeKind::Symbol,
            "keyword" => NodeKind::Keyword,
            "list" => NodeKind::List,
            "vector" => NodeKind::Vector,
            "map" => NodeKind::Map,
            "char" => NodeKind::Char,
            _ => return None,
        })
    }
}

impl SyntaxNode {
    /// Tag a parsed form, keeping its span.
    pub fn from_sexp(sexp: &Sexp) -> Self {
        let (kind, value, children) = match sexp {
            Sexp::Nil => (NodeKind::Nil, None, Vector::new()),
            Sexp::Boolean(b) => (NodeKind::Boolean, Some(b.to_string()), Vector::new()),
            Sexp::Integer(i, _) => (NodeKind::Integer, Some(i.to_string()), Vector::new()),
            Sexp::Float(f, _) => (NodeKind::Float, Some(f.to_string()), Vector::new()),
            Sexp::String(s, _) => (NodeKind::String, Some(s.clone()), Vector::new()),
            Sexp::Symbol(s, _) => (NodeKind::Symbol, Some(s.clone()), Vector::new()),
            Sexp::Keyword(k, _) => (NodeKind::Keyword, Some(k.clone()), Vector::new()),
            Sexp::Char(c, _) => (NodeKind::Char, Some(c.to_string()), Vector::new()),
            Sexp::List(l, _) | Sexp::Vector(l, _) => (
                if matches!(sexp, Sexp::List(_, _)) { NodeKind::List } else { NodeKind::Vector },
                None,
                l.iter().map(SyntaxNode::from_sexp).collect(),
            ),
            Sexp::Map(m, _) => {
                // A map is an unordered collection; keep it as a list of
                // key/value pairs so the bridge round-trips deterministically.
                let mut pairs: Vec<(Sexp, Sexp)> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                pairs.sort_by_key(|(k, _)| k.to_string());
                let mut children = Vector::new();
                for (k, v) in pairs {
                    children.push_back(SyntaxNode::from_sexp(&k));
                    children.push_back(SyntaxNode::from_sexp(&v));
                }
                (NodeKind::Map, None, children)
            }
        };
        SyntaxNode {
            kind,
            value,
            children,
            span: sexp.span(),
            origin: Origin::Source,
        }
    }

    pub fn with_origin(mut self, origin: Origin) -> Self {
        self.origin = origin;
        self
    }

    /// Rebuild the parsed form, spans included.
    pub fn to_sexp(&self) -> Sexp {
        let span = self.span;
        match &self.kind {
            NodeKind::Nil => Sexp::Nil,
            NodeKind::Boolean => Sexp::Boolean(self.value.as_deref() == Some("true")),
            NodeKind::Integer => Sexp::Integer(self.parse_int(), span),
            NodeKind::Float => Sexp::Float(self.parse_float(), span),
            NodeKind::String => Sexp::String(self.value.clone().unwrap_or_default(), span),
            NodeKind::Symbol => Sexp::Symbol(self.value.clone().unwrap_or_default(), span),
            NodeKind::Keyword => Sexp::Keyword(self.value.clone().unwrap_or_default(), span),
            NodeKind::Char => Sexp::Char(
                self.value.as_deref().and_then(|s| s.chars().next()).unwrap_or(' '),
                span,
            ),
            NodeKind::List => Sexp::List(self.children.iter().map(SyntaxNode::to_sexp).collect(), span),
            NodeKind::Vector => {
                Sexp::Vector(self.children.iter().map(SyntaxNode::to_sexp).collect(), span)
            }
            NodeKind::Map => {
                let items: Vec<Sexp> = self.children.iter().map(SyntaxNode::to_sexp).collect();
                let mut map = HashMap::new();
                for pair in items.chunks(2) {
                    if let [k, v] = pair {
                        map.insert(k.clone(), v.clone());
                    }
                }
                Sexp::Map(map, span)
            }
        }
    }

    /// The tagged `Value` form. Ordinary Zio map, readable from Zio.
    pub fn to_value(&self) -> Value {
        let mut map = HashMap::new();
        map.insert(Value::Keyword("kind".into()), Value::Keyword(self.kind.keyword().into()));
        map.insert(
            Value::Keyword("value".into()),
            match &self.value {
                Some(v) => Value::String(v.clone()),
                None => Value::Nil,
            },
        );
        map.insert(
            Value::Keyword("children".into()),
            Value::Vector(self.children.iter().map(SyntaxNode::to_value).collect()),
        );
        map.insert(
            Value::Keyword("span".into()),
            self.span.map_or(Value::Nil, span_to_value),
        );
        map.insert(Value::Keyword("origin".into()), self.origin.clone().to_value());
        Value::Map(map)
    }

    /// Read a tagged `Value` back. `None` when the value is not a node —
    /// including a map that lies about `:kind` or has no `:children`.
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        let kind = match map.get(&Value::Keyword("kind".into()))? {
            Value::Keyword(k) => NodeKind::from_keyword(k)?,
            _ => return None,
        };
        let text = match map.get(&Value::Keyword("value".into())) {
            None | Some(Value::Nil) => None,
            Some(Value::String(s)) => Some(s.clone()),
            _ => return None,
        };
        let children = match map.get(&Value::Keyword("children".into()))? {
            Value::Vector(items) => {
                let mut out = Vector::new();
                for item in items {
                    out.push_back(SyntaxNode::from_value(item)?);
                }
                out
            }
            _ => return None,
        };
        let span = match map.get(&Value::Keyword("span".into())) {
            None | Some(Value::Nil) => None,
            Some(other) => Some(span_from_value(other)?),
        };
        let origin = Origin::from_value(map.get(&Value::Keyword("origin".into()))?)?;
        Some(SyntaxNode { kind, value: text, children, span, origin })
    }

    fn parse_int(&self) -> i64 {
        self.value.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    fn parse_float(&self) -> f64 {
        self.value.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0.0)
    }
}
