//! Byte-bounded JSON-RPC framing; no evaluation or application policy.
use serde_json::Value;
use std::io::{self, Read, Write};

fn protocol(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub fn validate_rpc(value: &Value) -> io::Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| protocol("JSON-RPC frame must be an object"))?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(protocol("JSON-RPC version must be 2.0"));
    }
    if let Some(id) = object.get("id") {
        if !id.is_null() && !id.is_string() && !id.is_number() {
            return Err(protocol("invalid JSON-RPC id"));
        }
    }
    if let Some(method) = object.get("method") {
        if !method.is_string()
            || method.as_str() == Some("")
            || object.contains_key("result")
            || object.contains_key("error")
        {
            return Err(protocol("invalid JSON-RPC request"));
        }
        if object
            .get("params")
            .is_some_and(|v| !v.is_array() && !v.is_object())
        {
            return Err(protocol("invalid JSON-RPC params"));
        }
    } else {
        if !object.contains_key("id")
            || object.contains_key("result") == object.contains_key("error")
        {
            return Err(protocol("invalid JSON-RPC response"));
        }
        if let Some(error) = object.get("error") {
            if error.get("code").and_then(Value::as_i64).is_none()
                || error.get("message").and_then(Value::as_str).is_none()
            {
                return Err(protocol("invalid JSON-RPC error"));
            }
        }
    }
    Ok(())
}

pub fn content_length(headers: &[u8], max_body: usize) -> io::Result<usize> {
    let text = std::str::from_utf8(headers).map_err(|_| protocol("frame headers are not UTF-8"))?;
    let mut length = None;
    for line in text.split("\r\n").filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| protocol("malformed frame header"))?;
        if key.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(protocol("duplicate Content-Length"));
            }
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(protocol("invalid Content-Length"));
            }
            let parsed = value
                .parse::<usize>()
                .map_err(|_| protocol("invalid Content-Length"))?;
            if parsed > max_body {
                return Err(protocol("frame body exceeds byte limit"));
            }
            length = Some(parsed);
        } else if !key.eq_ignore_ascii_case("Content-Type") {
            return Err(protocol("unsupported frame header"));
        }
    }
    length.ok_or_else(|| protocol("missing Content-Length"))
}

/// Clean EOF returns None. EOF anywhere within a frame is an error.
pub fn read_content_length<R: Read>(
    reader: &mut R,
    max_headers: usize,
    max_body: usize,
) -> io::Result<Option<Value>> {
    let mut headers = Vec::new();
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) if headers.is_empty() => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated frame headers",
                ));
            }
            Ok(_) => headers.push(byte[0]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        if headers.len() > max_headers {
            return Err(protocol("frame headers exceed byte limit"));
        }
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let length = content_length(&headers, max_body)?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body)
        .map_err(|error| protocol(format!("invalid JSON frame: {error}")))?;
    validate_rpc(&value)?;
    Ok(Some(value))
}

pub fn write_content_length<W: Write>(
    writer: &mut W,
    value: &Value,
    max_body: usize,
) -> io::Result<()> {
    validate_rpc(value)?;
    let body = serde_json::to_vec(value).map_err(|error| protocol(error.to_string()))?;
    if body.len() > max_body {
        return Err(protocol("frame body exceeds byte limit"));
    }
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}
