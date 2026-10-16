//! Structured, read-only query service (ZIO-004; `docs/ai-language-interaction.md` §4).
//!
//! Five natives — `describe-symbol`, `help`, `apropos`, `module-exports`,
//! and `language-capabilities` — describe the current language surface
//! without executing target bindings, expanding macros, loading modules,
//! invoking getters/printers, or running examples. Metadata never proves
//! authorization; host grants are never asserted here.

use std::sync::Arc;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::im::{HashMap, Vector};
use crate::module::Module;
use crate::special::module_forms::resolve_qualified;
use crate::value::{NativeFn, NativeMetadata, Value};

use super::query_metadata::describe_value;

fn keyword(name: &str) -> Value {
    Value::Keyword(name.into())
}

fn map_key(name: &str) -> Value {
    keyword(name)
}

/// The interpreter's named special forms, mirroring the arms of
/// `special::eval_special_form`. Query is read-only with respect to
/// `special/mod.rs`, so this duplicates the dispatcher table; keep the
/// two lists in sync. The interpreter selects these before any
/// same-name environment binding (e.g. `and`/`or`).
const SPECIAL_FORMS: &[&str] = &[
    "quote",
    "quasiquote",
    "unquote",
    "unquote-splicing",
    "set!",
    "def",
    "defn",
    "if",
    "do",
    "fn",
    "let",
    "let*",
    "loop",
    "recur",
    "defmacro",
    "and",
    "or",
    "cond",
    "module",
    "require",
    "export",
    "defclass",
    "defgeneric",
    "defmethod",
    "call-next-method",
    "defpackage",
    "try",
];

fn is_special_form(name: &str) -> bool {
    SPECIAL_FORMS.contains(&name)
}

// ── Field assembly ─────────────────────────────────────────────────

fn base_fields(
    id: String,
    name: String,
    module: Option<String>,
    status: &str,
) -> HashMap<Value, Value> {
    let mut map = HashMap::new();
    map.insert(map_key("schema-version"), Value::Integer(1));
    map.insert(
        map_key("runtime-version"),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    map.insert(map_key("status"), keyword(status));
    map.insert(map_key("id"), Value::String(id));
    map.insert(map_key("name"), Value::String(name));
    map.insert(
        map_key("module"),
        module.map(Value::String).unwrap_or(Value::Nil),
    );
    // Independent module version, effects, and requirement claims have no
    // trusted source yet: nil means unknown, never "no effects/permissions".
    map.insert(map_key("module-version"), Value::Nil);
    map.insert(map_key("effects"), Value::Nil);
    map.insert(map_key("requires"), Value::Nil);
    map
}

/// Explicit unknown state for the `describe_value`-owned fields, used by
/// descriptions that have no runtime value behind them (special forms,
/// missing names, unloaded modules). Unknown signatures stay nil and are
/// never conflated with a zero-arity signature.
fn unknown_common() -> HashMap<Value, Value> {
    let mut map = HashMap::new();
    map.insert(map_key("kind"), keyword("unknown"));
    map.insert(map_key("origin"), keyword("unknown"));
    map.insert(map_key("params"), Value::Nil);
    map.insert(map_key("rest"), Value::Nil);
    map.insert(map_key("doc"), Value::Nil);
    map.insert(map_key("documentation"), keyword("unknown"));
    map.insert(map_key("source"), Value::Nil);
    map.insert(map_key("examples"), Value::Vector(Vector::new()));
    map
}

fn merge_into(target: &mut HashMap<Value, Value>, extra: HashMap<Value, Value>) {
    for (key, value) in extra {
        target.insert(key, value);
    }
}

fn binding_description(
    engine: &dyn EvalEngine,
    name: &str,
    module: Option<&str>,
    value: &Value,
) -> HashMap<Value, Value> {
    let mut map = base_fields(
        format!("binding:{name}"),
        name.to_string(),
        module.map(str::to_string),
        "found",
    );
    merge_into(&mut map, describe_value(value, engine));
    map
}

fn special_form_description(name: &str) -> HashMap<Value, Value> {
    let mut map = base_fields(format!("special:{name}"), name.to_string(), None, "found");
    let mut common = unknown_common();
    common.insert(map_key("kind"), keyword("special-form"));
    common.insert(map_key("origin"), keyword("interpreter"));
    merge_into(&mut map, common);
    map
}

fn not_found_description(name: &str) -> HashMap<Value, Value> {
    let mut map = base_fields(
        format!("not-found:{name}"),
        name.to_string(),
        None,
        "not-found",
    );
    merge_into(&mut map, unknown_common());
    map
}

fn module_common_fields(parts: &[String], status: &str) -> HashMap<Value, Value> {
    let display = parts.join(".");
    let mut map = base_fields(format!("module:{display}"), display, None, status);
    let mut common = unknown_common();
    common.insert(map_key("kind"), keyword("module"));
    common.insert(map_key("origin"), keyword("module"));
    merge_into(&mut map, common);
    map
}

fn unloaded_module_description(parts: &[String]) -> HashMap<Value, Value> {
    let mut map = module_common_fields(parts, "not-loaded");
    map.insert(map_key("exports"), Value::Vector(Vector::new()));
    map
}

fn module_description(engine: &dyn EvalEngine, module: &Module) -> HashMap<Value, Value> {
    let display = module.display_name();
    let mut map = module_common_fields(&module.name, "found");
    let mut exports: Vec<(String, Value)> = Vec::new();
    // Only owned, explicitly exported bindings are ever listed.
    for export in &module.exports {
        if let Some(value) = module.exported(export) {
            let qualified = format!("{display}/{export}");
            exports.push((
                export.clone(),
                Value::Map(binding_description(
                    engine,
                    &qualified,
                    Some(&display),
                    &value,
                )),
            ));
        }
    }
    exports.sort_by(|a, b| a.0.cmp(&b.0));
    let export_values: Vector<Value> = exports.into_iter().map(|(_, value)| value).collect();
    map.insert(map_key("exports"), Value::Vector(export_values));
    map
}

// ── Name resolution ────────────────────────────────────────────────

fn describe_named(engine: &dyn EvalEngine, name: &str) -> HashMap<Value, Value> {
    // The interpreter dispatches special forms before environment lookup;
    // queries must agree with that precedence.
    if is_special_form(name) {
        return special_form_description(name);
    }
    if let Some(value) = engine.env().get(name) {
        return binding_description(engine, name, None, &value);
    }
    // Qualified `module/name` resolves only explicit exports of an
    // already-loaded module; this never triggers a load.
    if let Some(value) = resolve_qualified(engine, name) {
        let module_ns = name
            .rsplit_once('/')
            .map(|(ns, _)| ns.split('.').collect::<Vec<_>>().join("."))
            .unwrap_or_default();
        return binding_description(engine, name, Some(&module_ns), &value);
    }
    not_found_description(name)
}

fn module_parts_from(name: &str) -> Result<Vec<String>, EvalError> {
    if name.is_empty() {
        return Err(EvalError::invalid_form("module name cannot be empty"));
    }
    Ok(name.split('.').map(str::to_string).collect())
}

fn module_parts(arg: &Value) -> Result<Vec<String>, EvalError> {
    let name = match arg {
        Value::Keyword(s) | Value::Symbol(s) | Value::String(s) => s.clone(),
        other => {
            return Err(EvalError::type_error(
                "module name (keyword, symbol, or string)",
                other.value_type(),
            ));
        }
    };
    module_parts_from(&name)
}

/// Common argument handling for `describe-symbol` and `help`: a
/// keyword selects the module namespace, a symbol or string the root
/// environment (qualified names resolve against loaded module exports).
fn describe_arg(engine: &dyn EvalEngine, arg: &Value) -> Result<HashMap<Value, Value>, EvalError> {
    match arg {
        Value::Keyword(name) => {
            let parts = module_parts_from(name)?;
            Ok(match engine.find_module(&parts) {
                Some(module) => module_description(engine, &module),
                None => unloaded_module_description(&parts),
            })
        }
        Value::Symbol(name) | Value::String(name) => Ok(describe_named(engine, name)),
        other => Err(EvalError::type_error(
            "symbol, string, or keyword",
            other.value_type(),
        )),
    }
}

// ── Help rendering ─────────────────────────────────────────────────

/// Render scalars from a description map only. Unknown or exotic values
/// never fall through to arbitrary `Display`/`Debug` previews of the
/// target value.
fn render_scalar(value: &Value) -> String {
    match value {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.clone(),
        Value::Keyword(k) => format!(":{k}"),
        Value::Char(c) => c.to_string(),
        Value::Vector(items) => items
            .iter()
            .map(render_scalar)
            .collect::<Vec<_>>()
            .join(", "),
        Value::List(items) => format!(
            "({})",
            items
                .iter()
                .map(render_scalar)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Map(map) => render_map(map),
        other => format!("<{}>", other.value_type()),
    }
}

fn render_map(map: &HashMap<Value, Value>) -> String {
    let mut keys: Vec<Value> = map.keys().cloned().collect();
    keys.sort_by_cached_key(render_scalar);
    keys.iter()
        .map(|key| {
            format!(
                "{}={}",
                render_scalar(key),
                render_scalar(map.get(key).unwrap_or(&Value::Nil))
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn description_field<'a>(map: &'a HashMap<Value, Value>, key: &str) -> &'a Value {
    map.get(&map_key(key)).unwrap_or(&Value::Nil)
}

/// Human-readable lines from the same description map a client reads,
/// rendered through the host I/O; never through the target's printer.
fn render_description(description: &HashMap<Value, Value>) -> Vec<String> {
    let mut lines = Vec::new();
    for key in ["name", "status", "kind", "module", "origin"] {
        let value = render_scalar(description_field(description, key));
        lines.push(format!("{key}: {value}"));
    }

    // Signature from the actual callable's params/rest; unknown stays
    // visibly unknown instead of pretending to be zero-arity.
    let name = render_scalar(description_field(description, "name"));
    let signature = match description_field(description, "params") {
        Value::Vector(params) => {
            let mut parts: Vec<String> = params.iter().map(render_scalar).collect();
            if let Value::String(rest) = description_field(description, "rest") {
                parts.push("&".to_string());
                parts.push(rest.clone());
            }
            if parts.is_empty() {
                format!("({name})")
            } else {
                format!("({} {})", name, parts.join(" "))
            }
        }
        _ => format!("({name} …)"),
    };
    lines.push(format!("signature: {signature}"));

    match description_field(description, "doc") {
        Value::String(text) if !text.is_empty() => {
            lines.push("doc:".to_string());
            for line in text.lines() {
                lines.push(format!("  {line}"));
            }
        }
        _ => lines.push("doc: nil".to_string()),
    }
    lines.push(format!(
        "documentation: {}",
        render_scalar(description_field(description, "documentation"))
    ));

    match description_field(description, "source") {
        Value::Map(source) if !source.is_empty() => {
            lines.push(format!("source: {}", render_map(source)));
        }
        Value::Nil => lines.push("source: nil".to_string()),
        _ => lines.push("source: <unavailable>".to_string()),
    }

    match description_field(description, "examples") {
        Value::Vector(examples) if !examples.is_empty() => {
            lines.push("examples:".to_string());
            for example in examples {
                if let Value::Map(example_map) = example
                    && let Value::String(text) = description_field(example_map, "text")
                {
                    lines.push(format!("  {text}"));
                }
            }
        }
        _ => lines.push("examples: []".to_string()),
    }

    for key in [
        "effects",
        "requires",
        "module-version",
        "id",
        "schema-version",
        "runtime-version",
    ] {
        lines.push(format!(
            "{key}: {}",
            render_scalar(description_field(description, key))
        ));
    }
    lines
}

// ── Native bindings ────────────────────────────────────────────────

/// `(describe-symbol name)` → description map.
fn describe_symbol_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(Value::Map(describe_arg(engine, &args[0])?))
}

/// `(help name)` → renders the same description through the host I/O,
/// returns nil. No bespoke REPL/CLI branch.
fn help_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let description = describe_arg(engine, &args[0])?;
    let io = engine.io();
    for line in render_description(&description) {
        io.println(&line)?;
    }
    Ok(Value::Nil)
}

fn parse_apropos_numeric(value: &Value, what: &str) -> Result<i64, EvalError> {
    match value {
        Value::Integer(n) => Ok(*n),
        other => Err(EvalError::type_error(
            "integer",
            format!("{what}: {}", other.value_type()),
        )),
    }
}

/// `(apropos needle [offset limit])` → `{:items [...] :offset N :next-offset N|nil
/// :schema-version 1 :runtime-version V}`, deterministic name order.
fn apropos_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.is_empty() || args.len() > 3 {
        return Err(EvalError::wrong_arg_count_range(1, 3, args.len()));
    }
    let needle = match &args[0] {
        Value::String(s) => s.to_ascii_lowercase(),
        other => {
            return Err(EvalError::type_error("string", other.value_type()));
        }
    };
    let offset_raw = if args.len() >= 2 {
        parse_apropos_numeric(&args[1], "offset")?
    } else {
        0
    };
    if offset_raw < 0 {
        return Err(EvalError::invalid_form(
            "offset must be a non-negative integer",
        ));
    }
    let limit_raw = if args.len() >= 3 {
        parse_apropos_numeric(&args[2], "limit")?
    } else {
        20
    };
    if !(1..=100).contains(&limit_raw) {
        return Err(EvalError::invalid_form("limit must be between 1 and 100"));
    }
    let offset = offset_raw as usize;
    let limit = limit_raw as usize;

    // ponytail: scan live bindings/source docs per query; index immutable
    // source revisions if large sessions make discovery measurably slow.
    // Root-visible bindings: the closest scope wins for a repeated name.
    let mut by_name: std::collections::HashMap<String, HashMap<Value, Value>> =
        std::collections::HashMap::new();
    let mut scope = Some(engine.env().clone());
    while let Some(current) = scope {
        for (name, value) in current.owned_bindings() {
            // Env does not carry name/docs shifts; never freeze a stale
            // lookup here — every entry is re-described from the live value
            // at query time.
            let unique = by_name.get(&name);
            if unique.is_none() {
                by_name.insert(
                    name.clone(),
                    binding_description(engine, &name, None, &value),
                );
            }
        }
        scope = current.parent();
    }
    // Special forms take precedence over same-name bindings, matching the
    // interpreter; they overwrite the environment entry.
    for name in SPECIAL_FORMS {
        by_name.insert(name.to_string(), special_form_description(name));
    }

    // Case-insensitive ASCII match on name or existing documentation. A
    // binding whose stale doc disappeared with its redefinition no longer
    // matches (the description above is computed from the live value).
    let mut matched: Vec<(String, HashMap<Value, Value>)> = Vec::new();
    for (name, description) in by_name {
        let doc_matched = match description_field(&description, "doc") {
            Value::String(doc) => doc.to_ascii_lowercase().contains(&needle),
            _ => false,
        };
        if name.to_ascii_lowercase().contains(&needle) || doc_matched {
            matched.push((name, description));
        }
    }
    matched.sort_by(|a, b| a.0.cmp(&b.0));

    let total = matched.len();
    let items: Vector<Value> = matched
        .iter()
        .skip(offset)
        .take(limit)
        .map(|(_, description)| Value::Map(description.clone()))
        .collect();
    let next_offset = if offset.saturating_add(limit) < total {
        Some(Value::Integer((offset + limit) as i64))
    } else {
        None
    };

    let mut result = HashMap::new();
    result.insert(map_key("items"), Value::Vector(items));
    result.insert(map_key("offset"), Value::Integer(offset as i64));
    result.insert(map_key("next-offset"), next_offset.unwrap_or(Value::Nil));
    result.insert(map_key("schema-version"), Value::Integer(1));
    result.insert(
        map_key("runtime-version"),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    Ok(Value::Map(result))
}

/// `(module-exports name)` → module description plus name-sorted
/// `:exports`; never auto-loads, never lists non-owned bindings.
fn module_exports_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let parts = module_parts(&args[0])?;
    Ok(match engine.find_module(&parts) {
        Some(module) => Value::Map(module_description(engine, &module)),
        None => Value::Map(unloaded_module_description(&parts)),
    })
}

/// `(language-capabilities)` → capability map; host I/O grants, pause,
/// and GC are never asserted here.
fn language_capabilities_fn(
    args: Vector<Value>,
    _engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    if !args.is_empty() {
        return Err(EvalError::wrong_arg_count(0, args.len()));
    }
    let mut map = HashMap::new();
    map.insert(map_key("schema-version"), Value::Integer(1));
    map.insert(
        map_key("runtime-version"),
        Value::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    let target = if cfg!(target_arch = "wasm32") {
        "wasm"
    } else {
        "native"
    };
    map.insert(map_key("target"), Value::String(target.to_string()));
    map.insert(map_key("structured-query"), Value::Boolean(true));
    // The local query service is available; remote restart of a paused
    // run is not shipped and must not be claimed.
    map.insert(map_key("remote-restart"), Value::Boolean(false));
    Ok(Value::Map(map))
}

// ── Registration ───────────────────────────────────────────────────

static DESCRIBE_SYMBOL_META: NativeMetadata = NativeMetadata {
    params: &["name"],
    rest: None,
    doc: "Return a structured description map for a root-visible binding or loaded module. A symbol or string addresses the root environment; a qualified 'module/name' resolves only explicit exports of an already-loaded module; a keyword addresses the module namespace without loading.",
};

static HELP_META: NativeMetadata = NativeMetadata {
    params: &["name"],
    rest: None,
    doc: "Render the description of a root-visible binding or loaded module as human-readable text through the host I/O and return nil. Scripts and the REPL share this path; the target value's printer is never invoked.",
};

static APROPOS_META: NativeMetadata = NativeMetadata {
    params: &["needle"],
    rest: Some("options"),
    doc: "Search root-visible names and existing documentation for a case-insensitive ASCII substring and return {:items ... :offset ... :next-offset ...} sorted by name. Optional offset (default 0) and limit (default 20, 1..=100) page the results.",
};

static MODULE_EXPORTS_META: NativeMetadata = NativeMetadata {
    params: &["name"],
    rest: None,
    doc: "Return the module description plus a name-sorted :exports vector listing only owned, explicitly exported bindings. Never auto-loads the module.",
};

static LANGUAGE_CAPABILITIES_META: NativeMetadata = NativeMetadata {
    params: &[],
    rest: None,
    doc: "Return the current runtime's declared capabilities: schema and runtime version, compile target, structured query availability, and remote-restart: false. Host I/O grants, pause, and GC are not asserted here.",
};

/// Install the query service bindings into the root environment.
/// Additive only: existing registrations and signatures are untouched.
pub fn register(env: &Arc<Env>) {
    env.set(
        "describe-symbol".into(),
        Value::NativeFunction(
            NativeFn::new("describe-symbol", describe_symbol_fn)
                .with_metadata(&DESCRIBE_SYMBOL_META),
        ),
    );
    env.set(
        "help".into(),
        Value::NativeFunction(NativeFn::new("help", help_fn).with_metadata(&HELP_META)),
    );
    env.set(
        "apropos".into(),
        Value::NativeFunction(NativeFn::new("apropos", apropos_fn).with_metadata(&APROPOS_META)),
    );
    env.set(
        "module-exports".into(),
        Value::NativeFunction(
            NativeFn::new("module-exports", module_exports_fn).with_metadata(&MODULE_EXPORTS_META),
        ),
    );
    env.set(
        "language-capabilities".into(),
        Value::NativeFunction(
            NativeFn::new("language-capabilities", language_capabilities_fn)
                .with_metadata(&LANGUAGE_CAPABILITIES_META),
        ),
    );
}
