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

#[derive(Error, Debug, Clone, PartialEq)]
pub enum EvalError {
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Not a function: {value}")]
    NotAFunction { value: String },

    #[error("Wrong argument count: expected {expected}, got {got}")]
    WrongArgCount { expected: usize, got: usize },

    #[error("Wrong argument count: expected at least {min}, got {got}")]
    WrongArgCountMin { min: usize, got: usize },

    #[error("Type error: expected {expected}, got {got}")]
    TypeError { expected: &'static str, got: String },

    #[error("Division by zero")]
    DivisionByZero,

    #[error("Invalid form: {0}")]
    InvalidForm(String),

    #[error("Macro error: {0}")]
    MacroError(String),

    #[error("recur not in tail position")]
    RecurNotTail,

    #[error("recur without loop frame")]
    RecurWithoutLoop,

    #[error("{0}")]
    Custom(String),
}

impl EvalError {
    pub fn type_error(expected: &'static str, got: &str) -> Self {
        EvalError::TypeError {
            expected,
            got: got.to_string(),
        }
    }
}
