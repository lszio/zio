use crate::span::Span;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ReaderError {
    #[error("Unexpected EOF")]
    UnexpectedEOF,
    #[error("Unexpected token: {0}")]
    UnexpectedToken(String),
    #[error("Map must have even number of elements")]
    OddMapElements,
    #[error("Missing expression after quote")]
    MissingQuoteExpr,
}

/// Source location context attached to evaluation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanContext {
    pub span: Option<Span>,
}

impl SpanContext {
    pub const DUMMY: SpanContext = SpanContext { span: None };

    pub fn new(span: Option<Span>) -> Self {
        SpanContext { span }
    }

    pub fn from_span(span: Span) -> Self {
        SpanContext { span: Some(span) }
    }

    /// Return the formatted span location string, empty if no span.
    pub fn location(&self) -> String {
        match &self.span {
            Some(s) => format!(" at line {}, col {}", s.line, s.col),
            None => String::new(),
        }
    }
}

impl From<Option<Span>> for SpanContext {
    fn from(span: Option<Span>) -> Self {
        SpanContext { span }
    }
}

impl std::error::Error for SpanContext {}

impl std::fmt::Display for SpanContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.span {
            Some(s) => write!(f, "at {}:{}", s.line, s.col),
            None => Ok(()),
        }
    }
}

/// Evaluation errors with optional source location.
/// Display implementation includes span info when available.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    SymbolNotFound(String, SpanContext),
    NotAFunction {
        value: String,
        span: SpanContext,
    },
    WrongArgCount {
        expected: usize,
        got: usize,
        span: SpanContext,
    },
    WrongArgCountMin {
        min: usize,
        got: usize,
        span: SpanContext,
    },
    WrongArgCountRange {
        min: usize,
        max: usize,
        got: usize,
        span: SpanContext,
    },
    IndexOutOfBounds {
        index: usize,
        length: usize,
        span: SpanContext,
    },
    TypeError {
        expected: &'static str,
        got: String,
        span: SpanContext,
    },
    DivisionByZero {
        span: SpanContext,
    },
    InvalidForm(String, SpanContext),
    MacroError(String, SpanContext),
    RecurNotTail {
        span: SpanContext,
    },
    RecurWithoutLoop {
        span: SpanContext,
    },
    Custom(String, SpanContext),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use EvalError::*;
        match self {
            SymbolNotFound(s, ctx) => write!(f, "symbol not found: {s}{}", ctx.location()),
            NotAFunction { value, span: ctx } => write!(f, "not a function: {value}{}", ctx.location()),
            WrongArgCount { expected, got, span: ctx } => {
                write!(f, "wrong argument count: expected {expected}, got {got}{}", ctx.location())
            }
            WrongArgCountMin { min, got, span: ctx } => {
                write!(f, "wrong argument count: expected at least {min}, got {got}{}", ctx.location())
            }
            WrongArgCountRange { min, max, got, span: ctx } => {
                write!(f, "wrong argument count: expected between {min} and {max}, got {got}{}", ctx.location())
            }
            IndexOutOfBounds { index, length, span: ctx } => {
                write!(f, "index out of bounds: index {index} but length is {length}{}", ctx.location())
            }
            TypeError { expected, got, span: ctx } => {
                write!(f, "type error: expected {expected}, got {got}{}", ctx.location())
            }
            DivisionByZero { span: ctx } => write!(f, "division by zero{}", ctx.location()),
            InvalidForm(msg, ctx) => write!(f, "invalid form: {msg}{}", ctx.location()),
            MacroError(msg, ctx) => write!(f, "macro error: {msg}{}", ctx.location()),
            RecurNotTail { span: ctx } => write!(f, "recur not in tail position{}", ctx.location()),
            RecurWithoutLoop { span: ctx } => write!(f, "recur without loop frame{}", ctx.location()),
            Custom(msg, ctx) => write!(f, "{msg}{}", ctx.location()),
        }
    }
}

impl std::error::Error for EvalError {}

// ── Convenience constructors (default to Span DUMMY) ────────────

impl EvalError {
    pub fn type_error(expected: &'static str, got: impl AsRef<str>) -> Self {
        EvalError::TypeError {
            expected,
            got: got.as_ref().to_string(),
            span: SpanContext::DUMMY,
        }
    }

    pub fn symbol_not_found(name: impl Into<String>) -> Self {
        EvalError::SymbolNotFound(name.into(), SpanContext::DUMMY)
    }

    pub fn invalid_form(msg: impl Into<String>) -> Self {
        EvalError::InvalidForm(msg.into(), SpanContext::DUMMY)
    }

    pub fn custom(msg: impl Into<String>) -> Self {
        EvalError::Custom(msg.into(), SpanContext::DUMMY)
    }

    pub fn macro_error(msg: impl Into<String>) -> Self {
        EvalError::MacroError(msg.into(), SpanContext::DUMMY)
    }

    pub fn wrong_arg_count(expected: usize, got: usize) -> Self {
        EvalError::WrongArgCount {
            expected,
            got,
            span: SpanContext::DUMMY,
        }
    }

    pub fn wrong_arg_count_min(min: usize, got: usize) -> Self {
        EvalError::WrongArgCountMin {
            min,
            got,
            span: SpanContext::DUMMY,
        }
    }

    pub fn wrong_arg_count_range(min: usize, max: usize, got: usize) -> Self {
        EvalError::WrongArgCountRange {
            min,
            max,
            got,
            span: SpanContext::DUMMY,
        }
    }

    pub fn index_out_of_bounds(index: usize, length: usize) -> Self {
        EvalError::IndexOutOfBounds {
            index,
            length,
            span: SpanContext::DUMMY,
        }
    }

    pub fn not_a_function(value: impl Into<String>) -> Self {
        EvalError::NotAFunction {
            value: value.into(),
            span: SpanContext::DUMMY,
        }
    }

    pub fn division_by_zero() -> Self {
        EvalError::DivisionByZero {
            span: SpanContext::DUMMY,
        }
    }

    pub fn recur_not_tail() -> Self {
        EvalError::RecurNotTail {
            span: SpanContext::DUMMY,
        }
    }

    pub fn recur_without_loop() -> Self {
        EvalError::RecurWithoutLoop {
            span: SpanContext::DUMMY,
        }
    }

    /// Attach a source location to this error.
    pub fn with_span(self, span: Span) -> Self {
        let ctx = SpanContext { span: Some(span) };
        match self {
            Self::SymbolNotFound(s, _) => Self::SymbolNotFound(s, ctx),
            Self::NotAFunction { value, .. } => Self::NotAFunction { value, span: ctx },
            Self::WrongArgCount { expected, got, .. } => Self::WrongArgCount { expected, got, span: ctx },
            Self::WrongArgCountMin { min, got, .. } => Self::WrongArgCountMin { min, got, span: ctx },
            Self::WrongArgCountRange { min, max, got, .. } => Self::WrongArgCountRange { min, max, got, span: ctx },
            Self::IndexOutOfBounds { index, length, .. } => Self::IndexOutOfBounds { index, length, span: ctx },
            Self::TypeError { expected, got, .. } => Self::TypeError { expected, got, span: ctx },
            Self::DivisionByZero { .. } => Self::DivisionByZero { span: ctx },
            Self::InvalidForm(s, _) => Self::InvalidForm(s, ctx),
            Self::MacroError(s, _) => Self::MacroError(s, ctx),
            Self::RecurNotTail { .. } => Self::RecurNotTail { span: ctx },
            Self::RecurWithoutLoop { .. } => Self::RecurWithoutLoop { span: ctx },
            Self::Custom(s, _) => Self::Custom(s, ctx),
        }
    }

    /// Attach an optional source location (no-op if None).
    pub fn with_opt_span(self, span: Option<Span>) -> Self {
        match span {
            Some(s) => self.with_span(s),
            None => self,
        }
    }
}

impl std::cmp::PartialEq<str> for EvalError {
    fn eq(&self, other: &str) -> bool {
        self.to_string() == other
    }
}

impl From<std::convert::Infallible> for EvalError {
    fn from(x: std::convert::Infallible) -> Self {
        match x {}
    }
}
