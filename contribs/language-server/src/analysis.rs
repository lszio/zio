use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use zio_core::reader::lexer::{Token, tokenize};
use zio_core::span::SourceMap;

pub const MAX_SOURCE: usize = 1024 * 1024;
const MAX_TOKENS: usize = 100_000;
const MAX_DEPTH: usize = 128;
#[derive(Clone, Debug)]
pub struct Node {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub children: Vec<Node>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub uri: String,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone)]
pub struct Binding {
    pub name: String,
    pub target: Target,
    pub scope: usize,
    pub visible: usize,
    pub kind: u32,
    pub detail: String,
}
#[derive(Clone)]
pub struct Reference {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub scope: usize,
    pub target: Option<Target>,
}
#[derive(Clone)]
pub struct Scope {
    pub parent: Option<usize>,
    pub start: usize,
    pub end: usize,
}
#[derive(Clone)]
pub struct Import {
    pub module: String,
    pub alias: Option<String>,
    pub names: Option<Vec<String>>,
    pub scope: usize,
    pub start: usize,
}
pub struct Analysis {
    pub bindings: Vec<Binding>,
    pub references: Vec<Reference>,
    pub scopes: Vec<Scope>,
    pub exports: BTreeMap<String, Target>,
    pub modules: BTreeMap<String, BTreeMap<String, Target>>,
    pub imports: Vec<Import>,
    pub diagnostics: Vec<Value>,
    pub tokens: Vec<Token>,
}
pub fn position(source: &str, offset: usize) -> Value {
    let offset = offset.min(source.len());
    let mut end = offset;
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &source[..end];
    let line = prefix.bytes().filter(|b| *b == b'\n').count();
    let start = prefix.rfind('\n').map_or(0, |n| n + 1);
    json!({"line":line,"character":source[start..end].encode_utf16().count()})
}
pub fn offset(source: &str, pos: &Value) -> Option<usize> {
    let line = pos["line"].as_u64()? as usize;
    let character = pos["character"].as_u64()? as usize;
    let start = if line == 0 {
        0
    } else {
        source.match_indices('\n').nth(line - 1)?.0 + 1
    };
    let mut units = 0;
    for (index, c) in source[start..].char_indices() {
        if units == character {
            return Some(start + index);
        }
        if c == '\n' || c == '\r' {
            return None;
        }
        units += c.len_utf16();
        if units > character {
            return None;
        }
    }
    (units == character).then_some(source.len())
}
pub fn range(source: &str, start: usize, end: usize) -> Value {
    json!({"start":position(source,start),"end":position(source,end)})
}
fn diagnostic(source: &str, start: usize, end: usize, message: String) -> Value {
    json!({"range":range(source,start,end),"severity":1,"source":"zio","message":message})
}
fn tree(tokens: &[Token], index: &mut usize, depth: usize) -> Option<Node> {
    if depth > MAX_DEPTH {
        return None;
    }
    let token = tokens.get(*index)?;
    *index += 1;
    let mut node = Node {
        text: token.text.clone(),
        start: token.start.0,
        end: token.end().0,
        children: Vec::new(),
    };
    if node.text == "'" || node.text == "#" {
        if let Some(child) = tree(tokens, index, depth + 1) {
            node.end = child.end;
            node.children.push(child);
        }
    } else if matches!(node.text.as_str(), "(" | "[" | "{") {
        let close = match node.text.as_str() {
            "(" => ")",
            "[" => "]",
            _ => "}",
        };
        while let Some(next) = tokens.get(*index) {
            if next.text == close {
                node.end = next.end().0;
                *index += 1;
                break;
            }
            if matches!(next.text.as_str(), ")" | "]" | "}") {
                break;
            }
            let before = *index;
            if let Some(child) = tree(tokens, index, depth + 1) {
                node.end = child.end;
                node.children.push(child);
            }
            if *index == before {
                break;
            }
        }
    }
    Some(node)
}
fn symbol(n: &Node) -> bool {
    n.children.is_empty()
        && !n.text.is_empty()
        && !matches!(
            n.text.as_str(),
            "nil" | "true" | "false" | "&" | "." | "(" | ")" | "[" | "]" | "{" | "}" | "'" | "#"
        )
        && !n.text.starts_with([':', '"', '\\'])
        && n.text.parse::<f64>().is_err()
}
fn name(n: &Node) -> String {
    n.text.trim_start_matches(':').to_string()
}
impl Analysis {
    pub fn new(uri: &str, source: &str) -> Self {
        let mut a = Self {
            bindings: Vec::new(),
            references: Vec::new(),
            scopes: vec![Scope {
                parent: None,
                start: 0,
                end: source.len(),
            }],
            exports: BTreeMap::new(),
            modules: BTreeMap::new(),
            imports: Vec::new(),
            diagnostics: Vec::new(),
            tokens: Vec::new(),
        };
        if source.len() > MAX_SOURCE {
            a.diagnostics.push(diagnostic(
                source,
                0,
                0,
                "Document exceeds 1 MiB analysis limit".into(),
            ));
            return a;
        }
        a.tokens = tokenize(source);
        let mut nesting = 0usize;
        let mut too_deep = false;
        let mut quotes = 0usize;
        for token in &a.tokens {
            if matches!(token.text.as_str(), "(" | "[" | "{") {
                nesting += 1;
            } else if matches!(token.text.as_str(), ")" | "]" | "}") {
                nesting = nesting.saturating_sub(1);
            }
            if token.text == "'" {
                quotes += 1;
            } else {
                quotes = 0;
            }
            if nesting + quotes > MAX_DEPTH {
                too_deep = true;
                break;
            }
        }
        if a.tokens.len() > MAX_TOKENS || too_deep {
            a.diagnostics.push(diagnostic(
                source,
                0,
                0,
                "Source exceeds token or nesting analysis limit".into(),
            ));
            return a;
        }
        // A fresh map per revision: no append-only source registry or evaluator exists in this service.
        if let Err(error) = zio_core::syntax::inspect_source(&SourceMap::new(), uri, source) {
            let (start, end) = error
                .span()
                .map_or((source.len(), source.len()), |s| (s.start.0, s.end.0));
            a.diagnostics
                .push(diagnostic(source, start, end, error.to_string()));
        }
        let mut index = 0;
        let mut nodes = Vec::new();
        while index < a.tokens.len() {
            let before = index;
            if let Some(n) = tree(&a.tokens, &mut index, 0) {
                nodes.push(n);
            }
            if index == before {
                break;
            }
        }
        a.predeclare(uri, &nodes, 0);
        for n in &nodes {
            a.walk(uri, n, 0);
        }
        let names = export_names(&nodes);
        a.exports = a.exported(0, &names);
        a.resolve();
        a
    }
    fn scope(&mut self, parent: usize, n: &Node) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            start: n.start,
            end: n.end,
        });
        id
    }
    fn bind(
        &mut self,
        uri: &str,
        n: &Node,
        scope: usize,
        visible: usize,
        kind: u32,
        detail: String,
    ) {
        if !symbol(n) {
            return;
        }
        if self
            .bindings
            .iter()
            .any(|b| b.target.start == n.start && b.target.uri == uri)
        {
            return;
        }
        self.bindings.push(Binding {
            name: n.text.clone(),
            target: Target {
                uri: uri.into(),
                start: n.start,
                end: n.end,
            },
            scope,
            visible,
            kind,
            detail,
        });
    }
    fn predeclare(&mut self, uri: &str, nodes: &[Node], scope: usize) {
        for n in nodes {
            if n.text != "(" {
                continue;
            }
            let c = &n.children;
            let head = c.first().map(|n| n.text.as_str()).unwrap_or("");
            if matches!(
                head,
                "def" | "defn" | "defmacro" | "defclass" | "defgeneric"
            ) {
                if let Some(name) = c.get(1) {
                    let kind = if matches!(head, "defn" | "defmacro" | "defgeneric") {
                        12
                    } else if head == "defclass" {
                        5
                    } else {
                        13
                    };
                    self.bind(
                        uri,
                        name,
                        scope,
                        0,
                        kind,
                        format!(
                            "({} {})",
                            head,
                            c.iter()
                                .skip(1)
                                .take(2)
                                .map(display)
                                .collect::<Vec<_>>()
                                .join(" ")
                        ),
                    );
                }
            }
            if head == "do" {
                self.predeclare(uri, &c[1..], scope);
            }
        }
    }
    fn walk(&mut self, uri: &str, n: &Node, scope: usize) {
        if n.text == "'" {
            return;
        }
        if n.text == "#" {
            if let Some(c) = n.children.first() {
                for child in &c.children {
                    self.walk(uri, child, scope);
                }
            }
            return;
        }
        if n.text == "(" && !n.children.is_empty() {
            let c = &n.children;
            let head = c[0].text.as_str();
            match head {
                "quote" | "syntax-rules" => return,
                "module" => {
                    let inner = self.scope(scope, n);
                    let mut body = 2;
                    if c.get(2).is_some_and(|n| n.text == ":export") {
                        body = 4;
                    }
                    if body <= c.len() {
                        self.predeclare(uri, &c[body..], inner);
                        for x in &c[body..] {
                            self.walk(uri, x, inner);
                        }
                        let mut names = export_names(&c[body..]);
                        if body == 4 {
                            if let Some(ns) = c.get(3) {
                                names.extend(ns.children.iter().map(name));
                            }
                        }
                        if let Some(module) = c.get(1) {
                            let exports = self.exported(inner, &names);
                            self.modules.insert(name(module), exports.clone());
                            self.exports.extend(exports);
                        }
                    }
                    return;
                }
                "require" => {
                    if let Some(module) = c.get(1) {
                        let mut import = Import {
                            module: name(module),
                            alias: None,
                            names: None,
                            scope,
                            start: n.start,
                        };
                        let mut i = 2;
                        while i + 1 < c.len() {
                            match c[i].text.as_str() {
                                ":as" => import.alias = Some(c[i + 1].text.clone()),
                                ":refer" => {
                                    import.names =
                                        Some(c[i + 1].children.iter().map(name).collect())
                                }
                                _ => {}
                            }
                            i += 2;
                        }
                        self.imports.push(import);
                    }
                    return;
                }
                "export" => return,
                "def" => {
                    if let Some(x) = c.get(2) {
                        self.walk(uri, x, scope);
                    }
                    return;
                }
                "fn" | "lambda" | "defn" | "defmacro" => {
                    let parameter = if matches!(head, "fn" | "lambda") {
                        1
                    } else {
                        2
                    };
                    let inner = self.scope(scope, n);
                    if let Some(params) = c.get(parameter) {
                        for p in &params.children {
                            self.bind(uri, p, inner, 0, 13, "parameter".into());
                        }
                    }
                    if parameter + 1 < c.len() {
                        self.predeclare(uri, &c[parameter + 1..], inner);
                        for body in &c[parameter + 1..] {
                            self.walk(uri, body, inner);
                        }
                    }
                    return;
                }
                "let" | "let*" | "loop" => {
                    let inner = self.scope(scope, n);
                    if let Some(bindings) = c.get(1) {
                        let b = &bindings.children;
                        let pairs: Vec<(&Node, &Node)> = if bindings.text == "("
                            && b.iter().all(|n| n.text == "(" && n.children.len() == 2)
                        {
                            b.iter().map(|p| (&p.children[0], &p.children[1])).collect()
                        } else {
                            b.as_chunks::<2>()
                                .0
                                .iter()
                                .map(|p| (&p[0], &p[1]))
                                .collect()
                        };
                        for (binding, init) in pairs {
                            self.walk(uri, init, if head == "let*" { inner } else { scope });
                            self.bind(
                                uri,
                                binding,
                                inner,
                                if head == "let*" {
                                    init.end
                                } else {
                                    bindings.end
                                },
                                13,
                                "lexical binding".into(),
                            );
                        }
                    }
                    for body in c.iter().skip(2) {
                        self.walk(uri, body, inner);
                    }
                    return;
                }
                "catch" => {
                    let inner = self.scope(scope, n);
                    if let Some(binding) = c.get(2) {
                        self.bind(uri, binding, inner, 0, 13, "condition".into());
                    }
                    for body in c.iter().skip(3) {
                        self.walk(uri, body, inner);
                    }
                    return;
                }
                _ => {}
            }
        }
        if symbol(n) {
            self.references.push(Reference {
                name: n.text.clone(),
                start: n.start,
                end: n.end,
                scope,
                target: None,
            });
        }
        for c in &n.children {
            self.walk(uri, c, scope);
        }
    }
    fn exported(&self, scope: usize, names: &BTreeSet<String>) -> BTreeMap<String, Target> {
        self.bindings
            .iter()
            .filter(|b| b.scope == scope && names.contains(&b.name))
            .map(|b| (b.name.clone(), b.target.clone()))
            .collect()
    }
    pub fn resolve(&mut self) {
        for r in &mut self.references {
            r.target = None;
            let mut scope = Some(r.scope);
            while let Some(s) = scope {
                if let Some(b) = self
                    .bindings
                    .iter()
                    .rev()
                    .find(|b| b.scope == s && b.name == r.name && b.visible <= r.start)
                {
                    r.target = Some(b.target.clone());
                    break;
                }
                scope = self.scopes[s].parent;
            }
        }
    }
    pub fn import(&mut self, import: &Import, exports: &BTreeMap<String, Target>) {
        for (name, target) in exports {
            if import
                .names
                .as_ref()
                .is_none_or(|names| names.contains(name))
            {
                self.bindings.push(Binding {
                    name: name.clone(),
                    target: target.clone(),
                    scope: import.scope,
                    visible: import.start,
                    kind: 12,
                    detail: format!("export from {}", import.module),
                });
            }
            if let Some(alias) = &import.alias {
                self.bindings.push(Binding {
                    name: format!("{}/{}", alias, name),
                    target: target.clone(),
                    scope: import.scope,
                    visible: import.start,
                    kind: 12,
                    detail: format!("export from {}", import.module),
                });
            }
        }
    }
    pub fn target(&self, point: usize) -> Option<Target> {
        self.bindings
            .iter()
            .find(|b| {
                b.target.start <= point && point < b.target.end && b.detail != "parameter imported"
            })
            .map(|b| b.target.clone())
            .or_else(|| {
                self.references
                    .iter()
                    .find(|r| r.start <= point && point < r.end)
                    .and_then(|r| r.target.clone())
            })
    }
}
fn display(n: &Node) -> String {
    if n.children.is_empty() {
        n.text.clone()
    } else {
        format!(
            "{}{}{}",
            n.text,
            n.children.iter().map(display).collect::<Vec<_>>().join(" "),
            match n.text.as_str() {
                "(" => ")",
                "[" => "]",
                "{" => "}",
                _ => "",
            }
        )
    }
}
fn export_names(nodes: &[Node]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for n in nodes {
        if n.text == "(" && n.children.first().is_some_and(|n| n.text == "export") {
            for x in n.children.iter().skip(1) {
                if x.children.is_empty() {
                    names.insert(name(x));
                } else {
                    names.extend(x.children.iter().map(name));
                }
            }
        }
    }
    names
}
