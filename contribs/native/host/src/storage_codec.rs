//! Byte, fixed-width binary and JSON codecs. Deterministic output only:
//! `host/json-bytes` sorts object keys, `host/json-ordered` preserves the
//! caller's explicit order without a map round-trip.
use super::{bind, buffer, count};
use crate::values;
use std::collections::HashSet;
use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::value::Value;

fn kind(value: &Value) -> Result<&str, EvalError> {
    match value {
        Value::Keyword(name) | Value::String(name) => Ok(name),
        other => Err(EvalError::type_error(
            "binary kind keyword or string",
            other.value_type(),
        )),
    }
}

fn sequence<'a>(value: &'a Value, expected: &'static str) -> Result<&'a Vector<Value>, EvalError> {
    match value {
        Value::Vector(items) | Value::List(items) => Ok(items),
        other => Err(EvalError::type_error(expected, other.value_type())),
    }
}

fn number(value: &Value) -> Result<f64, EvalError> {
    match value {
        Value::Integer(v) => Ok(*v as f64),
        Value::Float(v) => Ok(*v),
        other => Err(EvalError::type_error("number", other.value_type())),
    }
}

fn out_of_range(kind_name: &str) -> EvalError {
    EvalError::custom(format!("invalid-input: value out of range for {kind_name}"))
}

fn unknown_kind(kind_name: &str) -> EvalError {
    EvalError::custom(format!("invalid-input: unknown binary kind: {kind_name}"))
}

fn binary_encode(kind_name: &str, value: &Value) -> Result<Vec<u8>, EvalError> {
    match kind_name {
        "u8" => u8::try_from(values::integer(value)?)
            .map(|v| vec![v])
            .map_err(|_| out_of_range(kind_name)),
        "u16-le" => u16::try_from(values::integer(value)?)
            .map(|v| v.to_le_bytes().to_vec())
            .map_err(|_| out_of_range(kind_name)),
        "u32-le" => u32::try_from(values::integer(value)?)
            .map(|v| v.to_le_bytes().to_vec())
            .map_err(|_| out_of_range(kind_name)),
        "u64-le" => u64::try_from(values::integer(value)?)
            .map(|v| v.to_le_bytes().to_vec())
            .map_err(|_| out_of_range(kind_name)),
        "i32-le" => i32::try_from(values::integer(value)?)
            .map(|v| v.to_le_bytes().to_vec())
            .map_err(|_| out_of_range(kind_name)),
        "i64-le" => Ok(values::integer(value)?.to_le_bytes().to_vec()),
        "f32-le" => {
            let raw = number(value)? as f32;
            if !raw.is_finite() {
                return Err(EvalError::custom(
                    "invalid-input: binary floats must be finite",
                ));
            }
            Ok(raw.to_le_bytes().to_vec())
        }
        "f64-le" => {
            let raw = number(value)?;
            if !raw.is_finite() {
                return Err(EvalError::custom(
                    "invalid-input: binary floats must be finite",
                ));
            }
            Ok(raw.to_le_bytes().to_vec())
        }
        _ => Err(unknown_kind(kind_name)),
    }
}

fn binary_decode(kind_name: &str, bytes: &[u8], offset: usize) -> Result<Value, EvalError> {
    let size = match kind_name {
        "u8" => 1,
        "u16-le" => 2,
        "u32-le" => 4,
        "u64-le" => 8,
        "i32-le" => 4,
        "i64-le" => 8,
        "f32-le" => 4,
        "f64-le" => 8,
        _ => return Err(unknown_kind(kind_name)),
    };
    let end = offset
        .checked_add(size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| {
            EvalError::custom(format!(
                "invalid-input: {kind_name} read at offset {offset} exceeds the available bytes"
            ))
        })?;
    let window = &bytes[offset..end];
    Ok(match kind_name {
        "u8" => Value::Integer(i64::from(window[0])),
        "u16-le" => Value::Integer(i64::from(u16::from_le_bytes(
            window[..2].try_into().unwrap(),
        ))),
        "u32-le" => Value::Integer(i64::from(u32::from_le_bytes(
            window[..4].try_into().unwrap(),
        ))),
        "u64-le" => Value::Integer(
            i64::try_from(u64::from_le_bytes(window[..8].try_into().unwrap())).map_err(|_| {
                EvalError::custom(
                    "invalid-input: u64 value exceeds the signed 64-bit range of host integers",
                )
            })?,
        ),
        "i32-le" => Value::Integer(i64::from(i32::from_le_bytes(
            window[..4].try_into().unwrap(),
        ))),
        "i64-le" => Value::Integer(i64::from_le_bytes(window[..8].try_into().unwrap())),
        "f32-le" => {
            let raw = f32::from_le_bytes(window[..4].try_into().unwrap());
            if !raw.is_finite() {
                return Err(EvalError::custom("invalid-input: nonfinite binary float"));
            }
            Value::Float(f64::from(raw))
        }
        "f64-le" => {
            let raw = f64::from_le_bytes(window[..8].try_into().unwrap());
            if !raw.is_finite() {
                return Err(EvalError::custom("invalid-input: nonfinite binary float"));
            }
            Value::Float(raw)
        }
        _ => unreachable!("size match already validated the kind"),
    })
}

/// Compact JSON object bytes in the caller's order. Values recurse through
/// `to_json` (sorted maps), but the top-level pair order is emitted verbatim;
/// digests over historical records depend on that order surviving.
fn ordered_object(pairs: &Value) -> Result<Vec<u8>, EvalError> {
    let mut out = Vec::new();
    out.push(b'{');
    let mut seen: HashSet<&str> = HashSet::new();
    for pair in sequence(pairs, "vector or list of key-value pairs")?.iter() {
        let entry = sequence(pair, "key-value pair")?;
        if entry.len() != 2 {
            return Err(EvalError::custom(
                "invalid-input: json-ordered pairs must each be [key value]",
            ));
        }
        let key = match &entry[0] {
            Value::String(key) | Value::Keyword(key) => key,
            other => return Err(EvalError::type_error("string key", other.value_type())),
        };
        if !seen.insert(key) {
            return Err(EvalError::custom(format!(
                "invalid-input: duplicate JSON object key: {key}"
            )));
        }
        if out.len() > 1 {
            out.push(b',');
        }
        out.extend(
            serde_json::to_vec(&serde_json::Value::String(key.clone()))
                .map_err(|e| EvalError::custom(format!("JSON encoding error: {e}")))?,
        );
        out.push(b':');
        out.extend(
            serde_json::to_vec(&values::to_json(&entry[1])?)
                .map_err(|e| EvalError::custom(format!("JSON encoding error: {e}")))?,
        );
    }
    out.push(b'}');
    Ok(out)
}

pub(super) fn install(ctx: &EvalContext) {
    bind(ctx, "host/bytes", |args, _| {
        values::arity(&args, 1)?;
        values::with_bytes(&args[0], |bytes| buffer(bytes.to_vec()))
    });
    bind(ctx, "host/byte-length", |args, _| {
        values::arity(&args, 1)?;
        values::with_bytes(&args[0], |bytes| Value::Integer(bytes.len() as i64))
    });
    bind(ctx, "host/byte-slice", |args, _| {
        values::arity(&args, 3)?;
        let start = count(&args[1])?;
        let length = count(&args[2])?;
        values::with_bytes(&args[0], |bytes| {
            let end = start
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| {
                    EvalError::custom("invalid-input: byte-slice range exceeds the available bytes")
                })?;
            Ok(buffer(bytes[start..end].to_vec()))
        })?
    });
    bind(ctx, "host/byte-concat", |args, _| {
        values::arity(&args, 1)?;
        let mut out = Vec::new();
        for part in sequence(&args[0], "vector or list of byte sequences")?.iter() {
            values::with_bytes(part, |bytes| out.extend_from_slice(bytes))?;
        }
        Ok(buffer(out))
    });
    bind(ctx, "host/bytes-utf8", |args, _| {
        values::arity(&args, 1)?;
        values::with_bytes(&args[0], |bytes| {
            std::str::from_utf8(bytes)
                .map(|text| Value::String(text.into()))
                .map_err(|_| EvalError::custom("invalid-input: bytes are not valid UTF-8"))
        })?
    });
    bind(ctx, "host/binary-encode", |args, _| {
        values::arity(&args, 2)?;
        Ok(buffer(binary_encode(kind(&args[0])?, &args[1])?))
    });
    bind(ctx, "host/binary-decode", |args, _| {
        values::arity(&args, 3)?;
        let offset = count(&args[2])?;
        values::with_bytes(&args[1], |bytes| {
            binary_decode(kind(&args[0])?, bytes, offset)
        })?
    });
    bind(ctx, "host/json-bytes", |args, _| {
        values::arity(&args, 1)?;
        let json = values::to_json(&args[0])?;
        serde_json::to_vec(&json)
            .map(buffer)
            .map_err(|e| EvalError::custom(format!("JSON encoding error: {e}")))
    });
    bind(ctx, "host/json-ordered", |args, _| {
        values::arity(&args, 1)?;
        Ok(buffer(ordered_object(&args[0])?))
    });
    bind(ctx, "host/json-read", |args, _| {
        values::arity(&args, 1)?;
        values::with_bytes(&args[0], |bytes| {
            let json: serde_json::Value = serde_json::from_slice(bytes)
                .map_err(|e| EvalError::custom(format!("JSON parse error: {e}")))?;
            values::from_json(json)
        })?
    });
}
