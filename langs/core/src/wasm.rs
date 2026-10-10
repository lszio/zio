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

// ── First-class JS values ─────────────────────────────────────────
// Any JsValue without a natural Zio primitive mapping becomes an opaque
// ZOS object (`#<js-object>`), so a handle returned by one call can feed
// the next: (-> (js/eval "window") (js/prop "document") (js/prop "title")).
// Threading is the ONLY chaining machinery — the objects themselves are
// inert, and every operation is an ordinary namespaced function.

#[cfg(feature = "wasm")]
static NEXT_JS_IDENTITY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(feature = "wasm")]
thread_local! {
    static JS_OBJECT_CLASS: std::sync::Arc<crate::zos::object::Class> = std::sync::Arc::new(
        crate::zos::object::Class {
            name: "js-object".into(),
            superclasses: vec![],
            slots: vec![],
            cpl: vec!["js-object".into()],
        },
    );
}

#[cfg(feature = "wasm")]
struct ForeignJs {
    header: crate::zos::object::ObjectHeader,
    value: JsValue,
}

#[cfg(feature = "wasm")]
impl ForeignJs {
    fn new(value: JsValue) -> Self {
        ForeignJs {
            header: crate::zos::object::ObjectHeader {
                class: JS_OBJECT_CLASS.with(|c| c.clone()),
                flags: crate::zos::object::ObjectFlags::NONE,
                identity: Some(NEXT_JS_IDENTITY.fetch_add(1, std::sync::atomic::Ordering::Relaxed)),
            },
            value,
        }
    }
}

#[cfg(feature = "wasm")]
impl std::fmt::Debug for ForeignJs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForeignJs({self:?})")
    }
}

#[cfg(feature = "wasm")]
impl crate::zos::object::ZosObject for ForeignJs {
    fn header(&self) -> &crate::zos::object::ObjectHeader {
        &self.header
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn clone_box(&self) -> Box<dyn crate::zos::object::ZosObject> {
        Box::new(ForeignJs::new(self.value.clone()))
    }
}

/// Map a JS result into a Zio value: primitives become primitives,
/// everything else stays an opaque first-class object.
#[cfg(feature = "wasm")]
fn js_to_value(value: JsValue) -> Value {
    if value.is_null() || value.is_undefined() {
        Value::Nil
    } else if let Some(b) = value.as_bool() {
        Value::Boolean(b)
    } else if let Some(n) = value.as_f64() {
        if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
            Value::Integer(n as i64)
        } else {
            Value::Float(n)
        }
    } else if let Some(s) = value.as_string() {
        Value::String(s)
    } else {
        Value::Object(Box::new(ForeignJs::new(value)))
    }
}

/// Pass a Zio value into a JS call. Foreign JS objects unwrap to their
/// original handle; composite Zio data (maps, lists, vectors) converts
/// deeply to plain JS data via the JSON rules; ZOS objects are refused
/// rather than silently stringified.
#[cfg(feature = "wasm")]
fn value_to_js(value: &Value) -> Result<JsValue, EvalError> {
    match value {
        Value::Nil => Ok(JsValue::NULL),
        Value::Boolean(b) => Ok(JsValue::from_bool(*b)),
        Value::Integer(i) => {
            if (*i as f64).is_finite() {
                Ok(JsValue::from_f64(*i as f64))
            } else {
                Err(EvalError::custom(format!(
                    "integer {i} is not a finite JS number"
                )))
            }
        }
        Value::Float(f) => {
            if f.is_finite() {
                Ok(JsValue::from_f64(*f))
            } else {
                Err(EvalError::custom(format!(
                    "float {f} is not a finite JS number"
                )))
            }
        }
        Value::String(s) => Ok(JsValue::from_str(s)),
        Value::Object(o) => match o.as_any().downcast_ref::<ForeignJs>() {
            Some(foreign) => Ok(foreign.value.clone()),
            None => Err(EvalError::custom(
                "cannot pass a ZOS object to JavaScript (only primitives and js handles)",
            )),
        },
        // Composite data converts deeply via the JSON rules, so
        // (js/JSON.stringify {:a 1}) and passing a vector as an argument
        // behave like plain data — while opaque ZOS objects above still
        // refuse, keeping handles and data distinct.
        other => {
            let json = crate::builtins::json::to_json(other)?;
            Ok(json_to_js(json))
        }
    }
}

#[cfg(feature = "wasm")]
fn json_to_js(json: serde_json::Value) -> JsValue {
    use serde_json::Value as Json;
    match json {
        Json::Null => JsValue::NULL,
        Json::Bool(b) => JsValue::from_bool(b),
        Json::Number(n) => JsValue::from_f64(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => JsValue::from_str(&s),
        Json::Array(items) => items
            .into_iter()
            .map(json_to_js)
            .collect::<js_sys::Array>()
            .into(),
        Json::Object(map) => {
            let object = js_sys::Object::new();
            for (key, value) in map {
                let _ = js_sys::Reflect::set(&object, &JsValue::from_str(&key), &json_to_js(value));
            }
            JsValue::from(object)
        }
    }
}

/// Extract the JS handle from a value. Foreign handles unwrap; primitives
/// and composite data convert to their JS counterparts (a Zio string is a
/// JS string with `length` and string methods; `Reflect` needs an object
/// receiver, so primitives are boxed like `new String(...)`); ZOS objects
/// are refused.
#[cfg(feature = "wasm")]
fn js_handle(value: &Value) -> Result<JsValue, EvalError> {
    match value {
        Value::Object(o) => match o.as_any().downcast_ref::<ForeignJs>() {
            Some(foreign) => Ok(foreign.value.clone()),
            None => Err(EvalError::custom(
                "expected a JS object (from js/eval or js/prop); got a ZOS object",
            )),
        },
        other => {
            let js = value_to_js(other)?;
            if js.is_object() {
                Ok(js)
            } else {
                // Reflect needs an object receiver: box the primitive with
                // the JS Object() constructor (new String(...) semantics —
                // length and methods work).
                let object_fn = js_sys::Reflect::get(
                    &js_sys::global(),
                    &JsValue::from_str("Object"),
                )
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
                match object_fn {
                    Some(constructor) => constructor
                        .call1(&JsValue::NULL, &js)
                        .or_else(|_| Ok(js)),
                    None => Ok(js),
                }
            }
        }
    }
}

/// Resolve a dotted `js/<global.path>` symbol against the JS global object,
/// so `(js/console.log "x")`, `(js/Math.max 1 2)` and `(-> 3.7 js/Math.floor)`
/// work like ordinary functions. A function found on the path is wrapped with
/// its parent as `this` (so `console.log` really logs through `console`);
/// anything else maps through `js_to_value`. Registered builtins (`js/eval`,
/// `js/prop`, …) win over this path — plain env lookup happens first.
/// Resolution is interpreter-only: bytecode-compiled bodies resolve symbols
/// against the env and do not consult this hook.
#[cfg(feature = "wasm")]
pub(crate) fn resolve_js_symbol(name: &str) -> Option<Value> {
    let path = name.strip_prefix("js/")?;
    if path.is_empty() {
        return None;
    }
    let segments: Vec<&str> = path.split('.').collect();
    let mut parent = JsValue::from(js_sys::global());
    for segment in &segments[..segments.len() - 1] {
        parent = js_sys::Reflect::get(&parent, &JsValue::from_str(segment)).ok()?;
    }
    let final_value =
        js_sys::Reflect::get(&parent, &JsValue::from_str(segments[segments.len() - 1])).ok()?;
    if final_value.is_instance_of::<js_sys::Function>() {
        let function: js_sys::Function = final_value.dyn_into().unwrap();
        let qualified = name.to_string();
        Some(Value::NativeFunction(NativeFn::new(
            "js-bound-call",
            move |args, _| {
                let call_args: js_sys::Array = args
                    .iter()
                    .map(value_to_js)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .collect();
                function
                    .apply(&parent, &call_args)
                    .map(js_to_value)
                    .map_err(|err| EvalError::custom(format!("{qualified} call error: {err:?}")))
            },
        )))
    } else {
        Some(js_to_value(final_value))
    }
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
    // (js/eval code_str) — result maps to Zio values; JS objects stay
    // first-class handles so -> can chain into them.
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
                Ok(val) => Ok(js_to_value(val)),
                Err(err) => Err(EvalError::custom(format!("js/eval error: {err:?}"))),
            }
        })),
    );

    // (js/prop obj name) — property read; objects stay first-class.
    env.set(
        "js/prop".into(),
        Value::NativeFunction(NativeFn::new("js/prop", |args, _| {
            if args.len() != 2 {
                return Err(EvalError::wrong_arg_count(2, args.len()));
            }
            let target = js_handle(&args[0])?;
            let name = match &args[1] {
                Value::String(s) => JsValue::from_str(s),
                other => {
                    return Err(EvalError::type_error(
                        "property name string",
                        other.value_type(),
                    ));
                }
            };
            js_sys::Reflect::get(&target, &name)
                .map(js_to_value)
                .map_err(|err| EvalError::custom(format!("js/prop error: {err:?}")))
        })),
    );

    // (js/set obj name value) — property write; returns true on success.
    env.set(
        "js/set".into(),
        Value::NativeFunction(NativeFn::new("js/set", |args, _| {
            if args.len() != 3 {
                return Err(EvalError::wrong_arg_count(3, args.len()));
            }
            let target = js_handle(&args[0])?;
            let name = match &args[1] {
                Value::String(s) => JsValue::from_str(s),
                other => {
                    return Err(EvalError::type_error(
                        "property name string",
                        other.value_type(),
                    ));
                }
            };
            let payload = value_to_js(&args[2])?;
            js_sys::Reflect::set(&target, &name, &payload)
                .map(Value::Boolean)
                .map_err(|err| EvalError::custom(format!("js/set error: {err:?}")))
        })),
    );

    // (js/call obj method & args) — invoke a method on a JS object with
    // Zio-typed arguments; the result maps back through js_to_value.
    env.set(
        "js/call".into(),
        Value::NativeFunction(NativeFn::new("js/call", |args, _| {
            if args.len() < 2 {
                return Err(EvalError::wrong_arg_count(2, args.len()));
            }
            let target = js_handle(&args[0])?;
            let method = match &args[1] {
                Value::String(s) => JsValue::from_str(s),
                other => {
                    return Err(EvalError::type_error(
                        "method name string",
                        other.value_type(),
                    ));
                }
            };
            let function: js_sys::Function = js_sys::Reflect::get(&target, &method)
                .map_err(|err| EvalError::custom(format!("js/call error: {err:?}")))?
                .dyn_into()
                .map_err(|_| {
                    EvalError::custom(format!(
                        "js/call: {} is not a function",
                        method.as_string().unwrap_or_default()
                    ))
                })?;
            let call_args: js_sys::Array = args
                .iter()
                .skip(2)
                .map(value_to_js)
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .collect();
            function
                .apply(&target, &call_args)
                .map(js_to_value)
                .map_err(|err| EvalError::custom(format!("js/call error: {err:?}")))
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
