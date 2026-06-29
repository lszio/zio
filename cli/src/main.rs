use std::io::{self, Write};
use std::sync::Arc;

use zio_core::builtins;
use zio_core::context::EvalContext;
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::eval;
use zio_core::module;
use zio_core::reader;
use zio_core::span::SourceMap;
use zio_core::value::Value;

// ── Require loader ──────────────────────────────────────────────

fn make_require_loader(sm: &Arc<SourceMap>) -> Box<zio_core::context::ModuleLoader> {
    let sm = Arc::clone(sm);
    Box::new(move |mod_name: &[String], _source: &str, parent_env: &Arc<Env>| {
        let name_str = mod_name.join(".");
        let path = module::resolve_module_path(&name_str)?;

        let source = std::fs::read_to_string(&path)
            .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path.display())))?;

        let source_id = sm.register(name_str.clone(), source.clone());
        let sexp = reader::read_with_source(&source, source_id)
            .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path.display())))?;

        let module_env = Arc::new(Env::new(Some(parent_env.clone())));
        let ctx = EvalContext::with_loader(module_env.clone(), make_require_loader(&sm));
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

// ── Context setup ───────────────────────────────────────────────

fn make_ctx(sm: &Arc<SourceMap>) -> EvalContext {
    let env = make_root_env();
    let loader = make_require_loader(sm);
    EvalContext::with_loader(env, loader)
}

fn make_root_env() -> Arc<Env> {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    env
}

// ── Script runner ───────────────────────────────────────────────

fn run_script(path: &str, sm: &Arc<SourceMap>) -> Result<Value, EvalError> {
    let ctx = make_ctx(sm);
    load_stdlib(&ctx, sm);
    let source = std::fs::read_to_string(path)
        .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path)))?;
    let source_id = sm.register(path.to_string(), source.clone());
    let sexp = reader::read_with_source(&source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path)))?;
    eval::eval_in_context(&sexp, &ctx)
}

// ── Stdlib loader ───────────────────────────────────────────────

fn load_stdlib(ctx: &EvalContext, sm: &Arc<SourceMap>) {
    let source = zio_core::stdlib_source();
    let wrapped = format!("(do\n{source}\n)");
    let source_id = sm.register("core.zio".into(), wrapped.clone());
    match reader::read_with_source(&wrapped, source_id) {
        Ok(sexp) => {
            if let Err(e) = eval::eval_in_context(&sexp, ctx) {
                eprintln!("Warning: stdlib eval error: {e}");
            }
        }
        Err(e) => eprintln!("Warning: stdlib parse error: {e}"),
    }
}

// ── REPL ────────────────────────────────────────────────────────

fn run_repl(sm: &Arc<SourceMap>) {
    let ctx = make_ctx(sm);
    load_stdlib(&ctx, sm);

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
        let source_id = sm.register(format!("repl:{}", expr_counter), input.to_string());

        match reader::read_with_source(input, source_id) {
            Ok(sexp) => match eval::eval_in_context(&sexp, &ctx) {
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

fn main() {
    let source_map = Arc::new(SourceMap::new());
    let args: Vec<String> = std::env::args().collect();

    match args.len() {
        1 => run_repl(&source_map),
        2 if args[1] == "repl" => run_repl(&source_map),
        2 => match run_script(&args[1], &source_map) {
            Ok(val) => {
                if val != Value::Nil {
                    println!("{val}");
                }
            }
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Usage: zio [script.zio]");
            std::process::exit(1);
        }
    }
}
