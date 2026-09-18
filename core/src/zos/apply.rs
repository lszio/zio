//! ZOS object application protocol.
//!
//! The eval loop never inspects ZOS heap object types directly: "is this
//! object callable, and what does calling it mean?" is delegated here, so
//! GF dispatch and method-combination semantics stay inside the ZOS layer.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::special::TailResult;
use crate::value::{NativeFn, Value};
use crate::zos::gf::{DispatchResult, GFObject, Method};
use crate::zos::object::{Class, ClassRef, ZosObject};

/// Apply a ZOS heap object to already-evaluated arguments.
/// Returns `None` when the object is not callable; the eval loop then
/// reports "not a function" itself.
pub fn try_apply(
    obj: &dyn ZosObject,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Option<Result<TailResult, EvalError>> {
    let gf_obj = obj.as_any().downcast_ref::<GFObject>()?;
    Some(apply_generic_function(gf_obj, args, engine))
}

fn apply_generic_function(
    gf_obj: &GFObject,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    let arg_classes: Vec<ClassRef> = args.iter().map(value_class_ref).collect();
    let (dispatch, gf_name) = {
        let mut gf = gf_obj.gf.borrow_mut();
        (gf.dispatch(&arg_classes), gf.name.clone())
    };

    // No applicable methods?
    if dispatch.primary.is_empty() && dispatch.around.is_empty() {
        return Err(EvalError::custom(format!(
            "no applicable method for {gf_name} with args {:?}",
            arg_classes.iter().map(|c| &c.name).collect::<Vec<_>>()
        )));
    }

    // Build the method combination chain from innermost to outermost.
    // The innermost step: execute :before → :primary → :after
    let primary_step = build_primary_combination(&dispatch, &args)?;

    // Wrap in :around methods (outermost → innermost)
    let chain = if dispatch.around.is_empty() {
        primary_step
    } else {
        build_around_wrappers(&dispatch.around, &args, primary_step)?
    };

    // chain is a NativeFn. Call it with empty args (args captured in closures)
    let result = chain.call(im::vector![], engine)?;
    Ok(TailResult::Value(result))
}

/// Build the primary combination: :before → :primary → :after
fn build_primary_combination(
    dispatch: &DispatchResult,
    args: &Vector<Value>,
) -> Result<NativeFn, EvalError> {
    let args = args.clone();
    let before = dispatch.before.clone();
    let primary = dispatch.primary.clone();
    let after = dispatch.after.clone();
    Ok(NativeFn::new("__primary_combination__", move |_: Vector<Value>, engine: &dyn EvalEngine| -> Result<Value, EvalError> {
        // Execute :before methods (most specific first)
        for m in &before {
            let env = bind_method_args(&m.body, &args)?;
            engine.eval_expr(&m.body.body, &env, false)?;
        }

        // Execute :primary methods (most specific first — take the first)
        if let Some(m) = primary.first() {
            let env = bind_method_args(&m.body, &args)?;
            let result = engine.eval_expr(&m.body.body, &env, false)?.into_value();

            // Execute :after methods (least specific first → reverse order)
            for m in after.iter().rev() {
                let env = bind_method_args(&m.body, &args)?;
                engine.eval_expr(&m.body.body, &env, false)?;
            }

            Ok(result)
        } else {
            Ok(Value::Nil)
        }
    }))
}

/// Wrap the primary combination in :around methods (outermost first).
/// Each :around method can call (call-next-method) to invoke the next in chain.
fn build_around_wrappers(
    around_methods: &[Arc<Method>],
    args: &Vector<Value>,
    inner: NativeFn,
) -> Result<NativeFn, EvalError> {
    let mut chain: NativeFn = inner;
    let args = args.clone();
    // Build from innermost to outermost
    for m in around_methods.iter().rev() {
        let prev_chain = chain.clone();
        let args = args.clone();
        let method = Arc::clone(m);

        chain = NativeFn::new("__around_method__", move |_: Vector<Value>, engine: &dyn EvalEngine| -> Result<Value, EvalError> {
            let env = bind_method_args(&method.body, &args)?;
            env.set("*next-method*".into(), Value::NativeFunction(prev_chain.clone()));
            engine.eval_expr(&method.body.body, &env, false).map(|r| r.into_value())
        });
    }

    Ok(chain)
}

/// Bind method parameters to the dispatched arguments in a child environment.
fn bind_method_args(func: &std::sync::Arc<crate::value::Function>, args: &Vector<Value>) -> Result<Arc<Env>, EvalError> {
    if func.rest_param.is_some() {
        Env::bind_variadic(&func.env, &func.params, &func.rest_param, args)
    } else {
        Env::bind(&func.env, &func.params, args)
    }
}

/// Get the class ref for a Value (used by GF dispatch).
pub fn value_class_ref(v: &Value) -> ClassRef {
    let name = value_class_name(v);
    Arc::new(Class {
        name: name.clone(),
        superclasses: Vec::new(),
        slots: Vec::new(),
        cpl: vec![name],
    })
}

/// Get the class name string for any Value.
fn value_class_name(v: &Value) -> String {
    match v {
        Value::Nil => "Nil".into(),
        Value::Boolean(_) => "Boolean".into(),
        Value::Integer(_) => "Integer".into(),
        Value::Float(_) => "Float".into(),
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
        Value::Object(o) => o.header().class.name.clone(),
        Value::Buffer(_) => "Buffer".into(),
        Value::Future(_) => "Future".into(),
        Value::Channel(_) => "Channel".into(),
    }
}
