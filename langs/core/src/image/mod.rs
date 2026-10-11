//! Runtime graph encoding: turn live `Value` roots into an identity-aware
//! wire image. Identity carriers (environments, closures, macros, buffers,
//! binding cells, classes, modules, ZOS instances with an explicit
//! identity) intern to one node per identity; pure data encodes inline and
//! clones per occurrence, matching the language's own clone semantics.
//! This is the M00 format premise — restore lives in M01.

pub mod restore;
pub mod save;
pub mod wire;

use std::collections::HashMap;
use std::sync::Arc;

use crate::bytecode::{BindingCell, Closure};
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{Macro, NativeFn, Value};

pub use wire::{Header, Image, ModuleRecord, Node, ResourceRequirement};

/// Bounded-resource budgets for encoding and validation. Defaults are
/// generous for real sessions; contracts use tighter values by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_bytes: usize,
    /// Pure-data and payload nesting budget, bounding recursion.
    pub max_depth: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        ImageLimits {
            max_nodes: 1 << 20,
            max_edges: 1 << 21,
            max_bytes: 256 << 20,
            // Debug-build encode/drop frames are fat and deeply nested
            // runtime Values already overflow test-thread stacks (the
            // known quoted-list limit). 256 keeps every path safe; raise
            // only together with an iterative encoder.
            max_depth: 256,
        }
    }
}

/// What a value can become in an image. This is the savability table for
/// the runtime variants: pure data clones per occurrence, identity
/// carriers intern, native bindings need restore-time registration, and
/// active resources are refused with the object path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Savability {
    /// Pure data: inline, clones per occurrence.
    Pure,
    /// Identity carrier: one node per interned identity.
    Identity,
    /// Needs restore-time registration (native binding).
    Registered,
    /// Active resource: refused with the object path.
    Refused,
}

/// Savability of a runtime variant. The counter id of a `NativeFn` is a
/// process-lifetime number and is never encoded — only its registered name.
pub fn savability(value: &Value) -> Savability {
    match value {
        Value::Nil
        | Value::Boolean(_)
        | Value::Integer(_)
        | Value::Float(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Keyword(_)
        | Value::Char(_)
        | Value::List(_)
        | Value::Vector(_)
        | Value::Map(_) => Savability::Pure,
        Value::Function(_) | Value::Macro(_) | Value::Buffer(_) | Value::Object(_) => {
            Savability::Identity
        }
        Value::NativeFunction(_) => Savability::Registered,
        Value::Future(_) | Value::Channel(_) => Savability::Refused,
    }
}

/// Keys that intern identity carriers. Pointers, not names: two envs with
/// equal contents stay distinct nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GraphKey {
    Env(*const Env),
    Function(*const crate::value::Function),
    Macro(*const Macro),
    Buffer(*const u8),
    Cell(*const u8),
    Class(*const crate::zos::object::Class),
    Module(*const u8),
    /// ZOS instance with an explicit identity number.
    Object(u64),
}

struct Encoder {
    nodes: Vec<Node>,
    interned: HashMap<GraphKey, usize>,
    limits: ImageLimits,
    resources: Vec<ResourceRequirement>,
    depth: usize,
    /// Save walks environment parent chains; plain graph encoding (M00
    /// contracts) keeps each env self-contained.
    encode_parents: bool,
}

/// Encode `roots` into a validated wire image.
pub fn encode_graph(roots: &[Value], limits: &ImageLimits) -> Result<Image, EvalError> {
    let mut encoder = Encoder {
        nodes: Vec::new(),
        interned: HashMap::new(),
        limits: *limits,
        resources: Vec::new(),
        depth: 0,
        encode_parents: false,
    };
    let mut root_ids = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        let encoded = encoder.value(root, &format!("roots[{index}]"))?;
        root_ids.push(match encoded {
            Encoded::Inline(payload) => encoder.node_from_payload(root, payload, index)?,
            Encoded::Ref(id) => id,
        });
    }
    let image = Image {
        header: Header {
            format: wire::FORMAT.to_string(),
            format_version: wire::FORMAT_VERSION,
            runtime: env!("CARGO_PKG_VERSION").to_string(),
            profile: "graph-v1".to_string(),
        },
        roots: root_ids,
        root_names: Vec::new(),
        nodes: encoder.nodes,
        modules: Vec::new(),
        resources: encoder.resources,
    };
    image.validate(limits)?;
    Ok(image)
}

/// One encoded value: either inline JSON data or a reference to a node.
enum Encoded {
    Inline(serde_json::Value),
    Ref(usize),
}

impl Encoder {
    fn node_limit(&self, path: &str) -> Result<(), EvalError> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(EvalError::invalid_form(format!(
                "image exceeds node limit {} at {path}",
                self.limits.max_nodes
            )));
        }
        Ok(())
    }

    fn push_node(&mut self, kind: &str, payload: serde_json::Value) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            id,
            kind: kind.to_string(),
            payload,
        });
        id
    }

    fn node_from_payload(
        &mut self,
        value: &Value,
        payload: serde_json::Value,
        index: usize,
    ) -> Result<usize, EvalError> {
        self.node_limit(&format!("roots[{index}]"))?;
        Ok(self.push_node(root_kind(value), payload))
    }

    fn value(&mut self, value: &Value, path: &str) -> Result<Encoded, EvalError> {
        if self.depth > self.limits.max_depth {
            return Err(EvalError::invalid_form(format!(
                "image exceeds nesting limit {} at {path}",
                self.limits.max_depth
            )));
        }
        self.depth += 1;
        let result = self.value_inner(value, path);
        self.depth -= 1;
        result
    }

    fn value_inner(&mut self, value: &Value, path: &str) -> Result<Encoded, EvalError> {
        match value {
            Value::Nil => Ok(Encoded::Inline(serde_json::json!({ "value": null }))),
            Value::Boolean(inner) => Ok(Encoded::Inline(serde_json::json!({ "value": inner }))),
            Value::Integer(inner) => Ok(Encoded::Inline(serde_json::json!({ "value": inner }))),
            Value::Float(inner) => {
                if inner.is_finite() {
                    Ok(Encoded::Inline(serde_json::json!({ "value": inner })))
                } else {
                    Err(EvalError::invalid_form(format!(
                        "non-finite float cannot be encoded at {path}"
                    )))
                }
            }
            Value::String(inner) => Ok(Encoded::Inline(serde_json::json!({
                "value": inner,
                "kind": "string",
            }))),
            Value::Symbol(inner) => Ok(Encoded::Inline(serde_json::json!({
                "value": inner,
                "kind": "symbol",
            }))),
            Value::Keyword(inner) => Ok(Encoded::Inline(serde_json::json!({
                "value": inner,
                "kind": "keyword",
            }))),
            Value::Char(inner) => Ok(Encoded::Inline(
                serde_json::json!({ "value": inner.to_string(), "kind": "char" }),
            )),
            Value::List(items) => self.sequence(items.iter(), "list", path),
            Value::Vector(items) => self.sequence(items.iter(), "vector", path),
            Value::Map(entries) => {
                let mut encoded: Vec<(String, serde_json::Value)> = Vec::new();
                for (key, entry) in entries.iter() {
                    let key = self.entry_side(key, &format!("{path}.key"))?;
                    let value = self.entry_side(entry, &format!("{path}.value"))?;
                    let pair = serde_json::json!([key, value]);
                    let order = serde_json::to_string(&pair).expect("json");
                    encoded.push((order, pair));
                }
                encoded.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(Encoded::Inline(serde_json::json!({
                    "entries": encoded.into_iter().map(|(_, entry)| entry).collect::<Vec<_>>()
                })))
            }
            Value::Function(inner) => self.function(inner, path).map(Encoded::Ref),
            Value::Macro(inner) => self.macro_(inner, path).map(Encoded::Ref),
            Value::NativeFunction(inner) => self.native(inner, path).map(Encoded::Ref),
            Value::Buffer(bytes) => {
                let key = GraphKey::Buffer(Arc::as_ptr(bytes) as *const u8);
                if let Some(id) = self.interned.get(&key) {
                    return Ok(Encoded::Ref(*id));
                }
                self.node_limit(path)?;
                let payload = serde_json::json!({
                    "bytes": bytes.lock().clone(),
                });
                let id = self.push_node("buffer", payload);
                self.interned.insert(key, id);
                Ok(Encoded::Ref(id))
            }
            Value::Object(object) => self.object(object.as_ref(), path).map(Encoded::Ref),
            Value::Future(_) => Err(EvalError::invalid_form(format!(
                "active resource cannot be encoded: future at {path}"
            ))),
            Value::Channel(_) => Err(EvalError::invalid_form(format!(
                "active resource cannot be encoded: channel at {path}"
            ))),
        }
    }

    fn sequence<'a>(
        &mut self,
        items: impl Iterator<Item = &'a Value>,
        kind: &str,
        path: &str,
    ) -> Result<Encoded, EvalError> {
        let mut encoded = Vec::new();
        for (index, item) in items.enumerate() {
            match self.value(item, &format!("{path}[{index}]"))? {
                Encoded::Inline(payload) => encoded.push(payload),
                Encoded::Ref(id) => encoded.push(serde_json::json!({ "$ref": id })),
            }
        }
        Ok(Encoded::Inline(
            serde_json::json!({ "seq": kind, "items": encoded }),
        ))
    }

    /// A map key or value: inline when pure, a reference node when it is
    /// an identity carrier appearing as data.
    fn entry_side(&mut self, value: &Value, path: &str) -> Result<serde_json::Value, EvalError> {
        match self.value(value, path)? {
            Encoded::Inline(payload) => Ok(payload),
            Encoded::Ref(id) => Ok(serde_json::json!({ "$ref": id })),
        }
    }

    fn env(&mut self, env: &Arc<Env>, path: &str) -> Result<usize, EvalError> {
        let key = GraphKey::Env(Arc::as_ptr(env));
        if let Some(id) = self.interned.get(&key) {
            return Ok(*id);
        }
        self.node_limit(path)?;
        // Reserve the id before encoding children so cycles close.
        let reserved = self.push_node("env", serde_json::json!({ "bindings": {} }));
        self.interned.insert(key, reserved);
        let mut bindings = serde_json::Map::new();
        for (name, bound) in env.owned_bindings() {
            let encoded = self.value(&bound, &format!("{path}.{name}"))?;
            let encoded = match encoded {
                Encoded::Inline(payload) => payload,
                Encoded::Ref(id) => serde_json::json!({ "$ref": id }),
            };
            bindings.insert(name, encoded);
        }
        let mut payload = serde_json::json!({ "bindings": bindings });
        if self.encode_parents {
            if let Some(parent) = env.parent() {
                let parent_id = self.env(&parent, &format!("{path}.parent"))?;
                payload["parent"] = serde_json::json!({ "$ref": parent_id });
            }
        }
        self.nodes[reserved].payload = payload;
        Ok(reserved)
    }

    fn function(
        &mut self,
        function: &Arc<crate::value::Function>,
        path: &str,
    ) -> Result<usize, EvalError> {
        let key = GraphKey::Function(Arc::as_ptr(function));
        if let Some(id) = self.interned.get(&key) {
            return Ok(*id);
        }
        self.node_limit(path)?;
        let reserved = self.push_node("function", serde_json::json!({ "params": [] }));
        self.interned.insert(key, reserved);
        let payload = match &function.compiled {
            Some(closure) => self.compiled_payload(
                closure,
                function.params.clone(),
                function.rest_param.clone(),
                path,
            )?,
            None => {
                let body =
                    self.entry_side(&Value::from(function.body.clone()), &format!("{path}.body"))?;
                let env_id = self.env(&function.env, &format!("{path}.env"))?;
                serde_json::json!({
                    "params": function.params.iter().cloned().collect::<Vec<_>>(),
                    "rest": function.rest_param,
                    "body": body,
                    "env": { "$ref": env_id },
                })
            }
        };
        self.nodes[reserved].payload = payload;
        Ok(reserved)
    }

    fn macro_(&mut self, macro_: &Arc<Macro>, path: &str) -> Result<usize, EvalError> {
        let key = GraphKey::Macro(Arc::as_ptr(macro_));
        if let Some(id) = self.interned.get(&key) {
            return Ok(*id);
        }
        self.node_limit(path)?;
        let reserved = self.push_node("macro", serde_json::json!({ "params": [] }));
        self.interned.insert(key, reserved);
        let payload = match &macro_.compiled {
            Some(closure) => {
                let mut payload = self.compiled_payload(
                    closure,
                    macro_.params.clone(),
                    macro_.rest_param.clone(),
                    path,
                )?;
                payload["name"] = serde_json::json!(macro_.name);
                payload
            }
            None => {
                let body =
                    self.entry_side(&Value::from(macro_.body.clone()), &format!("{path}.body"))?;
                let env_id = self.env(&macro_.env, &format!("{path}.env"))?;
                serde_json::json!({
                    "name": macro_.name,
                    "params": macro_.params.iter().cloned().collect::<Vec<_>>(),
                    "rest": macro_.rest_param,
                    "body": body,
                    "env": { "$ref": env_id },
                })
            }
        };
        self.nodes[reserved].payload = payload;
        Ok(reserved)
    }

    /// Compiled callables reference their module artifact instead of
    /// copying instructions. Captures are shared binding cells.
    fn compiled_payload(
        &mut self,
        closure: &Arc<Closure>,
        params: im::Vector<String>,
        rest: Option<String>,
        path: &str,
    ) -> Result<serde_json::Value, EvalError> {
        let module_key = GraphKey::Module(Arc::as_ptr(&closure.module) as *const u8);
        let module_id = if let Some(id) = self.interned.get(&module_key) {
            *id
        } else {
            self.node_limit(&format!("{path}.module"))?;
            let json = closure.module.to_wire_json()?;
            let id = self.push_node("module", serde_json::json!({ "json": json }));
            self.interned.insert(module_key, id);
            id
        };
        let mut captures = Vec::with_capacity(closure.captures.len());
        for (index, cell) in closure.captures.iter().enumerate() {
            let cell_id = self.cell(cell, &format!("{path}.captures[{index}]"))?;
            captures.push(serde_json::json!({ "$ref": cell_id }));
        }
        let name = closure
            .module
            .functions
            .get(closure.fn_index)
            .and_then(|record| record.name.clone());
        Ok(serde_json::json!({
            "params": params.iter().cloned().collect::<Vec<_>>(),
            "rest": rest,
            "module": { "$ref": module_id },
            "function": name,
            "captures": captures,
        }))
    }

    fn cell(
        &mut self,
        cell: &std::rc::Rc<std::cell::RefCell<BindingCell>>,
        path: &str,
    ) -> Result<usize, EvalError> {
        let key = GraphKey::Cell(std::rc::Rc::as_ptr(cell) as *const u8);
        if let Some(id) = self.interned.get(&key) {
            return Ok(*id);
        }
        self.node_limit(path)?;
        let reserved = self.push_node("cell", serde_json::json!({ "value": null }));
        self.interned.insert(key, reserved);
        let payload = match &*cell.borrow() {
            BindingCell::Bound(value) => {
                let encoded = self.value(value, &format!("{path}.value"))?;
                match encoded {
                    Encoded::Inline(payload) => payload,
                    Encoded::Ref(id) => serde_json::json!({ "value": { "$ref": id } }),
                }
            }
            BindingCell::Unbound { name, .. } => {
                return Err(EvalError::invalid_form(format!(
                    "unbound reserved binding {name:?} cannot be encoded at {path}"
                )));
            }
        };
        self.nodes[reserved].payload = payload;
        Ok(reserved)
    }

    fn native(&mut self, native: &NativeFn, path: &str) -> Result<usize, EvalError> {
        self.node_limit(path)?;
        self.resources.push(ResourceRequirement {
            kind: "native-binding".to_string(),
            name: native.name().to_string(),
            path: path.to_string(),
        });
        Ok(self.push_node("native", serde_json::json!({ "name": native.name() })))
    }

    fn object(
        &mut self,
        object: &dyn crate::zos::object::ZosObject,
        path: &str,
    ) -> Result<usize, EvalError> {
        if let Some(instance) = object.as_any().downcast_ref::<crate::value::ZosInstance>() {
            return self.instance(instance, path);
        }
        if let Some(gf) = object.as_any().downcast_ref::<crate::zos::gf::GFObject>() {
            return self.generic_function(gf, path);
        }
        Err(EvalError::invalid_form(format!(
            "unsupported ZOS object type {:?} at {path}",
            object.header().class.name
        )))
    }

    fn class(
        &mut self,
        class: &Arc<crate::zos::object::Class>,
        path: &str,
    ) -> Result<usize, EvalError> {
        let key = GraphKey::Class(Arc::as_ptr(class));
        if let Some(id) = self.interned.get(&key) {
            return Ok(*id);
        }
        self.node_limit(&format!("{path}.class"))?;
        let reserved = self.push_node("class", serde_json::json!({ "name": class.name }));
        self.interned.insert(key, reserved);
        let mut superclasses = Vec::with_capacity(class.superclasses.len());
        for (index, parent) in class.superclasses.iter().enumerate() {
            let id = self.class(parent, &format!("{path}.superclasses[{index}]"))?;
            superclasses.push(serde_json::json!({ "$ref": id }));
        }
        let mut slots = Vec::with_capacity(class.slots.len());
        for slot in &class.slots {
            let initform = match &slot.initform {
                Some(form) => Some(self.entry_side(form, &format!("{path}.slots.initform"))?),
                None => None,
            };
            slots.push(serde_json::json!({
                "name": slot.name,
                "initargs": slot.initargs,
                "initform": initform,
                "accessor": slot.accessor,
            }));
        }
        self.nodes[reserved].payload = serde_json::json!({
            "name": class.name,
            "superclasses": superclasses,
            "slots": slots,
            "cpl": class.cpl,
        });
        Ok(reserved)
    }

    fn instance(
        &mut self,
        instance: &crate::value::ZosInstance,
        path: &str,
    ) -> Result<usize, EvalError> {
        // Explicit identity interns; anonymous instances clone per
        // occurrence, matching the language's own clone semantics.
        let key = instance.header.identity.map(GraphKey::Object);
        if let Some(key) = key {
            if let Some(id) = self.interned.get(&key) {
                return Ok(*id);
            }
        }
        self.node_limit(path)?;
        let reserved = self.push_node("instance", serde_json::json!({ "class": 0 }));
        if let Some(key) = key {
            self.interned.insert(key, reserved);
        }
        let class_id = self.class(&instance.header.class, path)?;
        let mut slots = serde_json::Map::new();
        for (name, slot) in &instance.slots {
            let encoded = self.value(slot, &format!("{path}.{name}"))?;
            let encoded = match encoded {
                Encoded::Inline(payload) => payload,
                Encoded::Ref(id) => serde_json::json!({ "$ref": id }),
            };
            slots.insert(name.clone(), encoded);
        }
        self.nodes[reserved].payload = serde_json::json!({
            "class": { "$ref": class_id },
            "identity": instance.header.identity,
            "flags": instance.header.flags.bits(),
            "slots": slots,
        });
        Ok(reserved)
    }

    fn generic_function(
        &mut self,
        gf: &crate::zos::gf::GFObject,
        path: &str,
    ) -> Result<usize, EvalError> {
        self.node_limit(path)?;
        let class_id = self.class(&gf.header.class, &format!("{path}.class"))?;
        let gf = gf.gf.borrow();
        let mut methods = Vec::with_capacity(gf.methods.len());
        for (index, method) in gf.methods.iter().enumerate() {
            let body_id = self.function(&method.body, &format!("{path}.methods[{index}]"))?;
            let specializers: Vec<String> = method
                .specializers
                .iter()
                .map(|specializer| match specializer {
                    crate::zos::gf::Specializer::T => "T".to_string(),
                    crate::zos::gf::Specializer::Exact(name) => format!("exact:{name}"),
                })
                .collect();
            methods.push(serde_json::json!({
                "specializers": specializers,
                "qualifier": format!("{:?}", method.qualifier).to_lowercase(),
                "body": { "$ref": body_id },
            }));
        }
        // The dispatch cache is derived state; restore rebuilds it.
        Ok(self.push_node(
            "generic-function",
            serde_json::json!({
                "class": { "$ref": class_id },
                "name": gf.name,
                "lambda_list": gf.lambda_list,
                "methods": methods,
            }),
        ))
    }
}

fn root_kind(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::Char(_) => "char",
        Value::List(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        _ => unreachable!("identity carriers and refused values never inline at root"),
    }
}
