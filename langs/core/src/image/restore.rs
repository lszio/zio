//! Still-image restore: bounded validation first, then a build that
//! materializes the whole graph before the context is touched. Nothing
//! partial is committed: a failure anywhere leaves the target context
//! unmodified.
//!
//! Identity rules mirror the encoder: environments and binding cells are
//! reserved before their contents fill (they have interior mutability),
//! so env ↔ closure cycles close on the same object. Functions, macros
//! and instances build after their children — a value cycle can only run
//! through an environment, never through a function directly.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use super::ImageLimits;
use super::wire::{self, Node};
use crate::bytecode::{BindingCell, Closure, ModuleArtifact};
use crate::context::EvalContext;
use crate::env::Env;
use crate::error::EvalError;
use crate::macros::value_to_sexp;
use crate::module::Module;
use crate::value::{Function, Macro, Value};
use crate::zos::gf::{GFObject, GenericFunction, Method, MethodQualifier, Specializer};
use crate::zos::object::{Class, ObjectFlags, SlotDefinition};

/// Validate and rebuild `image` inside `ctx`, returning the root values
/// in root order. Modules register last, after every root has built.
pub fn restore_image(
    ctx: &EvalContext,
    image: &wire::Image,
    limits: &ImageLimits,
) -> Result<Vec<Value>, EvalError> {
    image.validate(limits)?;
    if image.header.runtime != env!("CARGO_PKG_VERSION") {
        return Err(EvalError::invalid_form(format!(
            "runtime version mismatch: image was saved by {}, this is {}",
            image.header.runtime,
            env!("CARGO_PKG_VERSION")
        )));
    }
    // Native bindings are named requirements: the target context must
    // already hold the registration, else restore fails with the path
    // that needed it. Pointer and counter ids never travel in the format.
    for resource in &image.resources {
        if resource.kind != "native-binding" {
            return Err(EvalError::invalid_form(format!(
                "unknown resource kind {:?} at {}",
                resource.kind, resource.path
            )));
        }
        let bound = ctx.env.get(&resource.name);
        let ok =
            matches!(&bound, Some(Value::NativeFunction(native)) if native.name() == resource.name);
        if !ok {
            return Err(EvalError::invalid_form(format!(
                "missing native binding {:?} required at {}",
                resource.name, resource.path
            )));
        }
    }

    let mut builder = Builder {
        image,
        values: HashMap::new(),
        envs: HashMap::new(),
        cells: HashMap::new(),
        modules: HashMap::new(),
        classes: HashMap::new(),
        building: HashSet::new(),
        pending_envs: Vec::new(),
        pending_cells: Vec::new(),
        depth: 0,
        limits,
        ctx,
    };
    let mut roots = Vec::with_capacity(image.roots.len());
    for id in &image.roots {
        roots.push(builder.value(*id)?);
    }
    // Module environments join the graph before the link phase, so their
    // bindings fill with everything else.
    let mut module_envs = Vec::with_capacity(image.modules.len());
    for record in &image.modules {
        let Some(env_id) = record.env else {
            return Err(EvalError::invalid_form(format!(
                "module {:?} has no environment to restore",
                record.name
            )));
        };
        module_envs.push(builder.env(env_id)?);
    }
    // Link phase: every value now exists, so env bindings and capture
    // cells resolve to the shared objects instead of half-built ones.
    while let Some(id) = builder.pending_envs.first().copied() {
        builder.pending_envs.remove(0);
        builder.fill_env(id)?;
    }
    while let Some(id) = builder.pending_cells.first().copied() {
        builder.pending_cells.remove(0);
        builder.fill_cell(id)?;
    }

    // Commit point: everything above built without touching `ctx`.
    for (record, env) in image.modules.iter().zip(module_envs) {
        ctx.modules.borrow_mut().modules.push(Module {
            name: record.name.split('.').map(str::to_string).collect(),
            env,
            exports: record.exports.clone(),
            source: None,
        });
    }
    Ok(roots)
}

struct Builder<'a> {
    image: &'a wire::Image,
    values: HashMap<usize, Value>,
    envs: HashMap<usize, Arc<Env>>,
    cells: HashMap<usize, Rc<RefCell<BindingCell>>>,
    modules: HashMap<usize, Arc<ModuleArtifact>>,
    classes: HashMap<usize, Arc<Class>>,
    /// Function/macro nodes currently being built — a direct value cycle
    /// through them is unrepresentable and named here instead of hanging.
    building: HashSet<usize>,
    /// Env/cell nodes reserved but not yet linked; filled once every
    /// value exists.
    pending_envs: Vec<usize>,
    pending_cells: Vec<usize>,
    depth: usize,
    limits: &'a ImageLimits,
    ctx: &'a EvalContext,
}

impl Builder<'_> {
    fn node(&self, id: usize) -> Result<&Node, EvalError> {
        self.image
            .nodes
            .get(id)
            .ok_or_else(|| EvalError::invalid_form(format!("reference to missing node {id}")))
    }

    fn entering(&mut self, path: &str) -> Result<(), EvalError> {
        if self.depth > self.limits.max_depth {
            return Err(EvalError::invalid_form(format!(
                "restore exceeds nesting limit {} at {path}",
                self.limits.max_depth
            )));
        }
        self.depth += 1;
        Ok(())
    }

    fn ref_id(reference: &serde_json::Value, path: &str) -> Result<usize, EvalError> {
        reference
            .get("$ref")
            .and_then(|r| r.as_u64())
            .map(|id| id as usize)
            .ok_or_else(|| EvalError::invalid_form(format!("malformed reference at {path}")))
    }

    /// Build a value from an inline payload — the encoder's inline form
    /// (`{"value": …}` scalars, `{"seq", "items"}` sequences, `{"entries"}`
    /// maps) — or follow a reference to a node.
    fn payload_value(
        &mut self,
        payload: &serde_json::Value,
        path: &str,
    ) -> Result<Value, EvalError> {
        match payload {
            serde_json::Value::Null => Ok(Value::Nil),
            serde_json::Value::Bool(inner) => Ok(Value::Boolean(*inner)),
            serde_json::Value::Number(number) => {
                if let Some(int) = number.as_i64() {
                    Ok(Value::Integer(int))
                } else {
                    Ok(Value::Float(number.as_f64().unwrap_or_default()))
                }
            }
            serde_json::Value::String(inner) => Ok(Value::String(inner.clone())),
            serde_json::Value::Object(map) => {
                if map.contains_key("$ref") {
                    let id = Self::ref_id(payload, path)?;
                    return self.value(id);
                }
                if map.contains_key("value") && map.len() <= 2 {
                    let inner = self.payload_value(&map["value"], &format!("{path}.value"))?;
                    let coerced = match map.get("kind").and_then(|k| k.as_str()) {
                        Some("symbol") => match inner {
                            Value::String(name) => Value::Symbol(name),
                            other => other,
                        },
                        Some("keyword") => match inner {
                            Value::String(name) => Value::Keyword(name),
                            other => other,
                        },
                        Some("char") => match inner {
                            Value::String(name) => Value::Char(name.chars().next().unwrap_or('\0')),
                            other => other,
                        },
                        _ => inner,
                    };
                    return Ok(coerced);
                }
                self.entering(path)?;
                let built = if let Some(items) = map.get("items").and_then(|i| i.as_array()) {
                    let mut values = im::Vector::new();
                    for (index, item) in items.iter().enumerate() {
                        values.push_back(self.payload_value(item, &format!("{path}[{index}]"))?);
                    }
                    match map.get("seq").and_then(|s| s.as_str()) {
                        Some("vector") => Value::Vector(values),
                        _ => Value::List(values),
                    }
                } else if let Some(entries) = map.get("entries").and_then(|e| e.as_array()) {
                    let mut map = im::HashMap::new();
                    for (index, pair) in entries.iter().enumerate() {
                        let Some(pair) = pair.as_array() else {
                            return Err(EvalError::invalid_form(format!(
                                "malformed map entry at {path}[{index}]"
                            )));
                        };
                        let key = self.payload_value(
                            pair.first().unwrap_or(&serde_json::Value::Null),
                            &format!("{path}[{index}].key"),
                        )?;
                        let value = self.payload_value(
                            pair.get(1).unwrap_or(&serde_json::Value::Null),
                            &format!("{path}[{index}].value"),
                        )?;
                        map.insert(key, value);
                    }
                    Value::Map(map)
                } else {
                    return Err(EvalError::invalid_form(format!(
                        "unrecognized inline payload at {path}"
                    )));
                };
                self.depth -= 1;
                Ok(built)
            }
            serde_json::Value::Array(_) => Err(EvalError::invalid_form(format!(
                "unexpected bare array payload at {path}"
            ))),
        }
    }

    /// Build the value for node `id`. Environments and cells reserve
    /// before filling; everything else builds after its children.
    fn value(&mut self, id: usize) -> Result<Value, EvalError> {
        if let Some(built) = self.values.get(&id) {
            return Ok(built.clone());
        }
        if !self.building.insert(id) {
            return Err(EvalError::invalid_form(format!(
                "value cycle through node {id}; only environments can close cycles"
            )));
        }
        let node = self.node(id)?.clone();
        let path = format!("nodes[{id}]");
        self.entering(&path)?;
        let built = match node.kind.as_str() {
            "nil" => Value::Nil,
            "boolean" => Value::Boolean(node.payload["value"].as_bool().unwrap_or(false)),
            "integer" => Value::Integer(node.payload["value"].as_i64().unwrap_or_default()),
            "float" => Value::Float(node.payload["value"].as_f64().unwrap_or_default()),
            "string" => Value::String(node.payload["value"].as_str().unwrap_or_default().into()),
            "symbol" => Value::Symbol(node.payload["value"].as_str().unwrap_or_default().into()),
            "keyword" => Value::Keyword(node.payload["value"].as_str().unwrap_or_default().into()),
            "char" => Value::Char(
                node.payload["value"]
                    .as_str()
                    .and_then(|s| s.chars().next())
                    .unwrap_or('\0'),
            ),
            "list" | "vector" => {
                let mut values = im::Vector::new();
                if let Some(items) = node.payload["items"].as_array() {
                    for (index, item) in items.iter().enumerate() {
                        values.push_back(self.payload_value(item, &format!("{path}[{index}]"))?);
                    }
                }
                if node.kind == "vector" {
                    Value::Vector(values)
                } else {
                    Value::List(values)
                }
            }
            "map" => {
                let mut entries = im::HashMap::new();
                if let Some(pairs) = node.payload["entries"].as_array() {
                    for (index, pair) in pairs.iter().enumerate() {
                        let Some(pair) = pair.as_array() else {
                            continue;
                        };
                        let key = self.payload_value(
                            pair.first().unwrap_or(&serde_json::Value::Null),
                            &format!("{path}[{index}].key"),
                        )?;
                        let value = self.payload_value(
                            pair.get(1).unwrap_or(&serde_json::Value::Null),
                            &format!("{path}[{index}].value"),
                        )?;
                        entries.insert(key, value);
                    }
                }
                Value::Map(entries)
            }
            "function" => self.function(id, &path)?,
            "macro" => self.macro_(id, &path)?,
            "native" => {
                let name = node.payload["name"].as_str().unwrap_or_default();
                match self.ctx.env.get(name) {
                    Some(Value::NativeFunction(native)) => Value::NativeFunction(native),
                    _ => {
                        return Err(EvalError::invalid_form(format!(
                            "missing native binding {name:?} at {path}"
                        )));
                    }
                }
            }
            "buffer" => {
                let bytes = node.payload["bytes"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|b| b.as_u64().map(|b| b as u8))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Value::Buffer(Arc::new(parking_lot::Mutex::new(bytes)))
            }
            "instance" => self.instance(&node, &path)?,
            "generic-function" => self.generic_function(&node, &path)?,
            other => {
                return Err(EvalError::invalid_form(format!(
                    "cannot build value for node kind {other:?} at {path}"
                )));
            }
        };
        self.building.remove(&id);
        self.values.insert(id, built.clone());
        self.depth -= 1;
        Ok(built)
    }

    /// Build an environment node: parent chains are acyclic, so the chain
    /// resolves now; bindings link in the final phase. Reservation before
    /// return is what closes env ↔ closure cycles on the same Arc.
    fn env(&mut self, id: usize) -> Result<Arc<Env>, EvalError> {
        if let Some(built) = self.envs.get(&id) {
            return Ok(built.clone());
        }
        let node = self.node(id)?.clone();
        if node.kind != "env" {
            return Err(EvalError::invalid_form(format!(
                "node {id} is {}, expected an environment",
                node.kind
            )));
        }
        let path = format!("nodes[{id}]");
        let parent = match node.payload.get("parent") {
            Some(parent) => {
                let parent_id = Self::ref_id(parent, &format!("{path}.parent"))?;
                Some(self.env(parent_id)?)
            }
            None => None,
        };
        let env = Arc::new(Env::new(parent));
        self.envs.insert(id, env.clone());
        self.pending_envs.push(id);
        Ok(env)
    }

    fn fill_env(&mut self, id: usize) -> Result<(), EvalError> {
        let node = self.node(id)?.clone();
        let path = format!("nodes[{id}]");
        let env = self.envs[&id].clone();
        let bindings: Vec<(String, serde_json::Value)> = node.payload["bindings"]
            .as_object()
            .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        for (name, encoded) in bindings {
            let value = self.payload_value(&encoded, &format!("{path}.{name}"))?;
            env.set(name, value);
        }
        Ok(())
    }

    fn function(&mut self, id: usize, path: &str) -> Result<Value, EvalError> {
        let node = self.node(id)?.clone();
        let function = if let Some(module_field) = node.payload.get("module") {
            let module_id = Self::ref_id(module_field, &format!("{path}.module"))?;
            let module = self.module(module_id)?;
            let name = node.payload["function"].as_str().map(str::to_string);
            let index = match name {
                Some(name) => module
                    .functions
                    .iter()
                    .position(|record| record.name.as_deref() == Some(name.as_str()))
                    .ok_or_else(|| {
                        EvalError::invalid_form(format!(
                            "module artifact has no function named {name:?} at {path}"
                        ))
                    })?,
                None => {
                    return Err(EvalError::invalid_form(format!(
                        "compiled function without a recorded name cannot be restored at {path}"
                    )));
                }
            };
            let mut captures = im::Vector::new();
            if let Some(items) = node.payload["captures"].as_array() {
                for (cell_index, cell) in items.iter().enumerate() {
                    let cell_id = Self::ref_id(cell, &format!("{path}.captures[{cell_index}]"))?;
                    captures
                        .push_back(self.cell(cell_id, &format!("{path}.captures[{cell_index}]"))?);
                }
            }
            Function {
                params: params_of(&node),
                rest_param: rest_of(&node),
                body: crate::sexp::Sexp::Nil,
                env: Arc::new(Env::new(None)),
                compiled: Some(Arc::new(Closure {
                    module,
                    fn_index: index,
                    captures,
                })),
            }
        } else {
            let body = self.payload_value(
                node.payload.get("body").unwrap_or(&serde_json::Value::Null),
                &format!("{path}.body"),
            )?;
            let env_id = Self::ref_id(&node.payload["env"], &format!("{path}.env"))?;
            Function {
                params: params_of(&node),
                rest_param: rest_of(&node),
                body: value_to_sexp(&body)?,
                env: self.env(env_id)?,
                compiled: None,
            }
        };
        Ok(Value::Function(Arc::new(function)))
    }

    fn macro_(&mut self, id: usize, path: &str) -> Result<Value, EvalError> {
        let node = self.node(id)?.clone();
        let env_id = Self::ref_id(&node.payload["env"], &format!("{path}.env"))?;
        let body = self.payload_value(
            node.payload.get("body").unwrap_or(&serde_json::Value::Null),
            &format!("{path}.body"),
        )?;
        Ok(Value::Macro(Arc::new(Macro {
            name: node.payload["name"].as_str().unwrap_or_default().into(),
            params: params_of(&node),
            rest_param: rest_of(&node),
            body: value_to_sexp(&body)?,
            env: self.env(env_id)?,
            compiled: None,
        })))
    }

    fn cell(&mut self, id: usize, _path: &str) -> Result<Rc<RefCell<BindingCell>>, EvalError> {
        if let Some(built) = self.cells.get(&id) {
            return Ok(built.clone());
        }
        let reserved = Rc::new(RefCell::new(BindingCell::Bound(Value::Nil)));
        self.cells.insert(id, reserved.clone());
        self.pending_cells.push(id);
        Ok(reserved)
    }

    fn fill_cell(&mut self, id: usize) -> Result<(), EvalError> {
        let node = self.node(id)?.clone();
        let path = format!("nodes[{id}]");
        let cell = self.cells[&id].clone();
        let value = self.payload_value(
            node.payload
                .get("value")
                .unwrap_or(&serde_json::Value::Null),
            &format!("{path}.value"),
        )?;
        *cell.borrow_mut() = BindingCell::Bound(value);
        Ok(())
    }

    fn module(&mut self, id: usize) -> Result<Arc<ModuleArtifact>, EvalError> {
        if let Some(built) = self.modules.get(&id) {
            return Ok(built.clone());
        }
        let node = self.node(id)?.clone();
        let json = node.payload["json"].as_str().ok_or_else(|| {
            EvalError::invalid_form(format!("module node {id} has no artifact json"))
        })?;
        let value = crate::bytecode::deserialize_module(json)?;
        let artifact = Arc::new(crate::bytecode::value_to_artifact(&value)?);
        self.modules.insert(id, artifact.clone());
        Ok(artifact)
    }

    fn class(&mut self, id: usize) -> Result<Arc<Class>, EvalError> {
        if let Some(built) = self.classes.get(&id) {
            return Ok(built.clone());
        }
        let node = self.node(id)?.clone();
        let path = format!("nodes[{id}]");
        let mut superclasses = Vec::new();
        if let Some(parents) = node.payload["superclasses"].as_array() {
            for (index, parent) in parents.iter().enumerate() {
                let parent_id = Self::ref_id(parent, &format!("{path}.superclasses[{index}]"))?;
                superclasses.push(self.class(parent_id)?);
            }
        }
        let mut slots = Vec::new();
        if let Some(defs) = node.payload["slots"].as_array() {
            for def in defs {
                let initform = match def.get("initform") {
                    Some(form) if !form.is_null() => {
                        Some(self.payload_value(form, &format!("{path}.slots.initform"))?)
                    }
                    _ => None,
                };
                slots.push(SlotDefinition {
                    name: def["name"].as_str().unwrap_or_default().into(),
                    initargs: def["initargs"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    initform,
                    accessor: def["accessor"].as_str().map(str::to_string),
                });
            }
        }
        let class = Arc::new(Class {
            name: node.payload["name"].as_str().unwrap_or_default().into(),
            superclasses,
            slots,
            cpl: node.payload["cpl"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        });
        self.classes.insert(id, class.clone());
        Ok(class)
    }

    fn instance(&mut self, node: &Node, path: &str) -> Result<Value, EvalError> {
        let class_id = Self::ref_id(&node.payload["class"], &format!("{path}.class"))?;
        let class = self.class(class_id)?;
        let mut instance = crate::value::ZosInstance::new(class);
        instance.header.identity = node.payload["identity"].as_u64();
        instance.header.flags =
            ObjectFlags::from_bits(node.payload["flags"].as_u64().unwrap_or(0) as u8);
        if let Some(slots) = node.payload["slots"].as_object() {
            for (name, encoded) in slots {
                let value = self.payload_value(encoded, &format!("{path}.{name}"))?;
                instance.slots.insert(name.clone(), value);
            }
        }
        Ok(Value::Object(Box::new(instance)))
    }

    fn generic_function(&mut self, node: &Node, path: &str) -> Result<Value, EvalError> {
        let class_id = Self::ref_id(&node.payload["class"], &format!("{path}.class"))?;
        let class = self.class(class_id)?;
        let mut gf = GenericFunction::new(
            node.payload["name"].as_str().unwrap_or_default().into(),
            node.payload["lambda_list"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        );
        if let Some(methods) = node.payload["methods"].as_array() {
            for method in methods {
                let body_id = Self::ref_id(&method["body"], &format!("{path}.methods.body"))?;
                let body = match self.value(body_id)? {
                    Value::Function(body) => body,
                    other => {
                        return Err(EvalError::invalid_form(format!(
                            "method body is {}, expected a function at {path}",
                            other.value_type()
                        )));
                    }
                };
                let specializers = method["specializers"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|s| s.as_str())
                            .map(|s| match s.strip_prefix("exact:") {
                                Some(name) => Specializer::Exact(name.to_string()),
                                None => Specializer::T,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let qualifier = match method["qualifier"].as_str() {
                    Some("before") => MethodQualifier::Before,
                    Some("after") => MethodQualifier::After,
                    Some("around") => MethodQualifier::Around,
                    _ => MethodQualifier::Primary,
                };
                gf.add_method(Arc::new(Method {
                    specializers,
                    qualifier,
                    body,
                }));
            }
        }
        Ok(Value::Object(Box::new(GFObject {
            header: crate::zos::object::ObjectHeader {
                class,
                flags: ObjectFlags::NONE,
                identity: None,
            },
            gf: Arc::new(RefCell::new(gf)),
        })))
    }
}

fn params_of(node: &Node) -> im::Vector<String> {
    node.payload["params"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn rest_of(node: &Node) -> Option<String> {
    node.payload["rest"].as_str().map(str::to_string)
}
