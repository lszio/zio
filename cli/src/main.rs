use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::eval;
use zio_core::builtins;
use zio_core::im;
use zio_core::module;
use zio_core::value::{Value, NativeFn};
use zio_reader::reader;

// ── Thread-local global state for (load) ─────────────────────────

use std::cell::RefCell;
thread_local! {
    static GLOBAL_ENV: RefCell<Option<Arc<Env>>> = const { RefCell::new(None) };
}

fn set_global_env(env: Arc<Env>) {
    GLOBAL_ENV.with(|cell| {
        *cell.borrow_mut() = Some(env);
    });
}

fn with_global_env<F, R>(f: F) -> Result<R, EvalError>
where
    F: FnOnce(&Arc<Env>) -> Result<R, EvalError>,
{
    GLOBAL_ENV.with(|cell| {
        let guard = cell.borrow();
        let env = guard.as_ref().ok_or_else(|| {
            EvalError::Custom("load called without a global environment".into())
        })?;
        f(env)
    })
}

// ── load: (load "path.zio") → last value ────────────────────────

fn do_load(args: im::Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount { expected: 1, got: args.len() });
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string", other.value_type())),
    };

    let resolved = resolve_path(&path)?;
    let source = std::fs::read_to_string(&resolved)
        .map_err(|e| EvalError::Custom(format!("cannot read {}: {e}", resolved.display())))?;
    let sexp = reader::read(&source)
        .map_err(|e| EvalError::Custom(format!("parse error in {}: {e}", resolved.display())))?;

    with_global_env(|env| eval::eval(&sexp, env))
}

fn resolve_path(path: &str) -> Result<PathBuf, EvalError> {
    let p = PathBuf::from(path);
    if p.is_relative() {
        let cwd = std::env::current_dir()
            .map_err(|e| EvalError::Custom(format!("cannot get cwd: {e}")))?;
        Ok(cwd.join(p))
    } else {
        Ok(p)
    }
}

fn register_load_fn(env: &Arc<Env>) {
    env.set("load".into(), Value::NativeFunction(NativeFn::new("load", do_load)));
}

// ── Script runner ───────────────────────────────────────────────

pub fn run_script(path: &str) -> Result<Value, EvalError> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| EvalError::Custom(format!("cannot read {path}: {e}")))?;
    let sexp = reader::read(&source)
        .map_err(|e| EvalError::Custom(format!("parse error in {path}: {e}")))?;
    let env = make_global_env();
    set_global_env(env.clone());
    eval::eval(&sexp, &env)
}

// ── Environment setup ───────────────────────────────────────────

pub fn make_global_env() -> Arc<Env> {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    register_load_fn(&env);

    // Register the require loader: resolves module name → file path → parse → eval
    module::set_require_loader(|mod_name, _source, parent_env| {
        let name_str = mod_name.join(".");

        // Resolve path from module name (e.g. "zio.math" → "./zio/math.zio")
        let path = module::resolve_module_path(&name_str)?;

        // Check for circular requires
        module::begin_loading(&path)?;

        // Read source
        let source = std::fs::read_to_string(&path)
            .map_err(|e| EvalError::Custom(format!("cannot read {}: {e}", path.display())))?;

        // Parse
        let sexp = zio_reader::reader::read(&source)
            .map_err(|e| EvalError::Custom(format!("parse error in {}: {e}", path.display())))?;

        // Eval in a module-scoped env (child of parent_env)
        let module_env = Arc::new(Env::new(Some(parent_env.clone())));
        let _ = eval::eval(&sexp, &module_env);

        module::end_loading(&path);

        Ok(zio_core::module::Module {
            name: mod_name.to_vec(),
            env: module_env.clone(),
            exports: Vec::new(),
            source: None,
        })
    });

    env
}

// ── REPL ────────────────────────────────────────────────────────

fn run_repl() {
    let env = make_global_env();
    set_global_env(env.clone());

    println!("Zio REPL (v0.2)");
    println!("Press Ctrl+D or type (exit) to quit");

    loop {
        print!("zio> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {
                let input = input.trim();
                if input.is_empty() {
                    continue;
                }

                if input == "(exit)" {
                    break;
                }

                match reader::read(input) {
                    Ok(sexp) => match eval::eval(&sexp, &env) {
                        Ok(res) => println!("{}", res),
                        Err(e) => println!("Error: {}", e),
                    },
                    Err(e) => println!("Parse Error: {}", e),
                }
            }
            Err(e) => {
                println!("Error reading input: {}", e);
                break;
            }
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.len() {
        1 => run_repl(),
        2 if args[1] == "repl" || args[1] == "help" => {
            if args[1] == "help" {
                println!("Zio v0.2");
                println!("  zio              REPL");
                println!("  zio repl         REPL");
                println!("  zio run <file>   Run a script file");
                println!("  zio <file>       Run a script file");
            }
            run_repl();
        }
        _ => {
            let file_arg = if args[1] == "run" && args.len() > 2 {
                &args[2]
            } else {
                &args[1]
            };

            match run_script(file_arg) {
                Ok(v) => {
                    if !matches!(v, Value::Nil) {
                        println!("{}", v);
                    }
                }
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}