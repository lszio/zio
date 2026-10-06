//! Native builtin bindings, split by domain.
//!
//! Each submodule owns the Rust functions for one domain and a `register`
//! that installs its bindings. `setup_env` is the single entry point used
//! by the CLI, WASM entry, tests, and benches.

pub mod buffer;
pub mod collections;
pub mod concurrency;
pub mod io;
pub mod json;
pub mod macroexpand;
pub mod numeric;
pub mod predicates;
pub mod strings;
pub mod zos_access;

use std::sync::Arc;

use crate::env::Env;
use crate::value::{NativeFn, Value};

pub fn setup_env(env: &Arc<Env>) {
    numeric::register(env);
    predicates::register(env);
    collections::register(env);
    strings::register(env);
    zos_access::register(env);
    macroexpand::register(env);
    io::register(env);
    buffer::register(env);
    concurrency::register(env);
    json::register(env);

    // Error signaling (implemented with the condition-system form handler)
    env.set(
        "error".into(),
        Value::NativeFunction(NativeFn::new("error", crate::special::zos_forms::do_error_fn)),
    );
}
