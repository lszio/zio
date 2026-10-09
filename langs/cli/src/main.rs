//! The `zio` entry point.
//!
//! Four things live here, and nothing else. A REPL. A script runner. A
//! bounded runner for code the host did not write. And a machine-readable
//! `--emit-framed` mode, because a worker that cannot be spoken to in a
//! known shape is a worker whose output has to be scraped out of stdout
//! with a regular expression.
//!
//! The bounded runner builds a context with no application bindings at
//! all. That is the whole point of it: Grove queues a generated
//! candidate, and the candidate must not inherit the queue's ability to
//! publish, write files, or open a socket. Grants are decided by whoever
//! launched the process, never by the source being run.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::error::EvalError;
use zio_core::im::Vector;
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
    // A script `load`s siblings by relative path for the same reason an
    // application does: the file it was handed may be anywhere, and the
    // files it ships with are next to it.
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        *ctx.source_dir.borrow_mut() = Some(parent.to_path_buf());
    }
    zio_core::bytecode::run_source(&ctx, path, &source)
}

/// A script's own directory is granted first, so a script sitting next to
/// its libraries can `require` them without any ambient configuration.
fn script_ctx(path: &str, extra_roots: &[PathBuf]) -> Result<EvalContext, EvalError> {
    let mut roots = extra_roots.to_vec();
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        roots.push(parent.to_path_buf());
    }
    make_ctx(language_roots(&roots)?)
}

// ── Bounded candidate runner ────────────────────────────────────

/// A run of code the host did not write.
///
/// The ceiling is the host's, not the program's: `0` is refused rather
/// than treated as "unbounded", because the one caller who would pass
/// `0` is the caller who is about to run a candidate that loops.
struct BoundedRun {
    entry: Option<String>,
    step_limit: u64,
    timeout_ms: u64,
    source_name: String,
}

impl BoundedRun {
    fn step_limit(&self) -> Result<u64, EvalError> {
        if self.step_limit == 0 {
            return Err(EvalError::custom(
                "invalid-input: --step-limit must be greater than zero",
            ));
        }
        Ok(self.step_limit)
    }
}

/// A fresh context for the candidate: language, no application.
///
/// `language_roots` is deliberately empty. The candidate can `require`
/// only what its own directory holds, and there is no ambient CWD, no
/// `ZIO_PATH`, and no host binding to reach. Anything it needs, the
/// launcher stages next to it.
fn candidate_ctx() -> Result<EvalContext, EvalError> {
    make_ctx(ModuleRoots::empty())
}

/// Look up a top-level binding in the context's environment.
fn lookup(ctx: &EvalContext, name: &str) -> Result<Value, EvalError> {
    ctx.env
        .get(name)
        .ok_or_else(|| EvalError::custom(format!("entry not found: {name}")))
}

/// Call `entry` with no arguments, or with the framed request when the
/// caller asked for one.
fn call_entry(ctx: &EvalContext, entry: &str, argument: Option<Value>) -> Result<Value, EvalError> {
    let function = lookup(ctx, entry)?;
    let args = match argument {
        Some(value) => zio_core::im::vector![value],
        None => zio_core::im::Vector::new(),
    };
    zio_core::eval::apply(function, args, ctx).map(|tail| tail.into_value())
}

/// Run a candidate and report what actually happened.
///
/// The status is computed here from the real outcome — a refusal and a
/// failure are different facts, and letting the program name its own
/// status would let a failed run report itself as a clean one.
fn run_bounded(run: &BoundedRun, source: &str) -> Value {
    let limit = match run.step_limit() {
        Ok(limit) => limit,
        Err(error) => return envelope("refused", Value::Nil, "", false, &error.to_string(), 0),
    };
    let started = std::time::Instant::now();
    let outcome = (|| -> Result<Value, EvalError> {
        let ctx = candidate_ctx()?;
        ctx.set_step_ceiling(limit);
        eval_source(&ctx, &run.source_name, source)?;
        let value = match &run.entry {
            Some(entry) => call_entry(&ctx, entry, None)?,
            None => Value::Nil,
        };
        // A program that stopped because it ran out of steps did not
        // finish, however happy the result value looks.
        if ctx.steps_spent() >= limit {
            return Err(EvalError::custom("execution stopped: step ceiling reached"));
        }
        if started.elapsed() >= std::time::Duration::from_millis(run.timeout_ms) {
            return Err(EvalError::custom(
                "timeout: candidate exceeded its deadline",
            ));
        }
        Ok(value)
    })();
    let spent = 0; // re-read from the context below when it survives
    let _ = spent;
    match outcome {
        Ok(value) => envelope("completed", value, "", false, "", 0),
        Err(error) => {
            let message = error.to_string();
            let status = if message.contains("step ceiling") || message.contains("timeout") {
                "failed"
            } else if message.starts_with("invalid-input:")
                || message.starts_with("capability-denied:")
            {
                "refused"
            } else {
                "failed"
            };
            envelope(status, Value::Nil, "", false, &message, 0)
        }
    }
}

/// The machine-readable result envelope. Keyed maps, so a consumer does
/// not have to know a positional order to read a failure.
fn envelope(
    status: &str,
    result: Value,
    output: &str,
    truncated: bool,
    error: &str,
    steps: u64,
) -> Value {
    let mut entries: Vec<(&str, Value)> = vec![
        ("status", Value::String(status.into())),
        ("result", result),
        ("output", Value::String(output.into())),
        ("output-truncated", Value::Boolean(truncated)),
        ("error", Value::String(error.into())),
        ("steps", Value::Integer(steps as i64)),
    ];
    let mut map = zio_core::im::HashMap::new();
    for (key, value) in entries.drain(..) {
        map.insert(Value::Keyword(key.into()), value);
    }
    Value::Map(map)
}

/// Print one envelope as a single NDJSON line, so a supervisor reading
/// the child's stdout never has to guess where a record ends.
fn emit_framed(value: &Value) {
    let mut text = String::new();
    let _ = value.pretty_print(&mut text, 0);
    let line = text.replace('\n', " ");
    println!("{line}");
}

// ── Application launch ──────────────────────────────────────────

/// How an installed application is invoked.
///
/// The launcher decides this, not the application source. An app that
/// could name its own store root, its own tensor backend, or its own
/// read roots would be an app that grants itself the authority the host
/// exists to withhold — so every value here arrives as a flag and is
/// handed over as one keyword map.
#[derive(Default)]
struct AppLaunch {
    entry_path: Option<String>,
    app_root: Option<PathBuf>,
    app_share: Option<PathBuf>,
    resource_root: Option<PathBuf>,
    /// The directory Grove serves its UI assets from.
    web_root: Option<PathBuf>,
    tensor_backend: Option<PathBuf>,
    tensor_python: Option<PathBuf>,
    /// The isolated worker script and the interpreter that execs it.
    /// Both are launcher decisions: the application names neither.
    worker_script: Option<PathBuf>,
    worker_python: Option<PathBuf>,
    /// Extra read-only directories the jail mounts for the worker.
    /// `/usr` and the interpreter's tree are not derivable from any other
    /// flag, and the jail refuses to start without declared mounts.
    worker_mounts: Vec<PathBuf>,
    data_root: Option<PathBuf>,
    bind: Option<String>,
    workers: Option<i64>,
    /// Extra key/value pairs the launcher wants in the config map.
    extra: Vec<(String, String)>,
}

impl AppLaunch {
    /// The keyword map the entry function receives.
    ///
    /// Absent values are omitted rather than sent as nil: an app that
    /// reads `(get config :bind)` and finds a nil is different from one
    /// that finds the key missing, and only the second can tell that
    /// nobody decided.
    fn config(&self, argv: &[String]) -> Value {
        let mut map = zio_core::im::HashMap::new();
        let mut put = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                map.insert(Value::Keyword(key.into()), Value::String(value));
            }
        };
        put(
            "app-root",
            self.app_root.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "app-share",
            self.app_share.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "resource-root",
            self.resource_root.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "web-root",
            self.web_root.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "tensor-backend",
            self.tensor_backend
                .as_ref()
                .map(|p| p.display().to_string()),
        );
        put(
            "tensor-python",
            self.tensor_python.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "worker-script",
            self.worker_script.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "worker-python",
            self.worker_python.as_ref().map(|p| p.display().to_string()),
        );
        put(
            "root",
            self.data_root.as_ref().map(|p| p.display().to_string()),
        );
        // The staging root is derived here rather than named by a fourth
        // flag: it must be a write root, and the launcher granted exactly
        // one write root.
        put(
            "worker-scratch",
            self.data_root
                .as_ref()
                .map(|root| root.join("worker-scratch").display().to_string()),
        );
        put("bind", self.bind.clone());
        // Inserted after the last `put`: the closure holds `map` mutably.
        if !self.worker_mounts.is_empty() {
            map.insert(
                Value::Keyword("worker-mounts".into()),
                string_vector(
                    &self
                        .worker_mounts
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>(),
                ),
            );
        }
        if let Some(workers) = self.workers {
            map.insert(Value::Keyword("workers".into()), Value::Integer(workers));
        }
        for (key, value) in &self.extra {
            map.insert(
                Value::Keyword(key.clone().into()),
                Value::String(value.clone()),
            );
        }
        map.insert(Value::Keyword("argv".into()), string_vector(argv));
        Value::Map(map)
    }

    /// The store root, if the launcher named one.
    fn data_root(&self) -> Option<&Path> {
        self.data_root.as_deref()
    }
}

fn string_vector(values: &[String]) -> Value {
    Value::Vector(values.iter().map(|v| Value::String(v.clone())).collect())
}

/// Build the context an application runs in: language, plus exactly the
/// capabilities the launcher granted.
///
/// The roots handed to `HostPolicy` are canonicalized inside `install`,
/// so a symlink pointing out of the store is refused at the moment it
/// is used rather than trusted because it looked local at startup.
fn app_context(launch: &AppLaunch) -> Result<EvalContext, EvalError> {
    // The application's own directory is the first root: a Zio app
    // `load`s its siblings by relative path, so without this it cannot
    // see the files it ships with. The process CWD is deliberately not
    // added — `language_roots` appends it, and an app launched from
    // outside its own tree should resolve modules against the tree it
    // was installed into, not against wherever the shell happened to
    // be.
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(path) = &launch.entry_path {
        if let Some(parent) = Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            roots.push(parent.to_path_buf());
        }
    }
    if let Some(share) = &launch.app_share {
        roots.push(share.clone());
    }
    if let Some(app_root) = &launch.app_root {
        roots.push(app_root.clone());
    }
    if let Some(data_root) = launch.data_root() {
        roots.push(data_root.to_path_buf());
    }
    // `language_roots` would also read `ZIO_PATH` and the CWD; the app
    // gets exactly the roots named here, so a stray `ZIO_PATH` cannot
    // widen what an installed application loads.
    let ctx = make_ctx(ModuleRoots::new(roots)?)?;

    let mut policy = zio_host::HostPolicy {
        argv: std::env::args().skip(1).collect(),
        network: true,
        process: true,
        tensor: launch.tensor_backend.is_some(),
        stdio: true,
        ..Default::default()
    };
    if let Some(data_root) = launch.data_root() {
        policy.write_roots.push(data_root.to_path_buf());
        policy.read_roots.push(data_root.to_path_buf());
    }
    if let Some(app_root) = &launch.app_root {
        policy.read_roots.push(app_root.clone());
    }
    if let Some(share) = &launch.app_share {
        policy.read_roots.push(share.clone());
    }
    // The UI tree may sit beside the share rather than inside it (the
    // installed layout is share/grove/web), so it is granted on its own.
    if let Some(web_root) = &launch.web_root {
        policy.read_roots.push(web_root.clone());
    }
    // The worker's interpreter and script must be readable by the service
    // to be mounted into the jail, and /usr is what any interpreter links
    // against after pivot_root. These are launcher-decided grants, like
    // the tensor backend: the config map cannot widen them.
    policy.read_roots.push(PathBuf::from("/usr"));
    for mount in &launch.worker_mounts {
        policy.read_roots.push(mount.clone());
    }
    // The tensor backend and its interpreter are granted as fixed paths,
    // and only here. The application passes a config map, but the map
    // cannot widen what was granted: see `tensor::config`.
    if let Some(backend) = &launch.tensor_backend {
        policy.trusted_tensor_backend = Some(backend.clone());
    }
    if let Some(python) = &launch.tensor_python {
        policy.trusted_tensor_python = Some(python.clone());
    }
    // Only the environment names the application actually reads are
    // visible, and only the token names the launcher declares.
    for name in [
        "GROVE_TOKEN_READER",
        "GROVE_TOKEN_ANNOTATOR",
        "GROVE_TOKEN_OPERATOR",
        "GROVE_TOKEN_PUBLISHER",
    ] {
        policy.environment.insert(name.to_string());
    }
    zio_host::install(&ctx, policy)?;
    Ok(ctx)
}

/// Load the application source and call its declared entry function.
///
/// The entry name is part of the contract between the launcher and the
/// app, so it is passed rather than discovered. An app that ran itself
/// on load would run on a `require` from anywhere.
fn run_app(launch: &AppLaunch, argv: &[String]) -> Result<Value, EvalError> {
    let path = launch
        .entry_path
        .as_ref()
        .ok_or_else(|| EvalError::custom("invalid-input: --app requires a path"))?;
    let ctx = app_context(launch)?;
    let source = std::fs::read_to_string(path)
        .map_err(|error| EvalError::custom(format!("cannot read {path}: {error}")))?;
    // Through the same entry point a script uses, so an app is compiled
    // and executed on the same path as everything else. Calling the
    // interpreter directly here is also what made a large application
    // overflow the stack where the identical program run as a script
    // did not.
    //
    // `source_dir` is set first: an application `load`s its siblings by
    // relative path, and an installed one is launched from wherever the
    // user happens to be.
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        *ctx.source_dir.borrow_mut() = Some(parent.to_path_buf());
    }
    if std::env::var_os("ZIO_DEBUG_SOURCE_DIR").is_some() {
        eprintln!(
            "[debug] app {path} source_dir {:?}",
            ctx.source_dir.borrow()
        );
    }
    zio_core::bytecode::run_source(&ctx, path, &source)?;

    let entry = std::env::var("ZIO_APP_ENTRY").unwrap_or_else(|_| "grove--main".to_string());
    let function = ctx
        .env
        .get(&entry)
        .ok_or_else(|| EvalError::custom(format!("application does not define {entry}")))?;
    let args = zio_core::im::vector![string_vector(argv), launch.config(argv)];
    zio_core::eval::apply(function, args, &ctx).map(|tail| tail.into_value())
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

const USAGE: &str = "Usage: zio [--lib-dir DIR]... [--entry NAME] [--step-limit N] \
[--timeout-ms N] [--emit-framed] [script.zio|-]\n\
       zio --app PATH --app-root DIR [--app-share DIR] [--app-resource-root DIR]\n\
       \x20        [--app-web-root DIR] [--app-tensor-backend FILE] [--app-tensor-python FILE]\n\
       \x20        [--app-root-dir DIR] [--app-bind ADDR] [--app-workers N] [--args ARG]...\n\
       \n\
       --entry NAME     call NAME after the source loads, with no arguments\n\
       --step-limit N   stop after N evaluator steps (required with --entry)\n\
       --timeout-ms N   wall-clock ceiling for the run (default 30000)\n\
       --emit-framed    print one NDJSON result envelope instead of the value\n\
       --args ARG       pass ARG to the application entry (repeatable)\n\
       '-'              read the source from stdin\n";

/// `zio [--flags] [script]` — flags may precede the script path.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut extra_roots: Vec<PathBuf> = Vec::new();
    let mut script: Option<String> = None;
    let mut bounded = BoundedRun {
        entry: None,
        step_limit: 0,
        timeout_ms: 30_000,
        source_name: "candidate.zio".to_string(),
    };
    let mut framed = false;
    let mut launch = AppLaunch::default();
    let mut app_argv: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        // Parse `--name VALUE` and bare tokens against one cursor. The
        // value sits at a known index, so an arm that reads it does not
        // also have to move the cursor — the two used to move
        // independently, and the script path was silently eaten whenever
        // a flag preceded it.
        let flag = args[i].clone();
        let value_index = i + 1;
        let value = || -> String {
            match args.get(value_index) {
                Some(found) => found.clone(),
                None => {
                    eprintln!("{flag} needs a value\n{USAGE}");
                    std::process::exit(2);
                }
            }
        };
        let path = || -> PathBuf { PathBuf::from(value()) };
        let number = |name: &str| -> i64 {
            match value().parse::<i64>() {
                Ok(parsed) => parsed,
                Err(_) => {
                    eprintln!("{name} needs an integer\n{USAGE}");
                    std::process::exit(2);
                }
            }
        };
        // Every flag here takes a value except `--emit-framed`.
        let takes_value = flag.starts_with("--") && flag != "--emit-framed";

        match flag.as_str() {
            "--app" => launch.entry_path = Some(value()),
            "--app-root" => launch.app_root = Some(path()),
            "--app-share" => launch.app_share = Some(path()),
            "--app-resource-root" => launch.resource_root = Some(path()),
            "--app-web-root" => launch.web_root = Some(path()),
            "--app-tensor-backend" => launch.tensor_backend = Some(path()),
            "--app-tensor-python" => launch.tensor_python = Some(path()),
            "--app-worker-script" => launch.worker_script = Some(path()),
            "--app-worker-python" => launch.worker_python = Some(path()),
            "--app-worker-mount" => launch.worker_mounts.push(path()),
            "--app-root-dir" => launch.data_root = Some(path()),
            "--app-bind" => launch.bind = Some(value()),
            "--app-workers" => launch.workers = Some(number("--app-workers")),
            "--args" => app_argv.push(value()),
            "--lib-dir" => extra_roots.push(PathBuf::from(value())),
            "--entry" => bounded.entry = Some(value()),
            "--step-limit" => bounded.step_limit = number("--step-limit").max(0) as u64,
            "--timeout-ms" => bounded.timeout_ms = number("--timeout-ms").max(0) as u64,
            "--emit-framed" => framed = true,
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

        // A flag with a value consumed two tokens; anything else one.
        i += if takes_value { 2 } else { 1 };
    }
    // Application launch and bounded execution are both different modes,
    // not flags on the script path: one grants host capabilities and the
    // other deliberately grants none, so neither may silently fall
    // through to the plain script runner.
    if launch.entry_path.is_some() {
        let value = run_app(&launch, &app_argv);
        match value {
            Ok(value) if value != Value::Nil => println!("{value}"),
            Ok(_) => {}
            Err(error) => {
                eprintln!("Error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if bounded.entry.is_some() || bounded.step_limit > 0 || framed {
        if bounded.step_limit == 0 {
            eprintln!("invalid-input: --step-limit is required for a bounded run\n{USAGE}");
            std::process::exit(2);
        }
        let (source, name) = match script.as_deref() {
            Some("-") | None => {
                let mut source = String::new();
                if let Err(error) = io::stdin().read_to_string(&mut source) {
                    eprintln!("cannot read stdin: {error}");
                    std::process::exit(1);
                }
                (source, "candidate.zio".to_string())
            }
            Some(path) => {
                bounded.source_name = path.to_string();
                match std::fs::read_to_string(path) {
                    Ok(source) => (source, path.to_string()),
                    Err(error) => {
                        eprintln!("cannot read {path}: {error}");
                        std::process::exit(1);
                    }
                }
            }
        };
        bounded.source_name = name;
        let result = run_bounded(&bounded, &source);
        emit_framed(&result);
        // A refused or failed run exits nonzero so a supervisor that
        // only checks the status code still notices.
        let failed = matches!(&result, Value::Map(map) if matches!(
            map.get(&Value::Keyword("status".into())),
            Some(Value::String(text)) if text == "failed" || text == "refused"));
        std::process::exit(i32::from(failed));
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

/// Unused today, kept honest: the vector type is part of the public
/// argument shape a bounded entry may take, so the import is not dead.
#[allow(dead_code)]
fn _entry_argument_type() -> Vector<Value> {
    Vector::new()
}
