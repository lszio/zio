//! Printing, reading, and file I/O builtins (through the injectable IoHost
//! where applicable; file operations use the host filesystem directly).

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

/// How `print` and `println` render one argument.
///
/// A string prints as itself. `Display` escapes it, which is right for
/// `str` (where the value has to be readable as a literal) and wrong for
/// `print` (where the caller has a string in hand and wants it on the
/// line). `prn` is the one that escapes.
fn print_arg(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => format!("{other}"),
    }
}

pub fn print_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let s: String = args.iter().map(print_arg).collect::<Vec<_>>().join(" ");
    engine.io().print(&s)?;
    Ok(Value::Nil)
}

pub fn println(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let s: String = args.iter().map(print_arg).collect::<Vec<_>>().join(" ");
    engine.io().println(&s)?;
    Ok(Value::Nil)
}

pub fn prn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    // `prn` prints the readable form: a string keeps its quotes, a
    // nested value stays inspectable. `Debug` on `Value` would print the
    // enum variant instead, which is what the derive does — so a quoted
    // string is built here rather than delegated.
    let render = |value: &Value| -> String {
        match value {
            Value::String(text) => {
                serde_json::to_string(text).unwrap_or_else(|_| format!("{text:?}"))
            }
            Value::List(_) | Value::Vector(_) | Value::Map(_) => {
                let mut text = String::new();
                let _ = value.pretty_print(&mut text, 0);
                text
            }
            other => format!("{other}"),
        }
    };
    let s: String = args.iter().map(render).collect::<Vec<_>>().join(" ");
    engine.io().println(&s)?;
    Ok(Value::Nil)
}

pub fn read_line(_args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    match engine.io().read_line() {
        Err(_) => Ok(Value::Nil),
        Ok(input) => {
            let trimmed = input.trim_end_matches('\n').trim_end_matches('\r');
            Ok(Value::String(trimmed.to_string()))
        }
    }
}

/// (pprint value) → nil — pretty-print a value with indentation.
pub fn pprint_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let mut output = String::new();
    args[0]
        .pretty_print(&mut output, 0)
        .map_err(|_| EvalError::custom("pprint formatting error"))?;
    engine.io().println(&output)?;
    Ok(Value::Nil)
}

pub fn file_exists_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s,
        other => return Err(EvalError::type_error("string path", other.value_type())),
    };
    Ok(Value::Boolean(engine.io().file_exists(path)?))
}

pub fn read_string_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s,
        other => return Err(EvalError::type_error("string", other.value_type())),
    };
    let sexp = crate::reader::reader::read(s).map_err(|e| EvalError::custom(e.to_string()))?;
    Ok(Value::from(sexp))
}

// ── File Loading ──────────────────────────────────────────────────

/// (load path) → last value — read and evaluate a file.
/// Path resolution and file access go through the host's IoHost (ADR-011).
fn resolve_builtin_path(
    path: &str,
    engine: &dyn EvalEngine,
) -> Result<std::path::PathBuf, EvalError> {
    let p = std::path::PathBuf::from(path);
    if p.is_absolute() {
        return Ok(p);
    }
    // The source's own directory first, then the working directory.
    // An installed application is launched from wherever the user
    // happens to be, so a sibling `(load "contracts.zio")` has to
    // resolve against the tree it ships in rather than the shell's
    // position; the CWD stays as the fallback for a script that
    // deliberately loads by relative path.
    if let Some(base) = engine.source_dir() {
        let candidate = base.join(&p);
        if engine.io().file_exists(&candidate.to_string_lossy())? {
            return Ok(candidate);
        }
    }
    let cwd = engine.io().current_dir()?;
    Ok(std::path::PathBuf::from(cwd).join(p))
}

pub fn do_load(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string", other.value_type())),
    };
    let resolved = resolve_builtin_path(&path, engine)?;
    let source = engine.io().read_file(&resolved.to_string_lossy())?;
    // Evaluate every top-level form, not just the first. Spans are kept and
    // registered in the context's SourceMap so runtime errors inside the
    // file carry line/column information.
    let source_id = engine
        .source_map()
        .register(resolved.to_string_lossy().into_owned(), source.clone());
    let forms = crate::reader::reader::read_program_with_source(&source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", resolved.display())))?;
    let env = engine.env();
    let mut last = Value::Nil;
    for sexp in forms {
        last = engine.eval_expr(&sexp, env, false)?.into_value();
    }
    Ok(last)
}

// ── File I/O ──────────────────────────────────────────────────────

/// (slurp path) → string — read entire file into a string (via IoHost).
pub fn slurp(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string", format!("{other}"))),
    };
    engine
        .io()
        .read_file(&path)
        .map(Value::String)
        .map_err(|e| EvalError::custom(format!("slurp error: {e}")))
}

/// (spit path content) → nil — write string to a file (via IoHost).
pub fn spit(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string", format!("{other}"))),
    };
    let content = match &args[1] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string", format!("{other}"))),
    };
    engine
        .io()
        .write_file(&path, &content)
        .map_err(|e| EvalError::custom(format!("spit error: {e}")))?;
    Ok(Value::Nil)
}

pub fn register(env: &Arc<Env>) {
    env.set(
        "print".into(),
        Value::NativeFunction(NativeFn::new("print", print_fn)),
    );
    env.set(
        "println".into(),
        Value::NativeFunction(NativeFn::new("println", println)),
    );
    env.set(
        "prn".into(),
        Value::NativeFunction(NativeFn::new("prn", prn)),
    );
    env.set(
        "read-line".into(),
        Value::NativeFunction(NativeFn::new("read-line", read_line)),
    );
    env.set(
        "pprint".into(),
        Value::NativeFunction(NativeFn::new("pprint", pprint_fn)),
    );
    env.set(
        "file-exists?".into(),
        Value::NativeFunction(NativeFn::new("file-exists?", file_exists_fn)),
    );
    env.set(
        "read-string".into(),
        Value::NativeFunction(NativeFn::new("read-string", read_string_fn)),
    );
    env.set(
        "load".into(),
        Value::NativeFunction(NativeFn::new("load", do_load)),
    );
    env.set(
        "slurp".into(),
        Value::NativeFunction(NativeFn::new("slurp", slurp)),
    );
    env.set(
        "spit".into(),
        Value::NativeFunction(NativeFn::new("spit", spit)),
    );
}
