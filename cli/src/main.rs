use std::io::{self, Write};
use std::sync::Arc;

use zio_core::builtins;
use zio_core::context::EvalContext;
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::eval;
use zio_core::module;
use zio_core::value::Value;
use zio_core::reader;


// ── Script runner ───────────────────────────────────────────────

pub fn run_script(path: &str) -> Result<Value, EvalError> {
    let ctx = make_ctx();
    load_stdlib(&ctx);
    let source = std::fs::read_to_string(path)
        .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path)))?;
    let sexp = reader::read(&source)
        .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path)))?;
    eval::eval_in_context(&sexp, &ctx)
}

// ── Require loader ──────────────────────────────────────────────

fn make_require_loader() -> Box<zio_core::context::ModuleLoader> {
    Box::new(|mod_name: &[String], _source: &str, parent_env: &Arc<Env>| {
        let name_str = mod_name.join(".");
        let path = module::resolve_module_path(&name_str)?;

        let source = std::fs::read_to_string(&path)
            .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path.display())))?;

        let sexp = reader::read(&source)
            .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path.display())))?;

        let module_env = Arc::new(Env::new(Some(parent_env.clone())));
        let ctx = EvalContext::with_loader(module_env.clone(), make_require_loader());
        eval::eval_in_context(&sexp, &ctx)
            .map_err(|e| EvalError::custom(format!("error loading module {}: {e}", name_str)))?;

        Ok(module::Module {
            name: mod_name.to_vec(),
            env: module_env.clone(),
            exports: Vec::new(),
            source: None,
        })
    })
}

fn make_ctx() -> EvalContext {
    let env = make_root_env();
    EvalContext::with_loader(env, make_require_loader())
}

fn make_root_env() -> Arc<Env> {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    env
}

/// Load the core standard library at startup (embedded in zio-core).
fn load_stdlib(ctx: &EvalContext) {
    let source = zio_core::stdlib_source();
    // Wrap entire file in a (do ...) since reader::read only parses one form
    let wrapped = format!("(do\n{source}\n)");
    match reader::read(&wrapped) {
        Ok(sexp) => {
            if let Err(e) = eval::eval_in_context(&sexp, ctx) {
                eprintln!("Warning: stdlib eval error: {e}");
            }
        }
        Err(e) => eprintln!("Warning: stdlib parse error: {e}"),
    }
}

// ── REPL ────────────────────────────────────────────────────────

fn run_repl() {
    let ctx = make_ctx();
    load_stdlib(&ctx);

    println!("Zio REPL");
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

        match reader::read(input) {
            Ok(sexp) => match eval::eval_in_context(&sexp, &ctx) {
                Ok(val) => println!("{val}"),
                Err(e) => eprintln!("Error: {e}"),
            },
            Err(e) => eprintln!("Parse error: {e}"),
        }
    }
}

// ── Entry point ─────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.len() {
        1 => run_repl(),
        2 if args[1] == "repl" => run_repl(),
        2 => {
            match run_script(&args[1]) {
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
        _ => {
            eprintln!("Usage: zio [script.zio]");
            std::process::exit(1);
        }
    }
}