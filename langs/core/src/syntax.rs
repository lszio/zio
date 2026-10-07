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
    MacroCall {
        macro_name: String,
        call_site: Option<Span>,
    },
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
        if map.len() != 3 {
            return None;
        }
        let detail = match map.get(&Value::Keyword("by".into()))? {
            Value::String(s) => s.clone(),
            _ => return None,
        };
        let call_site = match map.get(&Value::Keyword("call-site".into()))? {
            Value::Nil => None,
            value => Some(span_from_value(value)?),
        };
        if matches!(kind, "source" | "quoted") && (!detail.is_empty() || call_site.is_some()) {
            return None;
        }
        if kind == "generated" && call_site.is_some() {
            return None;
        }
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
    map.insert(
        Value::Keyword("source".into()),
        Value::Integer(span.source_id.0 as i64),
    );
    map.insert(
        Value::Keyword("start".into()),
        Value::Integer(span.start.0 as i64),
    );
    map.insert(
        Value::Keyword("end".into()),
        Value::Integer(span.end.0 as i64),
    );
    map.insert(
        Value::Keyword("line".into()),
        Value::Integer(span.line as i64),
    );
    map.insert(
        Value::Keyword("col".into()),
        Value::Integer(span.col as i64),
    );
    Value::Map(map)
}

fn span_from_value(value: &Value) -> Option<Span> {
    let map = match value {
        Value::Map(m) => m,
        _ => return None,
    };
    if map.len() != 5 {
        return None;
    }
    let field = |name: &str| -> Option<usize> {
        match map.get(&Value::Keyword(name.into()))? {
            Value::Integer(i) => usize::try_from(*i).ok(),
            _ => None,
        }
    };
    let start = field("start")?;
    let end = field("end")?;
    let line = field("line")?;
    let col = field("col")?;
    if start > end || line == 0 || col == 0 {
        return None;
    }
    Some(Span::new(
        crate::span::SourceId(field("source")?),
        crate::span::BytePos(start),
        crate::span::BytePos(end),
        line,
        col,
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
                if matches!(sexp, Sexp::List(_, _)) {
                    NodeKind::List
                } else {
                    NodeKind::Vector
                },
                None,
                l.iter().map(SyntaxNode::from_sexp).collect(),
            ),
            Sexp::Map(m, _) => {
                // A map is an unordered collection; keep it as a list of
                // key/value pairs so the bridge round-trips deterministically.
                let mut pairs: Vec<(Sexp, Sexp)> =
                    m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
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
    pub fn to_sexp(&self) -> Result<Sexp, crate::error::EvalError> {
        self.validate()?;
        let span = self.span;
        let text = || self.value.as_deref().expect("validated syntax payload");
        Ok(match &self.kind {
            NodeKind::Nil => Sexp::Nil,
            NodeKind::Boolean => Sexp::Boolean(text() == "true"),
            NodeKind::Integer => Sexp::Integer(text().parse().expect("validated integer"), span),
            NodeKind::Float => Sexp::Float(text().parse().expect("validated float"), span),
            NodeKind::String => Sexp::String(text().into(), span),
            NodeKind::Symbol => Sexp::Symbol(text().into(), span),
            NodeKind::Keyword => Sexp::Keyword(text().into(), span),
            NodeKind::Char => Sexp::Char(text().chars().next().expect("validated character"), span),
            NodeKind::List => Sexp::List(
                self.children
                    .iter()
                    .map(SyntaxNode::to_sexp)
                    .collect::<Result<_, _>>()?,
                span,
            ),
            NodeKind::Vector => Sexp::Vector(
                self.children
                    .iter()
                    .map(SyntaxNode::to_sexp)
                    .collect::<Result<_, _>>()?,
                span,
            ),
            NodeKind::Map => {
                let mut map = HashMap::new();
                let mut children = self.children.iter();
                while let Some(key) = children.next() {
                    let key = key.to_sexp()?;
                    let value = children.next().expect("validated even map").to_sexp()?;
                    if map.insert(key, value).is_some() {
                        return Err(crate::error::EvalError::invalid_form(
                            "duplicate syntax map key",
                        )
                        .with_opt_span(span));
                    }
                }
                Sexp::Map(map, span)
            }
        })
    }

    /// The tagged `Value` form. Ordinary Zio map, readable from Zio.
    pub fn to_value(&self) -> Value {
        let mut map = HashMap::new();
        map.insert(
            Value::Keyword("kind".into()),
            Value::Keyword(self.kind.keyword().into()),
        );
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
        map.insert(
            Value::Keyword("origin".into()),
            self.origin.clone().to_value(),
        );
        Value::Map(map)
    }

    /// Read a tagged `Value` back. `None` when the value is not a node —
    /// including a map that lies about `:kind` or has no `:children`.
    pub fn from_value(value: &Value) -> Option<Self> {
        let map = match value {
            Value::Map(m) => m,
            _ => return None,
        };
        if map.len() != 5 {
            return None;
        }
        let kind = match map.get(&Value::Keyword("kind".into()))? {
            Value::Keyword(k) => NodeKind::from_keyword(k)?,
            _ => return None,
        };
        let text = match map.get(&Value::Keyword("value".into()))? {
            Value::Nil => None,
            Value::String(s) => Some(s.clone()),
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
        let span = match map.get(&Value::Keyword("span".into()))? {
            Value::Nil => None,
            other => Some(span_from_value(other)?),
        };
        let origin = Origin::from_value(map.get(&Value::Keyword("origin".into()))?)?;
        let node = SyntaxNode {
            kind,
            value: text,
            children,
            span,
            origin,
        };
        node.validate().ok()?;
        node.to_sexp().ok()?;
        Some(node)
    }

    pub fn validate(&self) -> Result<(), crate::error::EvalError> {
        let invalid = || {
            crate::error::EvalError::invalid_form("malformed syntax node").with_opt_span(self.span)
        };
        if let Some(span) = self.span {
            if span.start > span.end || span.line == 0 || span.col == 0 {
                return Err(invalid());
            }
        }
        let container = matches!(self.kind, NodeKind::List | NodeKind::Vector | NodeKind::Map);
        if container || self.kind == NodeKind::Nil {
            if self.value.is_some() {
                return Err(invalid());
            }
        } else {
            let text = self.value.as_deref().ok_or_else(invalid)?;
            let valid = match self.kind {
                NodeKind::Boolean => matches!(text, "true" | "false"),
                NodeKind::Integer => text.parse::<i64>().is_ok(),
                NodeKind::Float => text.parse::<f64>().is_ok_and(f64::is_finite),
                NodeKind::Char => text.chars().count() == 1,
                NodeKind::Symbol | NodeKind::Keyword => !text.is_empty(),
                NodeKind::String => true,
                _ => false,
            };
            if !valid {
                return Err(invalid());
            }
        }
        if !container && !self.children.is_empty() {
            return Err(invalid());
        }
        if self.kind == NodeKind::Map && !self.children.len().is_multiple_of(2) {
            return Err(invalid());
        }
        for child in &self.children {
            child.validate()?;
        }
        Ok(())
    }
}

/// Parse and inspect a document using its own source registry. Never evaluates.
pub fn inspect_source(
    source_map: &crate::span::SourceMap,
    name: &str,
    source: &str,
) -> Result<Vec<SyntaxNode>, crate::error::ReaderError> {
    let id = source_map.register(name.into(), source.into());
    crate::reader::reader::read_program_with_source(source, id)
        .map(|forms| forms.iter().map(SyntaxNode::from_sexp).collect())
}

pub fn register(env: &std::sync::Arc<crate::env::Env>) {
    use crate::value::NativeFn;
    env.set(
        "read-syntax".into(),
        Value::NativeFunction(NativeFn::new("read-syntax", |args, engine| {
            if args.len() != 2 {
                return Err(crate::error::EvalError::wrong_arg_count(2, args.len()));
            }
            let Value::String(source) = &args[0] else {
                return Err(crate::error::EvalError::type_error(
                    "string",
                    args[0].value_type(),
                ));
            };
            let Value::String(name) = &args[1] else {
                return Err(crate::error::EvalError::type_error(
                    "string",
                    args[1].value_type(),
                ));
            };
            inspect_source(engine.source_map(), name, source)
                .map(|nodes| Value::Vector(nodes.iter().map(SyntaxNode::to_value).collect()))
                .map_err(|e| e.into_eval(name))
        })),
    );
    env.set(
        "syntax->data".into(),
        Value::NativeFunction(NativeFn::new("syntax->data", |args, _| {
            if args.len() != 1 {
                return Err(crate::error::EvalError::wrong_arg_count(1, args.len()));
            }
            let node = SyntaxNode::from_value(&args[0]).ok_or_else(|| {
                crate::error::EvalError::invalid_form("expected valid syntax node")
            })?;
            Ok(Value::from(node.to_sexp()?))
        })),
    );
}
