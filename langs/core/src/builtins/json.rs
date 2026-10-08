//! Strict, deterministic JSON conversion. Object keys normalize strings and keywords only.
use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};
use im::Vector;
use std::sync::Arc;

pub fn to_json(value: &Value) -> Result<serde_json::Value, EvalError> {
    use serde_json::Value as Json;
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Boolean(v) => Json::Bool(*v),
        Value::Integer(v) => Json::Number((*v).into()),
        Value::Float(v) => Json::Number(
            serde_json::Number::from_f64(*v)
                .ok_or_else(|| EvalError::custom("JSON requires finite numbers"))?,
        ),
        Value::String(v) => Json::String(v.clone()),
        Value::List(items) | Value::Vector(items) => {
            Json::Array(items.iter().map(to_json).collect::<Result<_, _>>()?)
        }
        Value::Map(items) => {
            // Sort explicitly even when a consumer enables
            // serde_json/preserve_order.
            let mut ordered = std::collections::BTreeMap::new();
            for (key, value) in items {
                let key = match key {
                    Value::String(s) | Value::Keyword(s) => s.clone(),
                    other => {
                        return Err(EvalError::custom(format!(
                            "unsupported JSON object key: {}",
                            other.value_type()
                        )));
                    }
                };
                // `$` introduces the tag for a keyword or symbol in
                // *value* position. A key using it would decode back as
                // a tagged value rather than as a member of this object,
                // so it is refused where the ambiguity would start.
                if key.starts_with('$') {
                    return Err(EvalError::custom(format!(
                        "reserved JSON object key prefix: {key}"
                    )));
                }
                // A keyword or symbol sitting in value position has no
                // JSON counterpart that keeps its kind, so it is
                // tagged. A keyword as a *key* needs no tag: the key
                // position is unambiguous and every existing consumer
                // already reads it as a string.
                let encoded = match value {
                    Value::Keyword(name) => tagged("$keyword", name),
                    Value::Symbol(name) => tagged("$symbol", name),
                    other => to_json(other)?,
                };
                if ordered.insert(key.clone(), encoded).is_some() {
                    return Err(EvalError::custom(format!(
                        "colliding normalized JSON object key: {key}"
                    )));
                }
            }
            Json::Object(ordered.into_iter().collect())
        }
        // In array position a keyword is unambiguous — there is no key
        // it could be confused with — so it encodes as its plain string.
        // Tagging it would turn `[:teacher-label]` into
        // `[{"$keyword":"teacher-label"}]` and break every consumer that
        // already reads such a list.
        Value::Keyword(name) => Json::String(name.clone()),
        Value::Symbol(name) => Json::String(name.clone()),
        other => {
            return Err(EvalError::custom(format!(
                "unsupported JSON value: {}",
                other.value_type()
            )));
        }
    })
}

/// The tagged form of a keyword or symbol in JSON value position.
fn tagged(tag: &str, name: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(tag.to_string(), serde_json::Value::String(name.to_string()));
    serde_json::Value::Object(map)
}

pub fn from_json(value: serde_json::Value) -> Result<Value, EvalError> {
    use serde_json::Value as Json;
    Ok(match value {
        Json::Null => Value::Nil,
        Json::Bool(v) => Value::Boolean(v),
        Json::Number(v) => {
            if let Some(n) = v.as_i64() {
                Value::Integer(n)
            } else if v.is_u64() {
                return Err(EvalError::custom(
                    "JSON integer exceeds signed 64-bit range",
                ));
            } else {
                Value::Float(
                    v.as_f64()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| EvalError::custom("JSON requires finite numbers"))?,
                )
            }
        }
        Json::String(v) => Value::String(v),
        Json::Array(items) => {
            Value::Vector(items.into_iter().map(from_json).collect::<Result<_, _>>()?)
        }
        Json::Object(items) => {
            // Reverse the tagged encoding. An object with exactly one
            // `$`-prefixed key is a tagged keyword or symbol; a real
            // object never has one, because `to_json` refuses such a key.
            if items.len() == 1 {
                if let Some(Json::String(name)) = items.get("$keyword") {
                    return Ok(Value::Keyword(name.clone()));
                }
                if let Some(Json::String(name)) = items.get("$symbol") {
                    return Ok(Value::Symbol(name.clone()));
                }
            }
            Value::Map(
                items
                    .into_iter()
                    .map(|(k, v)| Ok((Value::String(k), from_json(v)?)))
                    .collect::<Result<_, EvalError>>()?,
            )
        }
    })
}

pub fn json_stringify_fn(
    args: Vector<Value>,
    _engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    serde_json::to_string(&to_json(&args[0])?)
        .map(Value::String)
        .map_err(|e| EvalError::custom(format!("JSON encoding error: {e}")))
}

pub fn json_parse_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let Value::String(source) = &args[0] else {
        return Err(EvalError::type_error("string", args[0].value_type()));
    };
    let json = serde_json::from_str(source)
        .map_err(|e| EvalError::custom(format!("JSON parse error: {e}")))?;
    from_json(json)
}

pub fn register(env: &Arc<Env>) {
    env.set(
        "json-stringify".into(),
        Value::NativeFunction(NativeFn::new("json-stringify", json_stringify_fn)),
    );
    env.set(
        "json-parse".into(),
        Value::NativeFunction(NativeFn::new("json-parse", json_parse_fn)),
    );
}
