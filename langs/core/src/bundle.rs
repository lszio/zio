//! Bundle payload format shared by every execution surface: the native
//! app footer and the browser's embedded payload carry the same JSON
//! record. Only code travels — source text, portable module JSON, image
//! JSON. Files the program reaches live inside the payload; nothing
//! outside the bundle is reachable, and no authority is stored.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::bootstrap::{ModuleRoots, eval_source, language_context_with_io};
use crate::error::EvalError;
use crate::io::IoHost;

pub const MAGIC: &[u8; 8] = b"ZIOBNDL1";
pub const FOOTER_VERSION: u32 = 1;
/// magic(8) + version(4) + payload_len(8) + payload_offset(8) + checksum(8)
pub const FOOTER_LEN: usize = 36;

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
    /// Declared modules, name -> source text.
    pub modules: BTreeMap<String, String>,
}

impl Bundle {
    pub fn encode(&self) -> Result<String, EvalError> {
        serde_json::to_string(self)
            .map_err(|error| EvalError::custom(format!("bundle encode: {error}")))
    }

    pub fn decode(payload: &str) -> Result<Self, EvalError> {
        let bundle: Bundle = serde_json::from_str(payload)
            .map_err(|error| EvalError::custom(format!("bundle decode: {error}")))?;
        if bundle.format != "zio-bundle" || bundle.version != 1 {
            return Err(EvalError::custom(format!(
                "bundle {:?} v{} is not supported",
                bundle.format, bundle.version
            )));
        }
        Ok(bundle)
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// runtime bytes + payload text, with a checksummed footer.
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

/// Run a bundle payload against `io` as the whole authority the program
/// has: stdout/console and stdin pass through to it, while every file
/// lives inside the payload. Declared modules load first, in manifest
/// order; a `require` of anything else cannot reach outward.
pub fn run_payload(io: Arc<dyn IoHost>, payload: &str) -> Result<String, EvalError> {
    let bundle = Bundle::decode(payload)?;
    let host: Arc<dyn IoHost> = Arc::new(BundleIoHost {
        base: io,
        files: Mutex::new(bundle.modules.clone()),
    });
    let ctx = language_context_with_io(ModuleRoots::empty(), host)?;
    for (name, text) in &bundle.modules {
        if ctx.modules.borrow().is_loaded(std::slice::from_ref(name)) {
            continue;
        }
        eval_source(&ctx, &format!("{name}.zio"), text)?;
    }
    let value = match &bundle.entry {
        EntrySpec::Source { name, text } => eval_source(&ctx, name, text)?,
        EntrySpec::Module { name, json } => {
            let module = crate::bytecode::deserialize_module(json)?;
            crate::bytecode::execute_module(&ctx, &module)
                .map_err(|error| EvalError::custom(format!("cannot execute {name}: {error}")))?
        }
        EntrySpec::Image { json, entry, .. } => {
            let image = crate::image::wire::Image::parse(json)?;
            let roots = crate::image::restore::restore_image(
                &ctx,
                &image,
                &crate::image::ImageLimits::default(),
            )?;
            for (index, root) in roots.iter().enumerate() {
                let key = image
                    .root_names
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| format!("root-{index}"));
                ctx.env.set(key, root.clone());
            }
            eval_source(&ctx, "bundle-entry", entry)?
        }
    };
    Ok(value.to_string())
}

/// Files live inside the bundle; everything else is the base host.
pub struct BundleIoHost {
    base: Arc<dyn IoHost>,
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
        self.base.print(msg)
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        self.base.println(msg)
    }

    fn read_line(&self) -> Result<String, EvalError> {
        self.base.read_line()
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        self.base.current_dir()
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

/// Base64 for embedding a payload in a single HTML file; the browser
/// decodes it with `atob` and a UTF-8 decoder.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let packed = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(packed >> 18) as usize & 63] as char);
        out.push(ALPHABET[(packed >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(packed >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[packed as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The default single-file browser app: the wasm-bindgen glue by URL,
/// the payload inline. Same contract as the native footer — decode the
/// payload, validate nothing else is expected, run it.
pub fn browser_html(payload: &str, glue_url: &str, title: &str) -> String {
    let encoded = base64_encode(payload.as_bytes());
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n</head>\n<body>\n<pre id=\"out\"></pre>\n<script type=\"module\">\nimport init, {{ ZioSession }} from '{glue_url}';\nconst PAYLOAD_B64 = '{encoded}';\nconst payload = new TextDecoder().decode(Uint8Array.from(atob(PAYLOAD_B64), c => c.charCodeAt(0)));\nawait init();\nconst session = new ZioSession();\ntry {{\n  const result = session.run_bundle(payload);\n  document.getElementById('out').textContent = result;\n}} catch (error) {{\n  document.getElementById('out').textContent = 'Error: ' + error;\n}}\n</script>\n</body>\n</html>\n"
    )
}
