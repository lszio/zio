#![recursion_limit = "256"]
pub mod bytecode;
pub mod value;
#[doc(hidden)]
pub mod im {
    pub use ::im::*;
}
pub mod bootstrap;
pub mod builtins;
pub mod bundle;
pub mod context;
pub mod env;
pub mod error;
pub mod eval;
pub mod fmt;
pub mod harness;
pub mod image;
pub mod interpreter;
pub mod io;
pub mod macros;
pub mod module;
pub mod observer;
pub mod reader;
pub mod sexp;
pub mod span;
pub mod special;
pub mod syntax;
pub mod wasm;
pub mod zos;
pub use harness::{
    Capability, CapabilitySet, CompileResult, Diagnostic, ObserverEvent, RunId, RunResult, Session,
    SessionDefaults, SessionId,
};
/// Return the embedded core standard library source.
pub fn stdlib_source() -> &'static str {
    include_str!("../assets/std/core.zio")
}
