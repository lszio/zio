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
/// The `#[source]` annotation satisfies thiserror's requirement.
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

    pub fn display(&self) -> String {
        match &self.span {
            Some(s) => format!(" at {}:{}", s.line, s.col),
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

#[derive(Error, Debug, Clone, PartialEq)]
pub enum EvalError {
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String, #[source] SpanContext),

    #[error("Not a function: {value}")]
    NotAFunction {
        value: String,
        span: SpanContext,
    },

    #[error("Wrong argument count: expected {expected}, got {got}")]
    WrongArgCount {
        expected: usize,
        got: usize,
        span: SpanContext,
    },

    #[error("Wrong argument count: expected at least {min}, got {got}")]
    WrongArgCountMin {
        min: usize,
        got: usize,
        span: SpanContext,
    },

    #[error("Wrong argument count: expected between {min} and {max}, got {got}")]
    WrongArgCountRange {
        min: usize,
        max: usize,
        got: usize,
        span: SpanContext,
    },

    #[error("Index out of bounds: index {index} but length is {length}")]
    IndexOutOfBounds {
        index: usize,
        length: usize,
        span: SpanContext,
    },

    #[error("Type error: expected {expected}, got {got}")]
    TypeError {
        expected: &'static str,
        got: String,
        span: SpanContext,
    },

    #[error("Division by zero")]
    DivisionByZero { span: SpanContext },

    #[error("Invalid form: {0}")]
    InvalidForm(String, #[source] SpanContext),

    #[error("Macro error: {0}")]
    MacroError(String, #[source] SpanContext),

    #[error("recur not in tail position")]
    RecurNotTail { span: SpanContext },

    #[error("recur without loop frame")]
    RecurWithoutLoop { span: SpanContext },

    #[error("{0}")]
    Custom(String, #[source] SpanContext),
}

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
}