use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bootstrap::{eval_source, language_context, ModuleRoots};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::error::EvalError;
use zio_core::value::Value;

// ── Context setup ───────────────────────────────────────────────

/// Module roots the language entry point is allowed to load from.
///
/// The explicit `--lib-dir`/`ZIO_PATH` roots come first; the process
/// CWD is appended last so a local script tree still works. Both are
/// real paths — no ambient "searched the CWD and also ZIO_PATH" inside
/// the core, and no symlink out of a granted root.
fn language_roots(explicit: &[PathBuf]) -> Result<ModuleRoots, EvalError> {
    let mut roots: Vec<PathBuf> = explicit.to_vec();
    if let Some(paths) = std::env::var_os("ZIO_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    ModuleRoots::new(roots)
}

/// Build the REPL/script evaluation context. The context owns the
/// SourceMap: every parsed source (script, REPL input, required module,
/// loaded file) registers there so spans resolve against one registry.
fn make_ctx(roots: ModuleRoots) -> Result<EvalContext, EvalError> {
    language_context(roots)
}

fn repl_ctx() -> Result<EvalContext, EvalError> {
    make_ctx(language_roots(&[])?)
}

// ── Script runner ───────────────────────────────────────────────

/// Run a script in the language context, without application host bindings.
fn run_script(path: &str, extra_roots: &[PathBuf]) -> Result<Value, EvalError> {
    let ctx = script_ctx(path, extra_roots)?;
    let source = std::fs::read_to_string(path)
        .map_err(|e| EvalError::custom(format!("cannot read {path}: {e}")))?;
    eval_source(&ctx, path, &source)
}

/// A script's own directory is granted first, so a script sitting next to
/// its libraries can `require` them without any ambient configuration.
fn script_ctx(path: &str, extra_roots: &[PathBuf]) -> Result<EvalContext, EvalError> {
    let mut roots = extra_roots.to_vec();
    if let Some(parent) = Path::new(path).parent().filter(|p| !p.as_os_str().is_empty()) {
        roots.push(parent.to_path_buf());
    }
    make_ctx(language_roots(&roots)?)
}

// ── REPL ────────────────────────────────────────────────────────

fn run_repl() {
    let ctx = match repl_ctx() {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };
    let sm = Arc::clone(ctx.source_map());

    println!("Zio REPL");
    println!("Press Ctrl+D or type (exit) to quit");

    let mut expr_counter = 0u64;
    loop {
        print!("zio> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("Error reading input: {e}");
                break;
            }
        }

        let input = input.trim();
        if input.is_empty() || input == "(exit)" {
            break;
        }

        expr_counter += 1;
        let name = format!("repl:{}", expr_counter);
        let source_id = sm.register(name.clone(), input.to_string());

        match zio_core::reader::read_with_source(input, source_id) {
            Ok(sexp) => match zio_core::eval::eval_in_context(&sexp, &ctx) {
                Ok(val) => {
                    // Pretty-print compound values for readability
                    match &val {
                        Value::List(_) | Value::Vector(_) | Value::Map(_) => {
                            let mut output = String::new();
                            let _ = val.pretty_print(&mut output, 0);
                            println!("{output}");
                        }
                        _ => println!("{val}"),
                    }
                }
                Err(e) => eprintln!("Error: {e}"),
            },
            Err(e) => eprintln!("Parse error: {e}"),
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────

const USAGE: &str = "Usage: zio [--lib-dir DIR]... [script.zio]";

/// `zio [--flags] [script]` — flags may precede the script path.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut script: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--lib-dir" => {
                i += 1;
                match args.get(i) {
                    Some(dir) => extra_roots.push(PathBuf::from(dir)),
                    None => {
                        eprintln!("--lib-dir needs a directory\n{USAGE}");
                        std::process::exit(2);
                    }
                }
            }
            other if other.starts_with("--") => {
                eprintln!("unknown option: {other}\n{USAGE}");
                std::process::exit(2);
            }
            other => {
                if script.is_some() {
                    eprintln!("unexpected argument: {other}\n{USAGE}");
                    std::process::exit(2);
                }
                script = Some(other.to_string());
            }
        }
        i += 1;
    }

    let Some(path) = script else {
        run_repl();
        return;
    };

    let result = run_script(&path, &extra_roots);

    match result {
        Ok(val) => {
            if val != Value::Nil {
                println!("{val}");
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}
