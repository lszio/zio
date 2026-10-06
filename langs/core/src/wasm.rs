#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg(feature = "wasm")]
use crate::context::EvalContext;
#[cfg(feature = "wasm")]
use crate::env::Env;
#[cfg(feature = "wasm")]
use crate::error::EvalError;
#[cfg(feature = "wasm")]
use crate::value::{NativeFn, Value};

/// Public WASM Entrypoint: Evaluate a Zio source string and return its string representation.
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub fn eval_zio(code: &str) -> String {
    let env = std::sync::Arc::new(Env::new(None));
    crate::builtins::setup_env(&env);

    // Register JS FFI builtins
    register_js_builtins(&env);

    let ctx = EvalContext::new(env.clone());
    match crate::reader::reader::read_program(code) {
        Ok(forms) => {
            let mut last = String::new();
            for sexp in forms {
                match crate::eval::eval_in_context(&sexp, &ctx) {
                    Ok(val) => last = format!("{val}"),
                    Err(e) => return format!("Error: {e}"),
                }
            }
            last
        }
        Err(e) => format!("Reader Error: {e}"),
    }
}

#[cfg(feature = "wasm")]
fn register_js_builtins(env: &std::sync::Arc<Env>) {
    // (js/eval code_str)
    env.set(
        "js/eval".into(),
        Value::NativeFunction(NativeFn::new("js/eval", |args, _| {
            if args.len() != 1 {
                return Err(EvalError::wrong_arg_count(1, args.len()));
            }
            let code = match &args[0] {
                Value::String(s) => s,
                other => return Err(EvalError::type_error("string", other.value_type())),
            };
            match js_sys::eval(code) {
                Ok(val) => Ok(Value::String(format!("{val:?}"))),
                Err(err) => Err(EvalError::custom(format!("js/eval error: {err:?}"))),
            }
        })),
    );

    // (js/console-log msg)
    env.set(
        "js/console-log".into(),
        Value::NativeFunction(NativeFn::new("js/console-log", |args, _| {
            let msg = args
                .iter()
                .map(|v| format!("{v}"))
                .collect::<Vec<_>>()
                .join(" ");
            web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&msg));
            Ok(Value::Nil)
        })),
    );

    // (js/dom-set-text selector text)
    env.set(
        "js/dom-set-text".into(),
        Value::NativeFunction(NativeFn::new("js/dom-set-text", |args, _| {
            if args.len() != 2 {
                return Err(EvalError::wrong_arg_count(2, args.len()));
            }
            let sel = match &args[0] {
                Value::String(s) => s,
                other => return Err(EvalError::type_error("selector string", other.value_type())),
            };
            let text = match &args[1] {
                Value::String(s) => s.clone(),
                _ => format!("{}", args[1]),
            };
            if let Some(window) = web_sys::window() {
                if let Some(doc) = window.document() {
                    if let Ok(Some(el)) = doc.query_selector(sel) {
                        el.set_text_content(Some(&text));
                        return Ok(Value::Boolean(true));
                    }
                }
            }
            Ok(Value::Boolean(false))
        })),
    );
}

/// Persistent REPL session: one Env + EvalContext across calls, so top-level
/// definitions survive between evaluations. `eval_zio` creates a fresh
/// environment per call and cannot do this.
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct ZioSession {
    ctx: EvalContext,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl ZioSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> ZioSession {
        let env = std::sync::Arc::new(Env::new(None));
        crate::builtins::setup_env(&env);
        register_js_builtins(&env);
        ZioSession {
            ctx: EvalContext::new(env),
        }
    }

    /// Evaluate a Zio source string; returns the last value's repr, or an
    /// `Error: ...` string on failure. State persists across calls.
    pub fn eval(&self, code: &str) -> String {
        match crate::reader::reader::read_program(code) {
            Ok(forms) => {
                let mut last = String::new();
                for sexp in forms {
                    match crate::eval::eval_in_context(&sexp, &self.ctx) {
                        Ok(val) => last = format!("{val}"),
                        Err(e) => return format!("Error: {e}"),
                    }
                }
                last
            }
            Err(e) => format!("Reader Error: {e}"),
        }
    }
}
