//! Bundled applications: one executable = the runtime binary plus a JSON
//! payload with a checksum footer. `zio build` appends the payload to a
//! copy of the runtime; at startup the runtime detects the footer and
//! runs the bundle instead of a script.
//!
//! The payload carries code only — source text, portable module JSON,
//! image JSON. Resources resolve against the bundle's in-memory file
//! host, so an app depends on nothing outside itself. A footer that
//! fails magic, version, bounds or checksum refuses the run with a
//! named error; there is no second "bundled runtime" path — it is the
//! same binary.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use zio_core::bootstrap::{ModuleRoots, eval_source, language_context_with_io};
use zio_core::error::EvalError;
use zio_core::io::IoHost;
use zio_core::value::Value;

const MAGIC: &[u8; 8] = b"ZIOBNDL1";
const FOOTER_VERSION: u32 = 1;
/// magic(8) + version(4) + payload_len(8) + payload_offset(8) + checksum(8)
const FOOTER_LEN: usize = 36;

pub const BUILD_USAGE: &str =
    "Usage: zio build INPUT --target native -o APP [--entry NAME] [--module NAME=PATH]...\n";

/// What the bundle runs: top-level source, a portable module, or a
/// restored image entered explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EntrySpec {
    Source {
        name: String,
        text: String,
    },
    Module {
        name: String,
        json: String,
    },
    Image {
        name: String,
        json: String,
        entry: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bundle {
    pub format: String,
    pub version: u32,
    pub entry: EntrySpec,
    /// Declared modules in dependency order (the order the build
    /// context loaded them), name -> source text.
    pub modules: BTreeMap<String, String>,
}

// ── Footer ───────────────────────────────────────────────────────

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// runtime bytes + payload text, with a bounded footer.
pub fn append_payload(runtime: &[u8], payload: &str) -> Vec<u8> {
    let mut out = runtime.to_vec();
    let offset = out.len() as u64;
    let bytes = payload.as_bytes();
    out.extend_from_slice(bytes);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FOOTER_VERSION.to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&fnv1a(bytes).to_le_bytes());
    out
}

/// The bundle payload of `exe`, or None when this is a plain runtime.
pub fn read_payload(exe: &[u8]) -> Result<Option<String>, EvalError> {
    if exe.len() < FOOTER_LEN || &exe[exe.len() - FOOTER_LEN..exe.len() - FOOTER_LEN + 8] != MAGIC {
        return Ok(None);
    }
    let tail = &exe[exe.len() - FOOTER_LEN..];
    let version = u32::from_le_bytes(tail[8..12].try_into().expect("4 bytes"));
    if version != FOOTER_VERSION {
        return Err(EvalError::custom(format!(
            "bundle footer version {version} is not supported (expected {FOOTER_VERSION})"
        )));
    }
    let len = u64::from_le_bytes(tail[12..20].try_into().expect("8 bytes"));
    let offset = u64::from_le_bytes(tail[20..28].try_into().expect("8 bytes"));
    let checksum = u64::from_le_bytes(tail[28..36].try_into().expect("8 bytes"));
    let limit = (exe.len() - FOOTER_LEN) as u64;
    if offset + len > limit {
        return Err(EvalError::custom(format!(
            "bundle payload is out of bounds: offset {offset} + length {len} exceeds {limit}"
        )));
    }
    let payload = &exe[offset as usize..(offset + len) as usize];
    if fnv1a(payload) != checksum {
        return Err(EvalError::custom(
            "bundle payload checksum mismatch: the file is corrupt",
        ));
    }
    String::from_utf8(payload.to_vec())
        .map(Some)
        .map_err(|error| EvalError::custom(format!("bundle payload is not UTF-8: {error}")))
}

// ── Build ────────────────────────────────────────────────────────

struct BuildArgs {
    input: PathBuf,
    output: PathBuf,
    target: String,
    entry: Option<String>,
    modules: Vec<(String, PathBuf)>,
}

fn parse_build(argv: &[String]) -> Result<BuildArgs, EvalError> {
    let mut input = None;
    let mut output = None;
    let mut target = None;
    let mut entry = None;
    let mut modules = Vec::new();
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
    if args.target != "native" {
        return Err(EvalError::invalid_form(format!(
            "build: unsupported target {:?}; only native is available",
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
                name: input_name,
                text,
            }
        }
        Some("zioc") => {
            let text = input_text()?;
            // Prove the module is portable here, not at first run.
            zio_core::bytecode::deserialize_module(&text)?;
            EntrySpec::Module {
                name: input_name,
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
                name: input_name,
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

    let bundle = Bundle {
        format: "zio-bundle".to_string(),
        version: 1,
        entry,
        modules,
    };
    let payload = serde_json::to_string(&bundle)
        .map_err(|error| EvalError::custom(format!("bundle encode: {error}")))?;

    // The runtime template is this very binary: no rustc, no checkout
    // paths, nothing to rebuild per application.
    let exe = std::env::current_exe()
        .map_err(|error| EvalError::custom(format!("cannot locate the runtime: {error}")))?;
    let runtime = std::fs::read(&exe)
        .map_err(|error| EvalError::custom(format!("cannot read the runtime: {error}")))?;
    if read_payload(&runtime)?.is_some() {
        // Building from a bundled app would nest payloads; the runtime
        // base is this binary minus its own payload.
        let base = strip_payload(&runtime);
        return write_app(&base, &payload, &args.output);
    }
    write_app(&runtime, &payload, &args.output)
}

fn strip_payload(runtime: &[u8]) -> Vec<u8> {
    // The footer is the last 40 bytes; the payload sits before it.
    runtime[..runtime.len() - FOOTER_LEN].to_vec()
}

fn write_app(runtime: &[u8], payload: &str, output: &Path) -> Result<(), EvalError> {
    let bytes = append_payload(runtime, payload);
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

// ── Run ──────────────────────────────────────────────────────────

/// Startup hook: when this binary carries a bundle, run it. A plain
/// runtime returns None and the normal argument handling continues.
pub fn maybe_run_bundled() -> Option<Result<(), EvalError>> {
    let exe = std::env::current_exe().ok()?;
    let bytes = std::fs::read(&exe).ok()?;
    match read_payload(&bytes) {
        Ok(Some(payload)) => Some(run_bundle(&payload)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}

fn run_bundle(payload: &str) -> Result<(), EvalError> {
    let bundle: Bundle = serde_json::from_str(payload)
        .map_err(|error| EvalError::custom(format!("bundle decode: {error}")))?;
    if bundle.format != "zio-bundle" || bundle.version != 1 {
        return Err(EvalError::custom(format!(
            "bundle {:?} v{} is not supported",
            bundle.format, bundle.version
        )));
    }
    let io: Arc<dyn IoHost> = Arc::new(BundleIoHost {
        inner: zio_core::io::StdIoHost,
        files: Mutex::new(bundle.modules.clone()),
    });
    // Empty module roots: nothing outside the bundle is reachable.
    let ctx = language_context_with_io(ModuleRoots::empty(), io)?;
    // Declared modules load in manifest order; a require of anything
    // else fails instead of reaching for a checkout.
    for (name, text) in &bundle.modules {
        if ctx.modules.borrow().is_loaded(std::slice::from_ref(name)) {
            continue;
        }
        eval_source(&ctx, &format!("{name}.zio"), text)?;
    }
    let value = match &bundle.entry {
        EntrySpec::Source { name, text } => eval_source(&ctx, name, text)?,
        EntrySpec::Module { name, json } => {
            let module = zio_core::bytecode::deserialize_module(json)?;
            zio_core::bytecode::execute_module(&ctx, &module)
                .map_err(|error| EvalError::custom(format!("cannot execute {name}: {error}")))?
        }
        EntrySpec::Image { name, json, entry } => {
            let image = zio_core::image::wire::Image::parse(json)?;
            let roots = zio_core::image::restore::restore_image(
                &ctx,
                &image,
                &zio_core::image::ImageLimits::default(),
            )?;
            for (index, root) in roots.iter().enumerate() {
                let key = image
                    .root_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("root-{index}"));
                ctx.env.set(key, root.clone());
            }
            let _ = name;
            eval_source(&ctx, "bundle-entry", entry)?
        }
    };
    if value != Value::Nil {
        println!("{value}");
    }
    Ok(())
}

/// stdout passes through; files live inside the bundle. A path outside
/// the declared modules simply does not exist here.
struct BundleIoHost {
    inner: zio_core::io::StdIoHost,
    files: Mutex<BTreeMap<String, String>>,
}

impl BundleIoHost {
    fn read(&self, path: &str) -> Result<String, EvalError> {
        self.files
            .lock()
            .map_err(|_| EvalError::custom("bundle file map is poisoned"))?
            .get(path)
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("{path} is not in the bundle")))
    }
}

impl IoHost for BundleIoHost {
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
        self.read(path)
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        self.files
            .lock()
            .map_err(|_| EvalError::custom("bundle file map is poisoned"))?
            .insert(path.to_string(), data.to_string());
        Ok(())
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        Ok(self
            .files
            .lock()
            .map_err(|_| EvalError::custom("bundle file map is poisoned"))?
            .contains_key(path))
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        self.read(path)?;
        Ok(path.to_string())
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        Ok(path == "." || path.is_empty())
    }
}
