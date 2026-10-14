//! `zio image` and `zio compile`: save, inspect and run a still image,
//! and compile a program to a portable module. Entry parsing is shared
//! by both save and run paths, and both build their context through the
//! same helpers as the script runner — there is no second context
//! assembly for artifacts.
//!
//! Restoring is authority-gated: the host states its file grants before
//! any resource is created, the image's resource requirements are checked
//! against what the target context actually holds, and the restored
//! context's I/O is the granted host — never the source-tree one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bootstrap::{ModuleRoots, language_context_with_io};
use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::image::{ImageLimits, restore, save, wire};
use zio_core::io::IoHost;
use zio_core::value::Value;

pub const IMAGE_USAGE: &str = "Usage: zio image save PROGRAM.zio -o IMAGE [--root NAME]...\n       zio image inspect IMAGE\n       zio image run IMAGE [--eval EXPR] [--grant-dir DIR]...\n       zio compile PROGRAM.zio -o OUT\n\
       \n\
       save      evaluate PROGRAM and write a still image; roots default to the\n\
       \x20        program result (name root-0); --root NAME stores the binding NAME\n       inspect   print header, roots, modules and required capabilities (no restore)\n       run       restore IMAGE into an empty-root context, bind each root by name,\n\
       \x20        then evaluate --eval (default: the first root); file access is\n\
       \x20        limited to --grant-dir roots\n       compile   compile PROGRAM to a portable module at -o\n";

/// Entry for both subcommands; unknown subcommands are a usage error.
pub fn run_image(argv: &[String]) -> Result<(), EvalError> {
    let Some(sub) = argv.first().map(String::as_str) else {
        return Err(usage());
    };
    match sub {
        "save" => save_command(&argv[1..]),
        "inspect" => inspect_command(&argv[1..]),
        "run" => run_command(&argv[1..]),
        _ => Err(usage()),
    }
}

fn usage() -> EvalError {
    EvalError::invalid_form(IMAGE_USAGE.to_string())
}

struct SaveArgs {
    program: String,
    output: PathBuf,
    roots: Vec<String>,
}

fn parse_save(argv: &[String]) -> Result<SaveArgs, EvalError> {
    let mut program = None;
    let mut output = None;
    let mut roots = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "-o" => {
                index += 1;
                output = Some(PathBuf::from(argv.get(index).ok_or_else(|| {
                    EvalError::invalid_form("image save: -o needs a path")
                })?));
            }
            "--root" => {
                index += 1;
                roots.push(
                    argv.get(index)
                        .ok_or_else(|| EvalError::invalid_form("image save: --root needs a name"))?
                        .clone(),
                );
            }
            other if program.is_none() => program = Some(other.to_string()),
            other => {
                return Err(EvalError::invalid_form(format!(
                    "image save: unexpected argument {other:?}"
                )));
            }
        }
        index += 1;
    }
    Ok(SaveArgs {
        program: program
            .ok_or_else(|| EvalError::invalid_form("image save: PROGRAM is required"))?,
        output: output
            .ok_or_else(|| EvalError::invalid_form("image save: -o IMAGE is required"))?,
        roots,
    })
}

fn save_command(argv: &[String]) -> Result<(), EvalError> {
    let args = parse_save(argv)?;
    let source = std::fs::read_to_string(&args.program)
        .map_err(|error| EvalError::custom(format!("cannot read {}: {error}", args.program)))?;
    let ctx = script_context(&args.program)?;
    let result = zio_core::bootstrap::eval_source(&ctx, &args.program, &source)?;
    let roots: Vec<(String, Value)> = if args.roots.is_empty() {
        vec![("root-0".to_string(), result)]
    } else {
        args.roots
            .iter()
            .map(|name| {
                ctx.env
                    .get_owned(name)
                    .map(|value| (name.clone(), value))
                    .ok_or_else(|| {
                        EvalError::invalid_form(format!("image save: no binding named {name:?}"))
                    })
            })
            .collect::<Result<_, _>>()?
    };
    let image = save::save_named_image(&ctx, &roots, &ImageLimits::default())?;
    let canonical = image.canonical()?;
    std::fs::write(&args.output, canonical.as_bytes()).map_err(|error| {
        EvalError::custom(format!("cannot write {}: {error}", args.output.display()))
    })?;
    println!(
        "saved {} ({} bytes, {} roots, {} nodes)",
        args.output.display(),
        canonical.len(),
        image.roots.len(),
        image.nodes.len()
    );
    Ok(())
}

fn inspect_command(argv: &[String]) -> Result<(), EvalError> {
    let path = argv
        .first()
        .ok_or_else(|| EvalError::invalid_form("image inspect: IMAGE is required"))?;
    let bytes = std::fs::read(path)
        .map_err(|error| EvalError::custom(format!("cannot read {path}: {error}")))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| EvalError::custom(format!("image is not UTF-8: {error}")))?;
    // Metadata only: parse validates structure and bounds, and never
    // runs the restorer or touches the context.
    let image = wire::Image::parse(&text)?;
    println!(
        "format: {} v{}",
        image.header.format, image.header.format_version
    );
    println!("runtime: {}", image.header.runtime);
    println!("profile: {}", image.header.profile);
    println!("nodes: {}", image.nodes.len());
    for (index, id) in image.roots.iter().enumerate() {
        let name = image
            .root_names
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("root-{index}"));
        let kind = image.nodes[*id].kind.clone();
        println!("root: {name} -> node {id} ({kind})");
    }
    for module in &image.modules {
        let source = module.source.clone().unwrap_or_else(|| "<memory>".into());
        println!(
            "module: {} exports {} (source {source})",
            module.name,
            module.exports.len()
        );
    }
    if image.resources.is_empty() {
        println!("required capabilities: none");
    } else {
        for resource in &image.resources {
            println!("required capability: {} {}", resource.kind, resource.name);
        }
    }
    Ok(())
}

struct RunArgs {
    image: String,
    eval_expr: Option<String>,
    grant_dirs: Vec<PathBuf>,
}

fn parse_run(argv: &[String]) -> Result<RunArgs, EvalError> {
    let mut image = None;
    let mut eval_expr = None;
    let mut grant_dirs = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--eval" => {
                index += 1;
                eval_expr = Some(
                    argv.get(index)
                        .ok_or_else(|| {
                            EvalError::invalid_form("image run: --eval needs an expression")
                        })?
                        .clone(),
                );
            }
            "--grant-dir" => {
                index += 1;
                let dir = argv.get(index).ok_or_else(|| {
                    EvalError::invalid_form("image run: --grant-dir needs a directory")
                })?;
                let canonical = std::fs::canonicalize(dir)
                    .map_err(|error| EvalError::custom(format!("cannot grant {dir}: {error}")))?;
                grant_dirs.push(canonical);
            }
            other if image.is_none() => image = Some(other.to_string()),
            other => {
                return Err(EvalError::invalid_form(format!(
                    "image run: unexpected argument {other:?}"
                )));
            }
        }
        index += 1;
    }
    Ok(RunArgs {
        image: image.ok_or_else(|| EvalError::invalid_form("image run: IMAGE is required"))?,
        eval_expr,
        grant_dirs,
    })
}

fn run_command(argv: &[String]) -> Result<(), EvalError> {
    let args = parse_run(argv)?;
    let bytes = std::fs::read(&args.image)
        .map_err(|error| EvalError::custom(format!("cannot read {}: {error}", args.image)))?;
    let text = String::from_utf8(bytes)
        .map_err(|error| EvalError::custom(format!("image is not UTF-8: {error}")))?;
    let image = wire::Image::parse(&text)?;

    // Grants are stated before any resource exists: the restore context
    // gets the granted host, never the source-tree one.
    let io: Arc<dyn IoHost> = Arc::new(GrantIoHost {
        inner: zio_core::io::StdIoHost,
        roots: args.grant_dirs.clone(),
    });
    // Empty module roots: a restored image carries no source tree.
    let ctx = language_context_with_io(ModuleRoots::empty(), io)?;
    let roots = restore::restore_image(&ctx, &image, &ImageLimits::default())?;

    let names: Vec<String> = (0..roots.len())
        .map(|index| {
            image
                .root_names
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("root-{index}"))
        })
        .collect();
    for (name, value) in names.iter().zip(roots.iter()) {
        ctx.env.set(name.clone(), value.clone());
    }

    let expression = args
        .eval_expr
        .unwrap_or_else(|| names.first().cloned().unwrap_or_else(|| "nil".to_string()));
    let value = zio_core::bootstrap::eval_source(&ctx, &args.image, &expression)?;
    println!("{}", value);
    Ok(())
}

/// Shared context assembly for artifact commands: same language
/// bootstrap as the script runner, module roots taken from the program's
/// directory plus ZIO_PATH.
fn script_context(program: &str) -> Result<EvalContext, EvalError> {
    let mut roots = Vec::new();
    if let Some(parent) = Path::new(program)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        roots.push(parent.to_path_buf());
    }
    if let Some(paths) = std::env::var_os("ZIO_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    zio_core::bootstrap::language_context(ModuleRoots::new(roots)?)
}

/// `zio compile PROGRAM -o OUT`: compile a program to a portable module
/// artifact. No user program is compiled with rustc; the Zio compiler
/// produces the module and the runtime executes it.
pub fn run_compile(argv: &[String]) -> Result<(), EvalError> {
    let mut program = None;
    let mut output = None;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "-o" => {
                index += 1;
                output =
                    Some(PathBuf::from(argv.get(index).ok_or_else(|| {
                        EvalError::invalid_form("compile: -o needs a path")
                    })?));
            }
            other if program.is_none() => program = Some(other.to_string()),
            other => {
                return Err(EvalError::invalid_form(format!(
                    "compile: unexpected argument {other:?}"
                )));
            }
        }
        index += 1;
    }
    let program = program.ok_or_else(|| EvalError::invalid_form("compile: PROGRAM is required"))?;
    let output = output.ok_or_else(|| EvalError::invalid_form("compile: -o OUT is required"))?;
    let ctx = script_context(&program)?;
    let module = zio_core::bytecode::compile_source(
        &ctx,
        &program,
        &std::fs::read_to_string(&program)
            .map_err(|error| EvalError::custom(format!("cannot read {program}: {error}")))?,
    )?;
    let canonical = zio_core::bytecode::serialize_module(&module)?;
    std::fs::write(&output, canonical.as_bytes()).map_err(|error| {
        EvalError::custom(format!("cannot write {}: {error}", output.display()))
    })?;
    println!(
        "compiled {} -> {} ({} bytes)",
        program,
        output.display(),
        canonical.len()
    );
    Ok(())
}

/// The restored context's I/O: stdio passes through, but every file
/// operation must land inside a granted directory. A canary outside a
/// grant is refused — restore inherits no authority of its own.
struct GrantIoHost {
    inner: zio_core::io::StdIoHost,
    roots: Vec<PathBuf>,
}

impl GrantIoHost {
    fn check(&self, path: &str) -> Result<PathBuf, EvalError> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|error| EvalError::custom(format!("cannot resolve {path}: {error}")))?;
        if self.roots.iter().any(|root| canonical.starts_with(root)) {
            return Ok(canonical);
        }
        Err(EvalError::custom(format!(
            "file access denied: {path} is outside every granted root"
        )))
    }
}

impl IoHost for GrantIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        self.inner.print(msg)
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        self.inner.println(msg)
    }

    fn read_line(&self) -> Result<String, EvalError> {
        self.inner.read_line()
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        self.inner.current_dir()
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        std::fs::read_to_string(self.check(path)?)
            .map_err(|error| EvalError::custom(format!("cannot read {path}: {error}")))
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        std::fs::write(self.check(path)?, data)
            .map_err(|error| EvalError::custom(format!("cannot write {path}: {error}")))
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        Ok(self.check(path)?.exists())
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        Ok(self.check(path)?.to_string_lossy().into_owned())
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        Ok(std::fs::metadata(self.check(path)?)
            .map_err(|error| EvalError::custom(format!("cannot inspect {path}: {error}")))?
            .is_dir())
    }
}

// ── zio fmt ───────────────────────────────────────────────────────

const FMT_USAGE: &str = "Usage: zio fmt [--write] [--check] [FILE]...\n\
       \x20        --write   write the formatted source back to each file\n\
       \x20        --check   report files that are not formatted, exit 1 if any\n\
       \x20        with no FILE, read the source from stdin and write it to stdout\n";

/// Format Zio source, one file or stdin, and report what changed.
///
/// The default is stdout, because a formatter is a tool whose author
/// runs it on files under version control: rewriting the tree by
/// default would turn a typo into a diff nobody asked for. `--write`
/// and `--check` are the two explicit directions.
pub fn run_fmt(argv: &[String]) -> Result<(), EvalError> {
    let mut write = false;
    let mut check = false;
    let mut files: Vec<String> = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "--write" | "-w" => write = true,
            "--check" => check = true,
            "--help" | "-h" => {
                print!("{FMT_USAGE}");
                return Ok(());
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(EvalError::invalid_form(format!(
                    "fmt: unknown option {other}"
                )));
            }
            other => files.push(other.to_string()),
        }
        index += 1;
    }
    if write && check {
        return Err(EvalError::invalid_form(
            "fmt: --write and --check are different directions; pick one",
        ));
    }

    if files.is_empty() {
        let mut source = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut source)
            .map_err(|e| EvalError::custom(format!("fmt: cannot read stdin: {e}")))?;
        let formatted = zio_core::fmt::format_source(&source)?;
        if check {
            if formatted != source {
                println!("stdin is not formatted");
                std::process::exit(1);
            }
            return Ok(());
        }
        print!("{formatted}");
        return Ok(());
    }

    let mut unformatted = Vec::new();
    for file in &files {
        let source = std::fs::read_to_string(file)
            .map_err(|e| EvalError::custom(format!("fmt: cannot read {file}: {e}")))?;
        let formatted = zio_core::fmt::format_source(&source)?;
        if check {
            if formatted != source {
                unformatted.push(file.clone());
            }
            continue;
        }
        if write {
            if formatted != source {
                std::fs::write(file, &formatted)
                    .map_err(|e| EvalError::custom(format!("fmt: cannot write {file}: {e}")))?;
                println!("formatted {file}");
            }
        } else {
            print!("{formatted}");
        }
    }
    if check && !unformatted.is_empty() {
        for file in &unformatted {
            println!("would reformat {file}");
        }
        std::process::exit(1);
    }
    Ok(())
}

// ── zio lsp ───────────────────────────────────────────────────────

const LSP_USAGE: &str = "Usage: zio lsp\n\
       \x20        start the language server that ships beside this binary (--stdio)\n";

/// Start the language server that belongs to this `zio`.
///
/// The language entry point is one binary a user installs, so the
/// language server is reached through it: `zio lsp` runs the `zio-lsp`
/// installed next to it, with the arguments passed through. Editors may
/// still start `zio-lsp` directly — both paths run the same server.
pub fn run_lsp(argv: &[String]) -> i32 {
    if argv.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{LSP_USAGE}");
        return 0;
    }
    let current = std::env::current_exe()
        .map_err(|error| EvalError::custom(format!("lsp: cannot locate this binary: {error}")))
        .and_then(|exe| {
            let dir = exe.parent().ok_or_else(|| {
                EvalError::custom("lsp: this binary has no directory to look beside")
            })?;
            let suffix = if cfg!(windows) { ".exe" } else { "" };
            let sibling = dir.join(format!("zio-lsp{suffix}"));
            if sibling.is_file() {
                Ok(sibling)
            } else {
                // Without a sibling, let the platform resolve it: an
                // install that puts both binaries on PATH still works.
                Ok(std::path::PathBuf::from(format!("zio-lsp{suffix}")))
            }
        });
    let server = match current {
        Ok(server) => server,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    match std::process::Command::new(&server).args(argv).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "Error: cannot start the language server at {}: {error}",
                server.display()
            );
            1
        }
    }
}
