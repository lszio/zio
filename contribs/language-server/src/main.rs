mod analysis;
use analysis::{Analysis, MAX_SOURCE, Target, offset, position, range};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
const MAX_FRAME: usize = 4 * 1024 * 1024;
const MAX_MEMORY: usize = 16 * 1024 * 1024;
const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "function",
    "variable",
    "keyword",
    "string",
    "number",
    "comment",
    "operator",
];
const BUILTINS: &[&str] = &[
    "def",
    "defn",
    "fn",
    "defmacro",
    "quote",
    "if",
    "do",
    "let",
    "let*",
    "loop",
    "recur",
    "and",
    "or",
    "cond",
    "set!",
    "module",
    "export",
    "require",
    "try",
    "catch",
    "finally",
    "throw",
    "defclass",
    "defgeneric",
    "defmethod",
    "new",
    "nil",
    "true",
    "false",
    "+",
    "-",
    "*",
    "/",
    "=",
    "<",
    ">",
    "map",
    "reduce",
    "filter",
    "first",
    "rest",
    "cons",
    "list",
    "vector",
    "get",
    "assoc",
    "count",
    "str",
    "println",
];
struct Document {
    source: String,
    version: i64,
}
struct Server {
    documents: BTreeMap<String, Document>,
    sources: BTreeMap<String, String>,
    analyses: BTreeMap<String, Analysis>,
    roots: Vec<PathBuf>,
    initialized: bool,
    shutdown: bool,
}
fn uri_path(uri: &str) -> Option<PathBuf> {
    let text = uri.strip_prefix("file://")?;
    let text = if let Some(path) = text.strip_prefix("localhost/") {
        format!("/{path}")
    } else {
        text.to_string()
    };
    if !text.starts_with('/') || text.contains(['?', '#']) {
        return None;
    }
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b'%' {
            bytes.push(u8::from_str_radix(text.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(text.as_bytes()[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(bytes).ok()?;
    if s.contains('\0') {
        return None;
    }
    Some(PathBuf::from(s))
}
fn path_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(b) {
            uri.push(*b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}
fn frame(r: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut size = None;
    let mut total = 0;
    loop {
        let mut bytes = Vec::new();
        let count = r.take(8193).read_until(b'\n', &mut bytes)?;
        if count == 0 {
            return if total == 0 {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete LSP header",
                ))
            };
        }
        total += count;
        if total > 8192 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "LSP header exceeds 8 KiB",
            ));
        }
        let line = std::str::from_utf8(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                if size.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "duplicate Content-Length",
                    ));
                }
                size = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                );
            }
        }
    }
    let length =
        size.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length"))?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "LSP frame exceeds 4 MiB",
        ));
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
fn send(w: &mut impl Write, value: Value) -> io::Result<()> {
    let body = serde_json::to_vec(&value)?;
    write!(w, "Content-Length: {}\r\n\r\n", body.len())?;
    w.write_all(&body)?;
    w.flush()
}
impl Server {
    fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
            sources: BTreeMap::new(),
            analyses: BTreeMap::new(),
            roots: Vec::new(),
            initialized: false,
            shutdown: false,
        }
    }
    fn refresh(&mut self) {
        self.sources = self
            .documents
            .iter()
            .map(|(uri, doc)| (uri.clone(), doc.source.clone()))
            .collect();
        self.analyses = self
            .sources
            .iter()
            .map(|(uri, source)| (uri.clone(), Analysis::new(uri, source)))
            .collect();
        let mut pending: Vec<String> = self
            .analyses
            .values()
            .flat_map(|a| a.imports.iter().map(|i| i.module.clone()))
            .collect();
        let mut visited = std::collections::BTreeSet::new();
        let mut memory: usize = self.sources.values().map(String::len).sum();
        while let Some(module) = pending.pop() {
            if !visited.insert(module.clone()) || visited.len() > 128 {
                continue;
            }
            if self
                .analyses
                .values()
                .any(|a| a.modules.contains_key(&module))
            {
                continue;
            }
            if module.split('.').any(|part| {
                part.is_empty()
                    || !part
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            }) {
                continue;
            }
            let filename = format!("{}.zio", module.replace('.', "/"));
            for root in &self.roots {
                let Ok(path) = root.join(&filename).canonicalize() else {
                    continue;
                };
                if !path.starts_with(root) {
                    continue;
                }
                let uri = path_uri(&path);
                if self.analyses.contains_key(&uri) {
                    break;
                }
                let Ok(file) = std::fs::File::open(&path) else {
                    continue;
                };
                if !file
                    .metadata()
                    .is_ok_and(|m| m.is_file() && m.len() <= MAX_SOURCE as u64)
                {
                    continue;
                }
                let mut source = String::new();
                if file
                    .take((MAX_SOURCE + 1) as u64)
                    .read_to_string(&mut source)
                    .is_err()
                    || source.len() > MAX_SOURCE
                    || memory + source.len() > MAX_MEMORY
                {
                    continue;
                }
                memory += source.len();
                let analysis = Analysis::new(&uri, &source);
                pending.extend(analysis.imports.iter().map(|i| i.module.clone()));
                self.sources.insert(uri.clone(), source);
                self.analyses.insert(uri, analysis);
                break;
            }
        }
        let mut modules = BTreeMap::new();
        for a in self.analyses.values() {
            modules.extend(a.modules.clone());
        }
        // Plain required files export only explicit owned names, never all observed bindings.
        for (uri, a) in &self.analyses {
            if let Some(path) = uri_path(uri) {
                for root in &self.roots {
                    if let Ok(relative) = path.strip_prefix(root) {
                        if relative.extension().is_some_and(|x| x == "zio") {
                            let name = relative
                                .with_extension("")
                                .to_string_lossy()
                                .replace('/', ".");
                            modules.entry(name).or_insert_with(|| a.exports.clone());
                        }
                    }
                }
            }
        }
        for a in self.analyses.values_mut() {
            let imports = a.imports.clone();
            for import in imports {
                if let Some(exports) = modules.get(&import.module) {
                    a.import(&import, exports);
                }
            }
            a.resolve();
        }
    }
    fn diagnostics(&self, uri: &str, w: &mut impl Write) -> io::Result<()> {
        let doc = self.documents.get(uri);
        let values = self
            .analyses
            .get(uri)
            .map(|a| a.diagnostics.clone())
            .unwrap_or_default();
        send(
            w,
            json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"version":doc.map(|d|d.version),"diagnostics":values}}),
        )
    }
    fn location(&self, target: &Target) -> Option<Value> {
        let source = self.sources.get(&target.uri)?;
        Some(json!({"uri":target.uri,"range":range(source,target.start,target.end)}))
    }
    fn query(&self, method: &str, p: &Value) -> Result<Value, (i64, String)> {
        let uri = p["textDocument"]["uri"]
            .as_str()
            .ok_or((-32602, "missing document URI".into()))?;
        let a = self
            .analyses
            .get(uri)
            .ok_or((-32602, "document is not open or indexed".into()))?;
        let source = &self.sources[uri];
        if method == "textDocument/documentSymbol" {
            return Ok(Value::Array(a.bindings.iter().filter(|b|b.target.uri==uri&&b.visible==0&&b.detail!="parameter"&&b.detail!="condition").map(|b|json!({"name":b.name,"kind":b.kind,"detail":b.detail,"range":range(source,b.target.start,b.target.end),"selectionRange":range(source,b.target.start,b.target.end)})).collect()));
        }
        if method == "textDocument/semanticTokens/full" {
            return Ok(json!({"data":semantic(source,a)}));
        }
        let point =
            offset(source, &p["position"]).ok_or((-32602, "invalid UTF-16 position".into()))?;
        if method == "textDocument/completion" {
            let prefix = source[..point]
                .chars()
                .rev()
                .take_while(|c| !c.is_whitespace() && !"()[]{}\"';".contains(*c))
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let scope = a
                .scopes
                .iter()
                .enumerate()
                .filter(|(_, s)| s.start <= point && point <= s.end)
                .max_by_key(|(id, _)| *id)
                .map_or(0, |(id, _)| id);
            let mut scopes = Vec::new();
            let mut next = Some(scope);
            while let Some(s) = next {
                scopes.push(s);
                next = a.scopes[s].parent;
            }
            let mut candidates = BTreeMap::new();
            for name in BUILTINS {
                if name.starts_with(&prefix) {
                    candidates.insert((*name).into(), json!({"label":name,"kind":14}));
                }
            }
            for s in scopes.into_iter().rev() {
                for b in &a.bindings {
                    if b.scope == s && b.visible <= point && b.name.starts_with(&prefix) {
                        candidates.insert(b.name.clone(),json!({"label":b.name,"kind":if b.kind==12{3}else{6},"detail":b.detail}));
                    }
                }
            }
            return Ok(
                json!({"isIncomplete":false,"items":candidates.into_values().collect::<Vec<_>>()}),
            );
        }
        let target = a
            .bindings
            .iter()
            .find(|b| b.target.uri == uri && b.target.start <= point && point < b.target.end)
            .map(|b| b.target.clone())
            .or_else(|| {
                a.references
                    .iter()
                    .find(|r| r.start <= point && point < r.end)
                    .and_then(|r| r.target.clone())
            });
        match method {
            "textDocument/definition" => Ok(json!(
                target
                    .as_ref()
                    .and_then(|t| self.location(t))
                    .into_iter()
                    .collect::<Vec<_>>()
            )),
            "textDocument/references" => {
                let Some(target) = target else {
                    return Ok(json!([]));
                };
                let mut locations = Vec::new();
                if p["context"]["includeDeclaration"]
                    .as_bool()
                    .unwrap_or(false)
                {
                    if let Some(location) = self.location(&target) {
                        locations.push(location);
                    }
                }
                for (uri, a) in &self.analyses {
                    for r in &a.references {
                        if r.target.as_ref() == Some(&target) {
                            if let Some(location) = self.location(&Target {
                                uri: uri.clone(),
                                start: r.start,
                                end: r.end,
                            }) {
                                locations.push(location);
                            }
                        }
                    }
                }
                Ok(json!(locations))
            }
            "textDocument/hover" => {
                let binding = target.as_ref().and_then(|target| {
                    self.analyses
                        .values()
                        .flat_map(|a| &a.bindings)
                        .find(|b| &b.target == target)
                });
                if let Some(b) = binding {
                    return Ok(
                        json!({"contents":{"kind":"plaintext","value":format!("{}\n{}",b.name,b.detail)}}),
                    );
                }
                if let Some(t) = a
                    .tokens
                    .iter()
                    .find(|t| t.start.0 <= point && point < t.end().0)
                {
                    if BUILTINS.contains(&t.text.as_str()) {
                        return Ok(
                            json!({"contents":{"kind":"plaintext","value":format!("Zio form or primitive: {}",t.text)},"range":range(source,t.start.0,t.end().0)}),
                        );
                    }
                }
                Ok(Value::Null)
            }
            _ => Err((-32601, format!("unsupported method: {method}"))),
        }
    }
    fn dispatch(&mut self, v: Value, w: &mut impl Write) -> io::Result<Option<i32>> {
        let id = v.get("id").cloned();
        let method = v["method"].as_str().unwrap_or("");
        let p = &v["params"];
        if method == "exit" {
            return Ok(Some(if self.shutdown { 0 } else { 1 }));
        }
        let result: Result<Value, (i64, String)> = if v["jsonrpc"] != "2.0" {
            Err((-32600, "invalid JSON-RPC envelope".into()))
        } else if method == "initialize" {
            if self.initialized {
                Err((-32600, "already initialized".into()))
            } else {
                self.initialized = true;
                let mut roots: Vec<String> = p["workspaceFolders"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f["uri"].as_str().map(str::to_string))
                    .collect();
                if let Some(root) = p["rootUri"].as_str() {
                    roots.push(root.into());
                }
                for uri in roots {
                    if let Some(path) = uri_path(&uri).and_then(|p| p.canonicalize().ok()) {
                        if path.is_dir() && !self.roots.contains(&path) {
                            self.roots.push(path);
                        }
                    }
                }
                if let Some(roots) = p["initializationOptions"]["moduleRoots"].as_array() {
                    for root in roots {
                        if let Some(path) = root
                            .as_str()
                            .map(PathBuf::from)
                            .and_then(|p| p.canonicalize().ok())
                        {
                            if path.is_dir() && !self.roots.contains(&path) {
                                self.roots.push(path);
                            }
                        }
                    }
                }
                Ok(
                    json!({"serverInfo":{"name":"zio-lsp","version":env!("CARGO_PKG_VERSION")},"capabilities":{"positionEncoding":"utf-16","textDocumentSync":{"openClose":true,"change":2},"documentSymbolProvider":true,"definitionProvider":true,"referencesProvider":true,"hoverProvider":true,"completionProvider":{"triggerCharacters":["/",":"]},"semanticTokensProvider":{"legend":{"tokenTypes":TOKEN_TYPES,"tokenModifiers":["declaration"]},"full":true}}}),
                )
            }
        } else if !self.initialized {
            Err((-32002, "server not initialized".into()))
        } else if self.shutdown {
            Err((-32600, "server has shut down".into()))
        } else if method == "shutdown" {
            self.shutdown = true;
            Ok(Value::Null)
        } else if method == "initialized" || method == "$/cancelRequest" {
            Ok(Value::Null)
        } else if method == "textDocument/didOpen" || method == "textDocument/didChange" {
            let doc = &p["textDocument"];
            if let (Some(uri), Some(version)) = (doc["uri"].as_str(), doc["version"].as_i64()) {
                let prior = self.documents.get(uri);
                if prior.is_none() && method == "textDocument/didChange" {
                    Err((-32602, "change for unopened document".into()))
                } else if prior.is_some_and(|d| version <= d.version) {
                    Ok(Value::Null)
                } else {
                    let mut source = if method == "textDocument/didOpen" {
                        doc["text"].as_str().map(str::to_string)
                    } else {
                        prior.map(|d| d.source.clone())
                    };
                    if method == "textDocument/didChange" {
                        if let Some(changes) = p["contentChanges"].as_array() {
                            for change in changes {
                                let Some(text) = change["text"].as_str() else {
                                    source = None;
                                    break;
                                };
                                if let Some(range) = change.get("range") {
                                    let Some(s) = source.as_mut() else {
                                        break;
                                    };
                                    let start = offset(s, &range["start"]);
                                    let end = offset(s, &range["end"]);
                                    if let (Some(start), Some(end)) = (start, end) {
                                        if start <= end {
                                            s.replace_range(start..end, text);
                                        } else {
                                            source = None;
                                            break;
                                        }
                                    } else {
                                        source = None;
                                        break;
                                    }
                                } else {
                                    source = Some(text.into());
                                }
                                if source.as_ref().is_some_and(|s| s.len() > MAX_SOURCE) {
                                    source = None;
                                    break;
                                }
                            }
                        } else {
                            source = None;
                        }
                    }
                    if let Some(source) = source.filter(|s| s.len() <= MAX_SOURCE) {
                        let memory: usize = self
                            .documents
                            .iter()
                            .filter(|(key, _)| key.as_str() != uri)
                            .map(|(_, d)| d.source.len())
                            .sum();
                        if memory + source.len() > MAX_MEMORY
                            || (prior.is_none() && self.documents.len() >= 64)
                        {
                            Err((-32602, "document memory limit reached".into()))
                        } else {
                            self.documents
                                .insert(uri.into(), Document { source, version });
                            self.refresh();
                            self.diagnostics(uri, w)?;
                            Ok(Value::Null)
                        }
                    } else {
                        Err((-32602, "invalid or oversized document edit".into()))
                    }
                }
            } else {
                Err((-32602, "missing URI or integer document version".into()))
            }
        } else if method == "textDocument/didClose" {
            if let Some(uri) = p["textDocument"]["uri"].as_str() {
                self.documents.remove(uri);
                self.refresh();
                send(
                    w,
                    json!({"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":uri,"diagnostics":[]}}),
                )?;
                Ok(Value::Null)
            } else {
                Err((-32602, "missing URI".into()))
            }
        } else {
            self.refresh();
            self.query(method, p)
        };
        if let Some(id) = id {
            send(
                w,
                match result {
                    Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                    Err((code, message)) => {
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
                    }
                },
            )?;
        } else if let Err((_, message)) = result {
            send(
                w,
                json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"type":1,"message":message}}),
            )?;
        }
        Ok(None)
    }
}
fn semantic(source: &str, a: &Analysis) -> Vec<u32> {
    let mut spans: Vec<(usize, usize, u32, u32)> = Vec::new();
    for token in &a.tokens {
        let t = token.text.as_str();
        let declaration = a.bindings.iter().find(|b| {
            b.target.start == token.start.0
                && b.target.end == token.end().0
                && b.detail != "parameter imported"
        });
        let kind = if let Some(b) = declaration {
            if b.kind == 12 {
                2
            } else if b.kind == 5 {
                1
            } else {
                3
            }
        } else if t.starts_with('"') || t.starts_with('\\') {
            5
        } else if t.parse::<f64>().is_ok() {
            6
        } else if t.starts_with(':') || BUILTINS.contains(&t) {
            4
        } else if matches!(t, "(" | ")" | "[" | "]" | "{" | "}" | "'" | "#") {
            8
        } else {
            3
        };
        spans.push((
            token.start.0,
            token.end().0,
            kind,
            if declaration.is_some() { 1 } else { 0 },
        ));
    }
    // The reader deliberately discards comments; identify only gaps, never semicolons inside strings.
    let mut cursor = 0;
    for token in &a.tokens {
        comments(source, cursor, token.start.0, &mut spans);
        cursor = token.end().0;
    }
    comments(source, cursor, source.len(), &mut spans);
    let mut split = Vec::new();
    for (start, end, kind, modifier) in spans {
        let mut at = start;
        while at < end {
            let next = source[at..end].find('\n').map_or(end, |n| at + n);
            let stop = if next > at && source.as_bytes()[next - 1] == b'\r' {
                next - 1
            } else {
                next
            };
            if stop > at {
                split.push((at, stop, kind, modifier));
            }
            at = if next < end { next + 1 } else { end };
        }
    }
    split.sort_by_key(|s| s.0);
    let mut data = Vec::new();
    let (mut last_line, mut last_char) = (0u32, 0u32);
    for (start, end, kind, modifier) in split {
        let p = position(source, start);
        let line = p["line"].as_u64().unwrap() as u32;
        let character = p["character"].as_u64().unwrap() as u32;
        data.extend([
            line - last_line,
            if line == last_line {
                character - last_char
            } else {
                character
            },
            source[start..end].encode_utf16().count() as u32,
            kind,
            modifier,
        ]);
        last_line = line;
        last_char = character;
    }
    data
}
fn comments(source: &str, start: usize, end: usize, spans: &mut Vec<(usize, usize, u32, u32)>) {
    let mut at = start;
    while at < end {
        let Some(index) = source[at..end].find(';') else {
            break;
        };
        let begin = at + index;
        let stop = source[begin..end].find('\n').map_or(end, |n| begin + n);
        spans.push((begin, stop, 7, 0));
        at = if stop < end { stop + 1 } else { end };
    }
}
fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut server = Server::new();
    loop {
        match frame(&mut input) {
            Ok(Some(value)) => match server.dispatch(value, &mut output) {
                Ok(Some(code)) => std::process::exit(code),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("zio-lsp: {error}");
                    std::process::exit(1);
                }
            },
            Ok(None) => break,
            Err(error) => {
                eprintln!("zio-lsp: {error}");
                std::process::exit(1);
            }
        }
    }
}
