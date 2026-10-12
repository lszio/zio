//! `zio build`: produce an application from source, a portable module
//! or an image. Native apps are one executable (runtime binary + JSON
//! payload + checksummed footer); browser apps are one HTML file (glue
//! URL + inline payload) with the same payload contract, run by the
//! wasm engine. Entry parsing, the module manifest and the payload
//! format are shared — the only difference is the shell around it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bundle::{Bundle, EntrySpec, browser_html};
use zio_core::error::EvalError;

/// Startup hook: when this binary carries a bundle, run it. A plain
/// runtime returns None and the normal argument handling continues.
pub fn maybe_run_bundled() -> Option<Result<(), EvalError>> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(&exe).ok()?;
    match zio_core::bundle::read_payload(&bytes) {
        Ok(Some(payload)) => Some(
            zio_core::bundle::run_payload(Arc::new(zio_core::io::StdIoHost), &payload).map(
                |value| {
                    if !value.is_empty() && value != "nil" {
                        println!("{value}");
                    }
                },
            ),
        ),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

pub const BUILD_USAGE: &str = "Usage: zio build INPUT --target {native|browser|wasm-wasi} -o APP [--entry NAME] [--module NAME=PATH]... [--glue URL]\n";

struct BuildArgs {
    input: PathBuf,
    output: PathBuf,
    target: String,
    entry: Option<String>,
    modules: Vec<(String, PathBuf)>,
    glue: String,
}

fn parse_build(argv: &[String]) -> Result<BuildArgs, EvalError> {
    let mut input = None;
    let mut output = None;
    let mut target = None;
    let mut entry = None;
    let mut modules = Vec::new();
    let mut glue = "./zio_core.js".to_string();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "-o" => {
                index += 1;
                output =
                    Some(PathBuf::from(argv.get(index).ok_or_else(|| {
                        EvalError::invalid_form("build: -o needs a path")
                    })?));
            }
            "--target" => {
                index += 1;
                target = Some(
                    argv.get(index)
                        .ok_or_else(|| EvalError::invalid_form("build: --target needs a value"))?
                        .clone(),
                );
            }
            "--entry" => {
                index += 1;
                entry = Some(
                    argv.get(index)
                        .ok_or_else(|| EvalError::invalid_form("build: --entry needs a name"))?
                        .clone(),
                );
            }
            "--glue" => {
                index += 1;
                glue = argv
                    .get(index)
                    .ok_or_else(|| EvalError::invalid_form("build: --glue needs a URL"))?
                    .clone();
            }
            "--module" => {
                index += 1;
                let spec = argv
                    .get(index)
                    .ok_or_else(|| EvalError::invalid_form("build: --module needs NAME=PATH"))?;
                let Some((name, path)) = spec.split_once('=') else {
                    return Err(EvalError::invalid_form(format!(
                        "build: --module expects NAME=PATH, got {spec:?}"
                    )));
                };
                modules.push((name.to_string(), PathBuf::from(path)));
            }
            other if input.is_none() => input = Some(PathBuf::from(other)),
            other => {
                return Err(EvalError::invalid_form(format!(
                    "build: unexpected argument {other:?}"
                )));
            }
        }
        index += 1;
    }
    Ok(BuildArgs {
        input: input.ok_or_else(|| EvalError::invalid_form("build: INPUT is required"))?,
        output: output.ok_or_else(|| EvalError::invalid_form("build: -o APP is required"))?,
        target: target.ok_or_else(|| EvalError::invalid_form("build: --target is required"))?,
        entry,
        modules,
        glue,
    })
}

/// Module names a source program requires and defines inline, read
/// through the real reader so the manifest check matches the language,
/// not a text scan. A require satisfied by an inline `(module :name …)`
/// form needs no bundle entry.
fn module_names(source: &str, name: &str) -> Result<(Vec<String>, BTreeSet<String>), EvalError> {
    let forms = zio_core::reader::reader::read_program(source)
        .map_err(|error| EvalError::custom(format!("cannot read {name}: {error}")))?;
    let mut required = BTreeSet::new();
    let mut inline = BTreeSet::new();
    for form in &forms {
        let zio_core::sexp::Sexp::List(items, _) = form else {
            continue;
        };
        let Some(zio_core::sexp::Sexp::Symbol(head, _)) = items.iter().next() else {
            continue;
        };
        let module = match items.iter().nth(1) {
            Some(zio_core::sexp::Sexp::Keyword(module, _)) => module,
            Some(zio_core::sexp::Sexp::Symbol(module, _)) => module,
            _ => continue,
        };
        match head.as_str() {
            "require" => {
                required.insert(module.clone());
            }
            "module" => {
                inline.insert(module.clone());
            }
            _ => {}
        }
    }
    Ok((required.into_iter().collect(), inline))
}

/// Scan every bundled module source so a missing dependency refuses the
/// build rather than the first run.
fn declared_closure(modules: &BTreeMap<String, String>) -> Result<(), EvalError> {
    for (name, text) in modules {
        let (required, inline) = module_names(text, &format!("module {name}"))?;
        for required in required {
            if !modules.contains_key(&required) && !inline.contains(&required) {
                return Err(EvalError::invalid_form(format!(
                    "module {name:?} requires :{required}, which the bundle does not declare; add --module {required}=PATH"
                )));
            }
        }
    }
    Ok(())
}

pub fn run_build(argv: &[String]) -> Result<(), EvalError> {
    let args = parse_build(argv)?;
    if args.target != "native" && args.target != "browser" && args.target != "wasm-wasi" {
        return Err(EvalError::invalid_form(format!(
            "build: unsupported target {:?}; expected native, browser or wasm-wasi",
            args.target
        )));
    }
    let input_name = args
        .input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let input_text = || {
        std::fs::read_to_string(&args.input).map_err(|error| {
            EvalError::custom(format!("cannot read {}: {error}", args.input.display()))
        })
    };

    let mut modules = BTreeMap::new();
    for (name, path) in &args.modules {
        let text = std::fs::read_to_string(path).map_err(|error| {
            EvalError::custom(format!("cannot read {}: {error}", path.display()))
        })?;
        modules.insert(name.clone(), text);
    }

    let entry = match args.input.extension().and_then(|e| e.to_str()) {
        Some("zio") => {
            let text = input_text()?;
            let (required, inline) = module_names(&text, &input_name)?;
            for required in required {
                if !modules.contains_key(&required) && !inline.contains(&required) {
                    return Err(EvalError::invalid_form(format!(
                        "{input_name:?} requires :{required}, which the bundle does not declare; add --module {required}=PATH"
                    )));
                }
            }
            EntrySpec::Source {
                name: input_name.clone(),
                text,
            }
        }
        Some("zioc") => {
            let text = input_text()?;
            // Prove the module is portable here, not at first run.
            zio_core::bytecode::deserialize_module(&text)?;
            EntrySpec::Module {
                name: input_name.clone(),
                json: text,
            }
        }
        Some("zio-image") => {
            let text = input_text()?;
            let image = zio_core::image::wire::Image::parse(&text)?;
            let entry = args.entry.clone().ok_or_else(|| {
                EvalError::invalid_form("build: an image entry needs --entry EXPRESSION")
            })?;
            let _ = image;
            EntrySpec::Image {
                name: input_name.clone(),
                json: text,
                entry,
            }
        }
        other => {
            return Err(EvalError::invalid_form(format!(
                "build: unsupported input extension {other:?}; expected .zio, .zioc or .zio-image"
            )));
        }
    };
    declared_closure(&modules)?;

    let payload = Bundle {
        format: "zio-bundle".to_string(),
        version: 1,
        entry,
        modules,
    }
    .encode()?;

    match args.target.as_str() {
        // The runtime template is this very binary: no rustc, no checkout
        // paths, nothing to rebuild per application.
        "native" => {
            let exe = std::env::current_exe().map_err(|error| {
                EvalError::custom(format!("cannot locate the runtime: {error}"))
            })?;
            let runtime = std::fs::read(&exe)
                .map_err(|error| EvalError::custom(format!("cannot read the runtime: {error}")))?;
            let base = if zio_core::bundle::read_payload(&runtime)?.is_some() {
                // Building from a bundled app would nest payloads; the
                // runtime base is this binary minus its own payload.
                runtime[..runtime.len() - zio_core::bundle::FOOTER_LEN].to_vec()
            } else {
                runtime
            };
            write_native_app(&base, &payload, &args.output)
        }
        "browser" => {
            let html = browser_html(&payload, &args.glue, &input_name);
            std::fs::write(&args.output, html.as_bytes()).map_err(|error| {
                EvalError::custom(format!("cannot write {}: {error}", args.output.display()))
            })?;
            println!(
                "built {} ({} bytes, payload {})",
                args.output.display(),
                html.len(),
                payload.len()
            );
            Ok(())
        }
        // WASI cannot read its own module, so the payload is compiled
        // into the runtime per application: one .wasm, no sidecar.
        "wasm-wasi" => write_wasi_app(&payload, &input_name, &args.output),
        _ => unreachable!("target was validated above"),
    }
}

/// Browser-only dependencies never make it into a WASI application:
/// they exist only in the engine's `wasm` feature.
fn refuse_browser_only(kind: &str, name: &str, text: &str, path: &str) -> Result<(), EvalError> {
    use zio_core::sexp::Sexp;
    let forms = zio_core::reader::reader::read_program(text)
        .map_err(|error| EvalError::custom(format!("cannot read {path}: {error}")))?;
    fn browser_symbol(head: &str) -> bool {
        matches!(
            head,
            "js/eval" | "js/global" | "js/prop" | "js/require" | "globalThis"
        )
    }
    fn scan(form: &Sexp, path: &str, kind: &str, name: &str) -> Result<(), EvalError> {
        match form {
            Sexp::List(items, _) => {
                if let Some(Sexp::Symbol(head, _)) = items.iter().next()
                    && browser_symbol(&head)
                {
                    return Err(EvalError::invalid_form(format!(
                        "{path} uses browser-only {head:?} ({kind} {name}); a WASI application cannot"
                    )));
                }
                for item in items.iter() {
                    scan(item, path, kind, name)?;
                }
                Ok(())
            }
            Sexp::Vector(items, _) => {
                for item in items.iter() {
                    scan(item, path, kind, name)?;
                }
                Ok(())
            }
            Sexp::Map(entries, _) => {
                for (key, value) in entries.iter() {
                    scan(key, path, kind, name)?;
                    scan(value, path, kind, name)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    for form in &forms {
        scan(form, path, kind, name)?;
    }
    Ok(())
}

fn write_wasi_app(payload: &str, input_name: &str, output: &Path) -> Result<(), EvalError> {
    let bundle = Bundle::decode(payload)?;
    if let zio_core::bundle::EntrySpec::Source { text, .. } = &bundle.entry {
        refuse_browser_only("entry", input_name, text, input_name)?;
    }
    for (name, text) in &bundle.modules {
        refuse_browser_only("module", name, text, &format!("module {name}"))?;
    }
    let exe = std::env::current_exe()
        .map_err(|error| EvalError::custom(format!("cannot locate the runtime: {error}")))?;
    // target/debug/zio-cli -> the workspace root, without a checkout
    // path baked into the command.
    let root = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| EvalError::custom("cannot locate the workspace root"))?
        .to_path_buf();
    let payload_file =
        std::env::temp_dir().join(format!("zio-wasi-payload-{}.json", std::process::id()));
    std::fs::write(&payload_file, payload.as_bytes())
        .map_err(|error| EvalError::custom(format!("cannot stage the payload: {error}")))?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = std::process::Command::new(cargo)
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "-p",
            "zio-wasi",
            "--target",
            "wasm32-wasip1",
        ])
        .env("ZIO_WASI_PAYLOAD", &payload_file)
        .status()
        .map_err(|error| EvalError::custom(format!("cannot build the WASI runtime: {error}")))?;
    let _ = std::fs::remove_file(&payload_file);
    if !status.success() {
        return Err(EvalError::custom(
            "the WASI runtime build failed; run tools/build-wasi.sh to check the target",
        ));
    }
    let built = root.join("target/wasm32-wasip1/release/zio-wasi.wasm");
    let bytes = std::fs::read(&built)
        .map_err(|error| EvalError::custom(format!("cannot read {}: {error}", built.display())))?;
    std::fs::write(output, &bytes).map_err(|error| {
        EvalError::custom(format!("cannot write {}: {error}", output.display()))
    })?;
    println!(
        "built {} ({} bytes, payload {})",
        output.display(),
        bytes.len(),
        payload.len()
    );
    Ok(())
}

fn write_native_app(runtime: &[u8], payload: &str, output: &Path) -> Result<(), EvalError> {
    let bytes = zio_core::bundle::append_payload(runtime, payload);
    std::fs::write(output, &bytes).map_err(|error| {
        EvalError::custom(format!("cannot write {}: {error}", output.display()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(output)
            .map_err(|error| {
                EvalError::custom(format!("cannot stat {}: {error}", output.display()))
            })?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(output, permissions).map_err(|error| {
            EvalError::custom(format!("cannot chmod {}: {error}", output.display()))
        })?;
    }
    println!(
        "built {} ({} bytes, payload {})",
        output.display(),
        bytes.len(),
        payload.len()
    );
    Ok(())
}
