#![recursion_limit = "256"]
pub mod value;
#[doc(hidden)]
pub mod im {
    pub use ::im::*;
}
pub mod sexp;
pub mod env;
pub mod eval;
pub mod builtins;
pub mod error;
pub mod macros;
pub mod module;
pub mod span;
pub mod special;
pub mod symbol;
pub mod reader;
pub mod context;
pub mod zos;
/// Return the embedded core standard library source.
pub fn stdlib_source() -> &'static str {
    include_str!("../stdlib/zio/core.zio")
}