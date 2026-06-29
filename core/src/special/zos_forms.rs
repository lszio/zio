use std::sync::Arc;
use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::Value;
use crate::zos::object::{Class, ClassRef, ObjectFlags, SlotDefinition};
use crate::zos::class;
use crate::zos::gf::{GenericFunction, Method, MethodQualifier, Specializer};
use crate::value::Function;

/// A ZosObject wrapper for GenericFunction so it can be stored as Value::Object.
#[derive(Debug, Clone)]
pub struct GFObject {
    pub header: crate::zos::object::ObjectHeader,
    pub gf: Arc<std::cell::RefCell<GenericFunction>>,
}

impl crate::zos::object::ZosObject for GFObject {
    fn header(&self) -> &crate::zos::object::ObjectHeader {
        &self.header
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn clone_box(&self) -> Box<dyn crate::zos::object::ZosObject> {
        Box::new(self.clone())
    }
}

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
/// (defgeneric name (params...))
/// Declare a generic function.
pub fn do_defgeneric(
    args: &[Sexp],
    env: &Arc<Env>,
    _engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::wrong_arg_count_min(2, args.len()));
    }

    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::type_error("symbol", format!("gf name: {}", other.kind()))),
    };

    // Parse lambda list: (shape) or (a b c)
    let lambda_list = match &args[1] {
        Sexp::List(l, _) | Sexp::Vector(l, _) => {
            l.iter().map(|item| match item {
                Sexp::Symbol(s, _) => s.clone(),
                _ => "<param>".into(),
            }).collect()
        }
        other => return Err(EvalError::type_error("list", format!("lambda list: {}", other.kind()))),
    };

    let gf = GenericFunction::new(name.clone(), lambda_list);
    let header = crate::zos::object::ObjectHeader {
        class: Arc::new(crate::zos::object::Class {
            name: "GenericFunction".into(),
            superclass: None,
            slots: Vec::new(),
        }),
        flags: ObjectFlags::MUTABLE,
        identity: None,
    };

    let gf_obj = GFObject {
        header,
        gf: Arc::new(std::cell::RefCell::new(gf)),
    };

    env.set(name, Value::Object(Box::new(gf_obj)));
    Ok(TailResult::Value(Value::Nil))
}

/// (defmethod name (specializer ...) body)
/// (defmethod name :qualifier (specializer ...) body)
/// Add a method to a generic function.
pub fn do_defmethod(
    args: &[Sexp],
    env: &Arc<Env>,
    _engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::wrong_arg_count_min(3, args.len()));
    }

    let gf_name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::type_error("symbol", format!("gf name: {}", other.kind()))),
    };

    // Check for qualifier after gf name
    let mut arg_idx = 1;
    let qualifier = match &args[arg_idx] {
        Sexp::Keyword(k, _) if k == "before" || k == ":before" => {
            arg_idx = 2;
            MethodQualifier::Before
        }
        Sexp::Keyword(k, _) if k == "after" || k == ":after" => {
            arg_idx = 2;
            MethodQualifier::After
        }
        Sexp::Keyword(k, _) if k == "around" || k == ":around" => {
            arg_idx = 2;
            MethodQualifier::Around
        }
        _ => MethodQualifier::Primary,
    };

    // Parse specializers: (shape) or ((name type) ...)
    let specializers_sexp = &args[arg_idx];
    arg_idx += 1;
    let specializer_list = match specializers_sexp {
        Sexp::List(l, _) | Sexp::Vector(l, _) => l,
        other => return Err(EvalError::type_error("list", format!("specializers: {}", other.kind()))),
    };

    let mut specializers = Vec::new();
    let mut param_names = Vec::new();
    for item in specializer_list {
        match item {
            Sexp::Symbol(s, _) => {
                // Bare symbol: (name) — accepts any type (T specializer)
                specializers.push(Specializer::T);
                param_names.push(s.clone());
            }
            Sexp::List(inner, _) if inner.len() == 2 => {
                // ((name TypeName))
                if let Sexp::Symbol(name_s, _) = &inner[0] {
                    if let Sexp::Symbol(type_s, _) = &inner[1] {
                        specializers.push(Specializer::Exact(type_s.clone()));
                        param_names.push(name_s.clone());
                    }
                }
            }
            _ => return Err(EvalError::invalid_form(
                format!("invalid specializer: {item}"),
            )),
        }
    }

    // Parse body
    let body_exprs = &args[arg_idx..];
    let body = if body_exprs.len() == 1 {
        body_exprs[0].clone()
    } else {
        Sexp::List(body_exprs.iter().cloned().collect(), None)
    };

    // Create a function value for the method body
    let function = Arc::new(Function {
        params: param_names.into_iter().collect(),
        rest_param: None,
        body,
        env: env.clone(),
    });

    let method = Arc::new(Method {
        specializers,
        qualifier,
        body: function,
    });

    // Look up the GF and add the method
    let gf_val = env.get(&gf_name).ok_or_else(|| {
        EvalError::custom(format!("generic function not found: {gf_name}"))
    })?;

    match &gf_val {
        Value::Object(o) => {
            if let Some(gf_obj) = o.as_any().downcast_ref::<GFObject>() {
                gf_obj.gf.borrow_mut().add_method(method);
                Ok(TailResult::Value(Value::Nil))
            } else {
                Err(EvalError::type_error("GenericFunction", "non-GF object"))
            }
        }
        _ => Err(EvalError::type_error("GenericFunction", gf_val.value_type())),
    }
}
/// (call-next-method) → value
/// Calls the next method in the method combination chain.
/// Must be called from within a method body bound to a GF dispatch.
pub fn do_call_next_method(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    match env.get("*next-method*") {
        Some(Value::NativeFunction(nf)) => {
            let result = nf.call(im::Vector::new(), engine)?;
            Ok(TailResult::Value(result))
        }
        Some(_) => Err(EvalError::custom("*next-method* is not a function")),
        None => Err(EvalError::custom("no next method available (call-next-method outside method)")),
    }
}
