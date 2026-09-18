use std::io::{self, Write};
use std::sync::Arc;

use zio_core::builtins;
use zio_core::context::EvalContext;
use zio_core::context::EvalRuntime;
use zio_core::context::ModuleRegistry;
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
        let forms = reader::reader::read_program_with_source(&source, source_id)
            .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path.display())))?;

        let module_env = Arc::new(Env::new(Some(parent_env.clone())));
        let ctx = EvalContext::with_loader(module_env.clone(), make_require_loader(&sm));
        // Collect (export ...) declarations from the module body.
        ctx.push_module_exports();
        for sexp in forms {
            eval::eval_in_context(&sexp, &ctx)
                .map_err(|e| EvalError::custom(format!("error loading module {}: {e}", name_str)))?;
        }
        let exports = ctx.take_module_exports();

        Ok(module::Module {
            name: mod_name.to_vec(),
            env: module_env.clone(),
            exports,
            source: None,
        })
    })
}

// ── Context setup ───────────────────────────────────────────────

/// Build the REPL/script evaluation context. The context owns the
/// SourceMap: every parsed source (script, REPL input, required module,
/// loaded file) registers there so spans resolve against one registry.
fn make_ctx() -> EvalContext {
    let env = make_root_env();
    let ctx = EvalContext::new(env);
    let loader = make_require_loader(ctx.source_map());
    *ctx.loader.borrow_mut() = Some(loader);
    ctx
}

fn make_root_env() -> Arc<Env> {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    env
}

// ── Script runner ───────────────────────────────────────────────

fn run_script(path: &str) -> Result<Value, EvalError> {
    run_script_with_llm(path, None)
}

/// Run a script with an optional replay llm host (ADR-016): the host is
/// attached from the outside, exactly as the zio-ai contract tests do.
fn run_script_with_llm(path: &str, llm: Option<Arc<dyn zio_ai::LlmHost>>) -> Result<Value, EvalError> {
    let ctx = make_ctx();
    zio_ai::install(&ctx, llm, None);
    load_stdlib(&ctx);
    let source = std::fs::read_to_string(path)
        .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path)))?;
    // Evaluate every top-level form directly (no wrapper form), so error
    // spans report exact script line/column positions.
    let source_id = ctx.source_map().register(path.to_string(), source.clone());
    let forms = reader::reader::read_program_with_source(&source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path)))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = eval::eval_in_context(&sexp, &ctx)?;
    }
    Ok(last)
}

// ── Stdlib loader ───────────────────────────────────────────────

fn load_stdlib(ctx: &EvalContext) {
    let source = zio_core::stdlib_source();
    let source_id = ctx.source_map().register("core.zio".into(), source.to_string());
    match reader::reader::read_program_with_source(source, source_id) {
        Ok(forms) => {
            for sexp in forms {
                if let Err(e) = eval::eval_in_context(&sexp, ctx) {
                    eprintln!("Warning: stdlib eval error: {e}");
                    break;
                }
            }
        }
        Err(e) => eprintln!("Warning: stdlib parse error: {e}"),
    }
}

// ── REPL ────────────────────────────────────────────────────────

fn run_repl() {
    let ctx = make_ctx();
    load_stdlib(&ctx);
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
    let args: Vec<String> = std::env::args().collect();

    match args.len() {
        1 => run_repl(),
        2 if args[1] == "repl" => run_repl(),
        2 => match run_script(&args[1]) {
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
        4 if args[1] == "--llm-replay" => {
            let host = zio_ai::mock::MockLlmHost::from_recording_file(&args[2])
                .map_err(|e| EvalError::custom(format!("--llm-replay: {e}")));
            let host = match host {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            };
            match run_script_with_llm(&args[3], Some(Arc::new(host))) {
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
            eprintln!("Usage: zio [--llm-replay recordings.zio] [script.zio]");
            std::process::exit(1);
        }
    }
}
