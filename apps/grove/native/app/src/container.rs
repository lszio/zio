//! The GVD1 observation container, written by the product.
//!
//! The worker reads a binary container, not a JSON blob. A product that
//! "represents" an observation as JSON would be staging something the
//! backend has never parsed, and the prediction that came back would
//! describe a file the model never saw. So the container is written here,
//! in the same format `examples/self-learning/generate.py` and
//! `apps/grove/workers/torch/data.py` agree on, and the one rule that matters is
//! kept: **a missing modality is a declared absence**, never a
//! zero-filled stand-in.
//!
//! Layout (little-endian):
//!   "GVD1" | u32 header_len | header JSON | rows…
//!   row = scene_id:u32, image_present:u8, numeric_present:u8,
//!         label:u8, flags:u8, r1:f32, r2:f32, pixels[256]
//!
//! `label` is `0xFF` when the caller has no oracle for this row — the
//! same sentinel the frozen holdout split uses, so "no answer here" is
//! indistinguishable from "no answer anywhere".

use grove::contracts::{Error, ErrorKind, Result};

/// The label sentinel meaning "this row has no answer".
pub const LABEL_NONE: u8 = 0xFF;
const PIXELS: usize = 256;
const ROW_HEADER: usize = 16;
const ROW_BYTES: usize = ROW_HEADER + PIXELS;

/// One observation row as the product sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub scene_id: u32,
    pub image_present: bool,
    pub numeric_present: bool,
    /// `None` = no oracle. Written as [`LABEL_NONE`].
    pub label: Option<u8>,
    pub flags: u8,
    pub readings: [f32; 2],
    /// Exactly 256 bytes. When `image_present` is false this is ignored
    /// by the reader, which is why the absence is a *declared* fact: the
    /// bytes are not consulted, and the mask says so.
    pub pixels: [u8; PIXELS],
}

impl Row {
    /// A row with a real image and no numeric reading — the shape the
    /// product's own observation form produces.
    pub fn from_parts(
        scene_id: u32,
        pixels: [u8; PIXELS],
        readings: [f32; 2],
        numeric_present: bool,
        label: Option<u8>,
    ) -> Self {
        Self {
            scene_id,
            image_present: true,
            numeric_present,
            label,
            flags: if numeric_present { 0 } else { 0b0000_0010 },
            readings,
            pixels,
        }
    }
}

/// Encode rows as a GVD1 container. The header is the same JSON the
/// generator writes, with `labels_present` reflecting whether any row
/// actually carries an answer — a container that claims labels while
/// holding none is how an evaluation set leaks its own answers.
pub fn encode(rows: &[Row], split: &str) -> Result<Vec<u8>> {
    if rows.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a container with no rows would let the worker predict about nothing",
        ));
    }
    let labels_present = rows.iter().any(|r| r.label.is_some());
    let header = serde_json::json!({
        "magic": "GVD1",
        "schema": 1,
        "generator": "grove.product/1.0.0",
        "split": split,
        "seed": 0,
        "scenes": rows.len(),
        "variants_per_scene": 1,
        "side": 16,
        "record_bytes": ROW_BYTES,
        "fields": ["scene_id", "image_present", "numeric_present", "label",
                   "flags", "reading1", "reading2", "pixels[256]"],
        "labels_present": labels_present,
    });
    let blob = serde_json::to_vec(&header).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("container header is not serializable: {e}"),
        )
    })?;

    let mut out = Vec::with_capacity(8 + blob.len() + rows.len() * ROW_BYTES);
    out.extend_from_slice(b"GVD1");
    out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
    out.extend_from_slice(&blob);
    for row in rows {
        // scene_id:u32, image_present:u8, numeric_present:u8, label:u8,
        // flags:u8, r1:f32, r2:f32, pixels[256]
        out.extend_from_slice(&row.scene_id.to_le_bytes());
        out.push(u8::from(row.image_present));
        out.push(u8::from(row.numeric_present));
        out.push(row.label.unwrap_or(LABEL_NONE));
        out.push(row.flags);
        out.extend_from_slice(&row.readings[0].to_le_bytes());
        out.extend_from_slice(&row.readings[1].to_le_bytes());
        out.extend_from_slice(&row.pixels);
    }
    Ok(out)
}

/// Read back the rows this module wrote. Used by the product's own
/// verification path and by the tests: a container writer that cannot
/// read its own output is a writer nobody should trust.
pub fn decode(bytes: &[u8]) -> Result<(serde_json::Value, Vec<Row>)> {
    if bytes.len() < 8 || &bytes[0..4] != b"GVD1" {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "not a GVD1 container",
        ));
    }
    let header_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let head = 8 + header_len;
    if bytes.len() < head {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "GVD1 header runs past the end of the container",
        ));
    }
    let header: serde_json::Value =
        serde_json::from_slice(&bytes[8..head]).map_err(|e| {
            Error::new(
                ErrorKind::IncompatibleState,
                format!("GVD1 header is not JSON: {e}"),
            )
        })?;
    let stride = header["record_bytes"].as_u64().unwrap_or(ROW_BYTES as u64) as usize;
    if stride < ROW_BYTES {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("GVD1 record_bytes {stride} is shorter than the row layout"),
        ));
    }
    let mut rows = Vec::new();
    let mut pos = head;
    while pos + stride <= bytes.len() {
        let mut pixels = [0u8; PIXELS];
        pixels.copy_from_slice(&bytes[pos + ROW_HEADER..pos + ROW_HEADER + PIXELS]);
        let label = bytes[pos + 6];
        rows.push(Row {
            scene_id: u32::from_le_bytes([
                bytes[pos],
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
            ]),
            image_present: bytes[pos + 4] != 0,
            numeric_present: bytes[pos + 5] != 0,
            label: if label == LABEL_NONE { None } else { Some(label) },
            flags: bytes[pos + 7],
            readings: [
                f32::from_le_bytes([
                    bytes[pos + 8],
                    bytes[pos + 9],
                    bytes[pos + 10],
                    bytes[pos + 11],
                ]),
                f32::from_le_bytes([
                    bytes[pos + 12],
                    bytes[pos + 13],
                    bytes[pos + 14],
                    bytes[pos + 15],
                ]),
            ],
            pixels,
        });
        pos += stride;
    }
    Ok((header, rows))
}
