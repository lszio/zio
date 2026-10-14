//! Image wire format: the record layout an encoded runtime graph travels
//! in, plus bounded validation. Node ids are dense (`id == index`); every
//! cross-node edge is a `{"$ref": id}` payload value. Validation is the
//! only trust boundary — nothing here reads a payload without passing it.

use serde::{Deserialize, Serialize};

use crate::error::EvalError;

use super::ImageLimits;

pub const FORMAT: &str = "zio-image";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Header {
    pub format: String,
    #[serde(rename = "format_version")]
    pub format_version: u32,
    pub runtime: String,
    pub profile: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Node {
    pub id: usize,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceRequirement {
    pub kind: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModuleRecord {
    pub name: String,
    pub exports: Vec<String>,
    /// Node id of the module's own environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<usize>,
    /// Source file name, when the module was loaded from disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One encoded runtime graph. `roots` are node ids; `nodes` are dense by
/// id. Modules and resources are side tables: modules record what restore
/// must re-bind, resources name what restore must re-grant or refuse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Image {
    pub header: Header,
    pub roots: Vec<usize>,
    /// Names for `roots`, same length. A name is a binding key the
    /// restore side chooses; it is not a path and carries no authority.
    #[serde(default)]
    pub root_names: Vec<String>,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub modules: Vec<ModuleRecord>,
    #[serde(default)]
    pub resources: Vec<ResourceRequirement>,
}

impl Image {
    pub fn parse(text: &str) -> Result<Self, EvalError> {
        let image: Image = serde_json::from_str(text)
            .map_err(|error| EvalError::invalid_form(format!("malformed image: {error}")))?;
        image.validate(&ImageLimits::default())?;
        Ok(image)
    }

    pub fn canonical(&self) -> Result<String, EvalError> {
        serde_json::to_string(self).map_err(|error| EvalError::custom(format!("encode: {error}")))
    }

    /// Bounded structural validation. Every failure is a named error; the
    /// reference walk is iterative so a hostile payload cannot exhaust the
    /// stack while being rejected.
    pub fn validate(&self, limits: &ImageLimits) -> Result<(), EvalError> {
        if self.header.format != FORMAT {
            return Err(EvalError::invalid_form(format!(
                "unknown image format {:?}; expected {FORMAT:?}",
                self.header.format
            )));
        }
        if self.header.format_version != FORMAT_VERSION {
            return Err(EvalError::invalid_form(format!(
                "unsupported image format version {}; expected {FORMAT_VERSION}",
                self.header.format_version
            )));
        }
        let bytes = self.canonical()?.len();
        if bytes > limits.max_bytes {
            return Err(EvalError::invalid_form(format!(
                "image exceeds byte limit: {bytes} > {}",
                limits.max_bytes
            )));
        }
        if self.nodes.len() > limits.max_nodes {
            return Err(EvalError::invalid_form(format!(
                "image exceeds node limit: {} > {}",
                self.nodes.len(),
                limits.max_nodes
            )));
        }
        for (index, node) in self.nodes.iter().enumerate() {
            if node.id != index {
                return Err(EvalError::invalid_form(format!(
                    "duplicate node id {} at index {index} (ids must be dense)",
                    node.id
                )));
            }
            let path = format!("nodes[{}]", node.id);
            check_shape(node, &path)?;
            let mut edges = 0usize;
            for (id, at) in collect_refs(&node.payload, &path, limits)? {
                edges += 1;
                if edges > limits.max_edges {
                    return Err(EvalError::invalid_form(format!(
                        "image exceeds edge limit: {edges} > {} at {at}",
                        limits.max_edges
                    )));
                }
                if id >= self.nodes.len() {
                    return Err(EvalError::invalid_form(format!(
                        "reference to missing node {id} at {at}"
                    )));
                }
            }
        }
        for id in &self.roots {
            if *id >= self.nodes.len() {
                return Err(EvalError::invalid_form(format!(
                    "root references missing node {id}"
                )));
            }
        }
        if !self.root_names.is_empty() && self.root_names.len() != self.roots.len() {
            return Err(EvalError::invalid_form(format!(
                "image has {} roots but {} root names",
                self.roots.len(),
                self.root_names.len()
            )));
        }
        let mut seen_names = std::collections::HashSet::new();
        for name in &self.root_names {
            if !seen_names.insert(name) {
                return Err(EvalError::invalid_form(format!(
                    "duplicate root name {name:?}"
                )));
            }
        }
        for module in &self.modules {
            if module.name.is_empty() {
                return Err(EvalError::invalid_form("module record with empty name"));
            }
            if let Some(id) = module.env {
                if id >= self.nodes.len() {
                    return Err(EvalError::invalid_form(format!(
                        "module {:?} references missing env node {id}",
                        module.name
                    )));
                }
            }
        }
        for resource in &self.resources {
            if resource.name.is_empty() {
                return Err(EvalError::invalid_form(format!(
                    "resource {:?} at {} has an empty name",
                    resource.kind, resource.path
                )));
            }
        }
        Ok(())
    }
}

/// Required top-level payload field per kind, structural only — semantic
/// reconstruction belongs to restore, not validation.
fn check_shape(node: &Node, path: &str) -> Result<(), EvalError> {
    let Some(object) = node.payload.as_object() else {
        return Err(EvalError::invalid_form(format!(
            "payload mismatch for kind {} at {path}: not an object",
            node.kind
        )));
    };
    let field = match node.kind.as_str() {
        "nil" | "boolean" | "integer" | "float" | "string" | "symbol" | "keyword" | "char"
        | "cell" => "value",
        "list" | "vector" => "items",
        "map" => "entries",
        "env" => "bindings",
        "function" | "macro" => "params",
        "native" => "name",
        "buffer" => "bytes",
        "instance" => "class",
        "class" => "name",
        "generic-function" => "name",
        "module" => "json",
        other => {
            return Err(EvalError::invalid_form(format!(
                "unknown node kind {other:?} at {path}"
            )));
        }
    };
    let Some(value) = object.get(field) else {
        return Err(EvalError::invalid_form(format!(
            "payload mismatch: kind {} missing {field:?} at {path}",
            node.kind
        )));
    };
    let type_ok = match node.kind.as_str() {
        "nil" => value.is_null(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "float" => value.is_number(),
        "string" | "symbol" | "keyword" => value.is_string(),
        "char" => value.is_string(),
        "native" | "class" | "generic-function" | "module" => value.is_string(),
        "buffer" => value.is_array(),
        "list" | "vector" | "map" | "env" | "function" | "macro" | "cell" | "instance" => true,
        _ => false,
    };
    if type_ok {
        Ok(())
    } else {
        Err(EvalError::invalid_form(format!(
            "payload mismatch: kind {} has wrong {field:?} type at {path}",
            node.kind
        )))
    }
}

/// Iteratively collect `{"$ref": id}` values from a payload, capping
/// nesting depth. Returns `(node id, path)` pairs in encounter order.
fn collect_refs(
    payload: &serde_json::Value,
    root: &str,
    limits: &ImageLimits,
) -> Result<Vec<(usize, String)>, EvalError> {
    let mut refs = Vec::new();
    let mut stack = vec![(payload, root.to_string(), 0usize)];
    while let Some((value, path, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(EvalError::invalid_form(format!(
                "payload exceeds nesting limit {} at {path}",
                limits.max_depth
            )));
        }
        match value {
            serde_json::Value::Object(map) => {
                if let Some(reference) = map.get("$ref") {
                    let Some(id) = reference.as_u64() else {
                        return Err(EvalError::invalid_form(format!(
                            "malformed reference at {path}"
                        )));
                    };
                    refs.push((id as usize, path));
                    continue;
                }
                for (key, child) in map {
                    stack.push((child, format!("{path}.{key}"), depth + 1));
                }
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    stack.push((child, format!("{path}[{index}]"), depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(refs)
}
