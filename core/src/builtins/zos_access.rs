//! ZOS object access and reflection builtins.
//!
//! These go through the `zos` layer's public helpers; the only GF
//! downcast lives in `zos::apply`.

use std::sync::Arc;

use im::Vector;
use im::vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

// ── ZOS Object Operations ─────────────────────────────────────────

/// (slot-value instance slot-name) → value
pub fn slot_value_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let instance = match &args[0] {
        Value::Object(o) => o,
        other => return Err(EvalError::type_error("object", other.value_type())),
    };
    let slot_name = match &args[1] {
        Value::Keyword(k) => k.clone(),
        Value::Symbol(s) => s.clone(),
        other => return Err(EvalError::type_error("keyword or symbol", other.value_type())),
    };
    if let Some(inst) = instance.as_any().downcast_ref::<crate::value::ZosInstance>() {
        inst.slots.get(&slot_name).cloned().ok_or_else(|| {
            EvalError::custom(format!("slot not found: {slot_name}"))
        })
    } else {
        Err(EvalError::type_error("ZosInstance", "non-instance object"))
    }
}

/// (make-instance class-name :slot1 val1 :slot2 val2 ...) → object
pub fn make_instance_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.is_empty() || args.len() % 2 != 1 {
        return Err(EvalError::wrong_arg_count_min(1, args.len()));
    }

    // First arg is a class (as Value::Object from defclass, or looked up symbol)
    let class_ref = match &args[0] {
        Value::Object(o) => o.header().class.clone(),
        Value::Symbol(s) => {
            let class_val = engine.env().get(s).ok_or_else(|| {
                EvalError::custom(format!("class not found: {s}"))
            })?;
            match &class_val {
                Value::Object(o) => o.header().class.clone(),
                _ => return Err(EvalError::type_error("class object", class_val.value_type())),
            }
        }
        other => return Err(EvalError::type_error("class object", other.value_type())),
    };

    let mut instance = crate::value::ZosInstance::new(class_ref.clone());

    // Process initargs
    let mut i = 1;
    while i < args.len() {
        let key = match &args[i] {
            Value::Keyword(k) => k.clone(),
            Value::Symbol(s) => s.clone(),
            other => return Err(EvalError::type_error("keyword or symbol", other.value_type())),
        };
        i += 1;
        if i < args.len() {
            instance.slots.insert(key, args[i].clone());
            i += 1;
        }
    }

    Ok(Value::Object(Box::new(instance)))
}

// ── ZOS Reflection API ─────────────────────────────────────────

/// (class-of obj) → symbol
pub fn class_of_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let class_name = match &args[0] {
        Value::Object(o) => o.header().class.name.clone(),
        Value::Nil => "Nil".into(),
        Value::Integer(_) => "Integer".into(),
        Value::Float(_) => "Float".into(),
        Value::Boolean(_) => "Boolean".into(),
        Value::String(_) => "String".into(),
        Value::Symbol(_) => "Symbol".into(),
        Value::Keyword(_) => "Keyword".into(),
        Value::List(_) => "List".into(),
        Value::Vector(_) => "Vector".into(),
        Value::Map(_) => "Map".into(),
        Value::Function(_) => "Function".into(),
        Value::NativeFunction(_) => "NativeFunction".into(),
        Value::Macro(_) => "Macro".into(),
        Value::Char(_) => "Character".into(),
        Value::Buffer(_) => "Buffer".into(),
        Value::Future(_) => "Future".into(),
        Value::Channel(_) => "Channel".into(),
    };
    Ok(Value::Symbol(class_name))
}

/// (class-name class) → symbol
pub fn class_name_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let class_ref = value_to_class_ref(&args[0])?;
    Ok(Value::Symbol(class_ref.name.clone()))
}
/// (class-direct-superclasses class) → list
pub fn class_direct_superclasses_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let class_ref = value_to_class_ref(&args[0])?;
    let supers: im::Vector<Value> = class_ref.superclasses.iter()
        .map(|c| Value::Symbol(c.name.clone()))
        .collect();
    Ok(Value::List(supers))
}

/// (class-precedence-list class) → list of symbols
pub fn class_precedence_list_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let class_ref = value_to_class_ref(&args[0])?;
    let cpl: im::Vector<Value> = class_ref.cpl.iter()
        .map(|name| Value::Symbol(name.clone()))
        .collect();
    Ok(Value::List(cpl))
}

/// (class-slots class) → list of slot name symbols
pub fn class_slots_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let class_ref = value_to_class_ref(&args[0])?;
    let slots: im::Vector<Value> = class_ref.slots.iter()
        .map(|s| Value::Symbol(s.name.clone()))
        .collect();
    Ok(Value::List(slots))
}

/// (generic-function-name gf) → symbol
pub fn gf_name_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Object(o) => {
            if let Some(gf) = crate::zos::gf::gf_shared(o.as_ref()) {
                let gf = gf.borrow();
                Ok(Value::Symbol(gf.name.clone()))
            } else {
                Err(EvalError::type_error("GenericFunction", "non-GF object"))
            }
        }
        other => Err(EvalError::type_error("GenericFunction", other.value_type())),
    }
}

/// (generic-function-methods gf) → list of method descriptions
pub fn gf_methods_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Object(o) => {
            if let Some(gf) = crate::zos::gf::gf_shared(o.as_ref()) {
                let gf = gf.borrow();
                let mut methods: im::Vector<Value> = im::Vector::new();
                for m in &gf.methods {
                    let specializers: im::Vector<Value> = m.specializers.iter()
                        .map(|s| match s {
                            crate::zos::gf::Specializer::T => Value::Keyword("t".into()),
                            crate::zos::gf::Specializer::Exact(name) => Value::Symbol(name.clone()),
                        })
                        .collect();
                    let qual = match m.qualifier {
                        crate::zos::gf::MethodQualifier::Primary => Value::Keyword("primary".into()),
                        crate::zos::gf::MethodQualifier::Before => Value::Keyword("before".into()),
                        crate::zos::gf::MethodQualifier::After => Value::Keyword("after".into()),
                        crate::zos::gf::MethodQualifier::Around => Value::Keyword("around".into()),
                    };
                    let method_desc: im::Vector<Value> = vector![
                        Value::List(specializers),
                        qual,
                    ];
                    methods.push_back(Value::List(method_desc));
                }
                Ok(Value::List(methods))
            } else {
                Err(EvalError::type_error("GenericFunction", "non-GF object"))
            }
        }
        other => Err(EvalError::type_error("GenericFunction", other.value_type())),
    }
}

// Helper: extract ClassRef from a Value (class object or instance)
fn value_to_class_ref(v: &Value) -> Result<crate::zos::object::ClassRef, EvalError> {
    match v {
        Value::Object(o) => Ok(o.header().class.clone()),
        other => Err(EvalError::type_error("class object", other.value_type())),
    }
}

pub fn register(env: &Arc<Env>) {
    env.set("slot-value".into(), Value::NativeFunction(NativeFn::new("slot-value", slot_value_fn)));
    env.set("make-instance".into(), Value::NativeFunction(NativeFn::new("make-instance", make_instance_fn)));
    env.set("class-of".into(), Value::NativeFunction(NativeFn::new("class-of", class_of_fn)));
    env.set("class-name".into(), Value::NativeFunction(NativeFn::new("class-name", class_name_fn)));
    env.set("class-direct-superclasses".into(), Value::NativeFunction(NativeFn::new("class-direct-superclasses", class_direct_superclasses_fn)));
    env.set("class-precedence-list".into(), Value::NativeFunction(NativeFn::new("class-precedence-list", class_precedence_list_fn)));
    env.set("class-slots".into(), Value::NativeFunction(NativeFn::new("class-slots", class_slots_fn)));
    env.set("slot-definitions".into(), Value::NativeFunction(NativeFn::new("slot-definitions", class_slots_fn)));
    env.set("generic-function-name".into(), Value::NativeFunction(NativeFn::new("generic-function-name", gf_name_fn)));
    env.set("generic-function-methods".into(), Value::NativeFunction(NativeFn::new("generic-function-methods", gf_methods_fn)));
}
