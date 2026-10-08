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
    match ZioSession::new() {
        Ok(session) => session.eval(code),
        Err(error) => format!(
            "Error: {}",
            error
                .as_string()
                .unwrap_or_else(|| "bootstrap failed".into())
        ),
    }
}

#[cfg(feature = "wasm")]
struct BrowserIoHost {
    files: crate::io::BufferIoHost,
}

#[cfg(feature = "wasm")]
impl crate::io::IoHost for BrowserIoHost {
    fn print(&self, message: &str) -> Result<(), EvalError> {
        web_sys::console::log_1(&JsValue::from_str(message));
        Ok(())
    }
    fn println(&self, message: &str) -> Result<(), EvalError> {
        self.print(message)
    }
    fn read_line(&self) -> Result<String, EvalError> {
        Err(EvalError::custom("browser has no stdin capability"))
    }
    fn current_dir(&self) -> Result<String, EvalError> {
        Err(EvalError::custom(
            "browser has no working-directory capability",
        ))
    }
    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        self.files.read_file(path)
    }
    fn write_file(&self, _: &str, _: &str) -> Result<(), EvalError> {
        Err(EvalError::custom(
            "browser source has no filesystem-write capability",
        ))
    }
    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        self.files.file_exists(path)
    }
    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        self.files.canonicalize_path(path)
    }
    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        self.files.is_directory(path)
    }
}

#[cfg(feature = "wasm")]
fn js_error(error: EvalError) -> JsValue {
    JsValue::from_str(
        &serde_json::json!({"kind":"zio-error","message":error.to_string()}).to_string(),
    )
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub fn parse_zio(source: &str, name: &str) -> Result<String, JsValue> {
    let map = crate::span::SourceMap::new();
    let nodes = crate::syntax::inspect_source(&map, name, source)
        .map_err(|e| js_error(e.into_eval(name)))?;
    let value = Value::Vector(
        nodes
            .iter()
            .map(crate::syntax::SyntaxNode::to_value)
            .collect(),
    );
    serde_json::to_string(&crate::builtins::json::to_json(&value).map_err(js_error)?)
        .map_err(|e| JsValue::from_str(&e.to_string()))
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
    io: std::sync::Arc<BrowserIoHost>,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl ZioSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<ZioSession, JsValue> {
        let io = std::sync::Arc::new(BrowserIoHost {
            files: crate::io::BufferIoHost::new(),
        });
        let roots = crate::bootstrap::ModuleRoots::new_with_io(vec!["/zio".into()], io.as_ref())
            .map_err(js_error)?;
        let ctx =
            crate::bootstrap::language_context_with_io(roots, io.clone()).map_err(js_error)?;
        register_js_builtins(&ctx.env);
        Ok(ZioSession { ctx, io })
    }

    /// Evaluate a Zio source string; returns the last value's repr, or an
    /// `Error: ...` string on failure. State persists across calls.
    pub fn eval(&self, code: &str) -> String {
        match self.eval_source(code, "<browser>") {
            Ok(value) => value,
            Err(error) => format!(
                "Error: {}",
                error
                    .as_string()
                    .unwrap_or_else(|| "evaluation failed".into())
            ),
        }
    }

    pub fn eval_source(&self, source: &str, name: &str) -> Result<String, JsValue> {
        crate::bytecode::run_source(&self.ctx, name, source)
            .map(|value| value.to_string())
            .map_err(js_error)
    }

    pub fn eval_json(&self, source: &str, name: &str) -> Result<String, JsValue> {
        let value = crate::bytecode::run_source(&self.ctx, name, source).map_err(js_error)?;
        serde_json::to_string(&crate::builtins::json::to_json(&value).map_err(js_error)?)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn parse(&self, source: &str, name: &str) -> Result<String, JsValue> {
        let nodes = crate::syntax::inspect_source(&self.ctx.source_map, name, source)
            .map_err(|e| js_error(e.into_eval(name)))?;
        let value = Value::Vector(
            nodes
                .iter()
                .map(crate::syntax::SyntaxNode::to_value)
                .collect(),
        );
        serde_json::to_string(&crate::builtins::json::to_json(&value).map_err(js_error)?)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn add_source(&self, path: &str, source: &str) -> Result<(), JsValue> {
        use crate::io::IoHost;
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/zio/{}.zio", path.replace('.', "/"))
        };
        let canonical = self.io.canonicalize_path(&path).map_err(js_error)?;
        if !std::path::Path::new(&canonical).starts_with("/zio") || !canonical.ends_with(".zio") {
            return Err(JsValue::from_str(
                "browser source path must be a .zio file under /zio",
            ));
        }
        self.io
            .files
            .write_file(&canonical, source)
            .map_err(js_error)
    }
}
