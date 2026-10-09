use zio_core::context::EvalEngine;
use zio_core::error::EvalError;
use zio_core::im::{HashMap, Vector};
use zio_core::special::TailResult;
use zio_core::value::Value;

pub fn arity(args: &Vector<Value>, expected: usize) -> Result<(), EvalError> {
    if std::env::var_os("ZIO_PROBE_ARITY").is_some() && args.len() != expected {
        eprintln!(
            "[arity] expected {expected} got {} args={args:?}",
            args.len()
        );
    }
    if args.len() == expected {
        Ok(())
    } else {
        Err(EvalError::wrong_arg_count(expected, args.len()))
    }
}

pub fn string(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::String(text) => Ok(text),
        other => Err(EvalError::type_error("string", other.value_type())),
    }
}

pub fn integer(value: &Value) -> Result<i64, EvalError> {
    match value {
        Value::Integer(number) => Ok(*number),
        other => Err(EvalError::type_error("integer", other.value_type())),
    }
}

pub fn get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Map(map) => map
            .get(&Value::Keyword(key.into()))
            .or_else(|| map.get(&Value::String(key.into()))),
        _ => None,
    }
}

pub fn map<'a>(entries: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    let mut map = HashMap::new();
    for (key, value) in entries {
        map.insert(Value::Keyword(key.into()), value);
    }
    Value::Map(map)
}

pub fn invoke(
    mut function: Value,
    mut args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    loop {
        match zio_core::eval::apply(function, args, engine)? {
            TailResult::Value(value) => return Ok(value),
            TailResult::TailCall(next, arguments) => {
                function = next;
                args = arguments;
            }
            TailResult::Recur(_) => {
                return Err(EvalError::invalid_form("recur escaped a host callback"));
            }
        }
    }
}

pub fn handle(
    value: &Value,
    kind: &str,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    match value {
        Value::NativeFunction(function) if function.name() == kind => function.call(args, engine),
        _ => Err(EvalError::custom(format!(
            "invalid-input: expected opaque {kind} resource"
        ))),
    }
}

pub fn with_bytes<T>(value: &Value, operation: impl FnOnce(&[u8]) -> T) -> Result<T, EvalError> {
    match value {
        Value::Buffer(buffer) => Ok(operation(&buffer.lock())),
        Value::String(text) => Ok(operation(text.as_bytes())),
        Value::Vector(items) | Value::List(items) => {
            let mut bytes = Vec::with_capacity(items.len());
            for item in items {
                let number = integer(item)?;
                bytes.push(
                    u8::try_from(number)
                        .map_err(|_| EvalError::custom("invalid-input: byte must be in 0..255"))?,
                );
            }
            Ok(operation(&bytes))
        }
        other => Err(EvalError::type_error(
            "buffer, string or byte sequence",
            other.value_type(),
        )),
    }
}

pub fn to_json(value: &Value) -> Result<serde_json::Value, EvalError> {
    zio_core::builtins::json::to_json(value)
}

pub fn from_json(value: serde_json::Value) -> Result<Value, EvalError> {
    zio_core::builtins::json::from_json(value)
}
