use std::sync::Arc;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::Value;
use crate::zos::object::{Class, ClassRef, ObjectFlags, SlotDefinition};
use crate::zos::class;

/// (defclass name superclass slots)
/// superclass: a symbol or nil (for root classes)
/// slots: ((slot-name :initarg :key :accessor fn-name) ...)
pub fn do_defclass(
    args: &[Sexp],
    env: &Arc<Env>,
    _engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::wrong_arg_count_min(3, args.len()));
    }

    let class_name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::type_error("symbol", format!("class name: {}", other.kind()))),
    };

    // Parse superclass (nil or symbol)
    let superclass: Option<ClassRef> = match &args[1] {
        Sexp::Nil => None,
        Sexp::Symbol(s, _) => {
            // Look up in env — if a class was previously defined with defclass,
            // it would be stored in the env as a Value::Object wrapping the class.
            // For now, only built-in classes are available.
            if let Some(Value::Object(o)) = env.get(s) {
                // The class object itself
                if let Some(cls) = o.as_any().downcast_ref::<crate::value::ZosInstance>() {
                    // For classes, we store them specially
                }
            }
            // Fallback: check built-in class registry
            None // Simplified for now
        }
        other => return Err(EvalError::type_error("symbol or nil", format!("superclass: {}", other.kind()))),
    };

    // Parse slot definitions
    let slots_sexp = &args[2];
    let slot_list = match slots_sexp {
        Sexp::List(l, _) | Sexp::Vector(l, _) => l,
        other => return Err(EvalError::type_error("list or vector", format!("slots: {}", other.kind()))),
    };

    let mut slots = Vec::new();
    for slot_sexp in slot_list {
        let slot_items = match slot_sexp {
            Sexp::List(l, _) => l,
            other => return Err(EvalError::invalid_form(
                format!("each slot definition must be a list, got {}", other.kind()),
            )),
        };
        if slot_items.is_empty() {
            return Err(EvalError::invalid_form("slot definition cannot be empty"));
        }

        let slot_name = match &slot_items[0] {
            Sexp::Symbol(s, _) => s.clone(),
            other => return Err(EvalError::type_error("symbol", format!("slot name: {}", other.kind()))),
        };

        // Parse :initarg, :accessor, etc.
        let mut initargs = Vec::new();
        let mut accessor = None;
        let mut i = 1;
        while i < slot_items.len() {
            match &slot_items[i] {
                Sexp::Keyword(k, _) if k == "initarg" || k == ":initarg" => {
                    i += 1;
                    if i < slot_items.len() {
                        if let Sexp::Keyword(k, _) = &slot_items[i] {
                            initargs.push(k.clone());
                        } else if let Sexp::Symbol(s, _) = &slot_items[i] {
                            initargs.push(s.clone());
                        }
                    }
                    i += 1;
                }
                Sexp::Keyword(k, _) | Sexp::Symbol(k, _) if k == "accessor" || k == ":accessor" => {
                    i += 1;
                    if i < slot_items.len() {
                        if let Sexp::Symbol(s, _) = &slot_items[i] {
                            accessor = Some(s.clone());
                        }
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }

        slots.push(SlotDefinition {
            name: slot_name,
            initargs,
            initform: None,
            accessor,
        });
    }

    // Create the class
    let class = Arc::new(Class {
        name: class_name.clone(),
        superclass,
        slots,
    });

    // Register in a global class registry stored in the env
    // For now, store as a ZosInstance wrapping the class
    let mut instance = crate::value::ZosInstance::new(class.clone());
    instance.header.flags = ObjectFlags::MUTABLE;
    instance.slots.insert("__class_name__".into(), Value::Symbol(class_name.clone()));

    env.set(class_name, Value::Object(Box::new(instance)));

    Ok(TailResult::Value(Value::Object(Box::new(
        crate::value::ZosInstance::new(class)
    ))))
}
