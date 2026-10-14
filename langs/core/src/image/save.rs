//! Still-image save: walk explicit roots with environment parent chains
//! and the context's module table into one validated wire image. Save is
//! a still moment — the execution stack, futures and channels are not
//! part of it and are refused by the encoder with their object path.

use super::{Encoder, ImageLimits, wire};
use crate::context::EvalContext;
use crate::error::EvalError;
use crate::value::Value;

/// Encode `roots` — plus every registered module's environment — into a
/// deterministic image. Environment parent chains are included, so a
/// closure over a local binding carries the chain it closes over,
/// including the context root.
pub fn save_image(
    ctx: &EvalContext,
    roots: &[Value],
    limits: &ImageLimits,
) -> Result<wire::Image, EvalError> {
    let mut encoder = Encoder {
        nodes: Vec::new(),
        interned: std::collections::HashMap::new(),
        limits: *limits,
        resources: Vec::new(),
        depth: 0,
        encode_parents: true,
    };

    // Modules first, ordered by display name: their environments join the
    // graph and each record names its env node. Sorting keeps the format
    // deterministic even though the table is an unordered Vec.
    let mut modules = ctx.modules.borrow().modules.clone();
    modules.sort_by_key(|module| module.display_name());
    let mut records = Vec::with_capacity(modules.len());
    for module in &modules {
        let path = format!("modules/{}", module.display_name());
        let env_id = encoder.env(&module.env, &path)?;
        let source = module
            .source
            .and_then(|id| ctx.source_map.get(id).map(|file| file.name.clone()));
        records.push(wire::ModuleRecord {
            name: module.display_name(),
            exports: {
                let mut exports = module.exports.clone();
                exports.sort();
                exports
            },
            env: Some(env_id),
            source,
        });
    }

    let mut root_ids = Vec::with_capacity(roots.len());
    for (index, root) in roots.iter().enumerate() {
        let encoded = encoder.value(root, &format!("roots[{index}]"))?;
        root_ids.push(match encoded {
            super::Encoded::Inline(payload) => {
                encoder.node_limit(&format!("roots[{index}]"))?;
                encoder.push_node(super::root_kind(root), payload)
            }
            super::Encoded::Ref(id) => id,
        });
    }

    let image = wire::Image {
        header: wire::Header {
            format: wire::FORMAT.to_string(),
            format_version: wire::FORMAT_VERSION,
            runtime: env!("CARGO_PKG_VERSION").to_string(),
            profile: "still-v1".to_string(),
        },
        roots: root_ids,
        root_names: Vec::new(),
        nodes: encoder.nodes,
        modules: records,
        resources: encoder.resources,
    };
    image.validate(limits)?;
    Ok(image)
}

/// Save with caller-chosen root names; restore binds each restored root
/// under its name. Names are binding keys chosen by the host — the image
/// never stores an authority or a path.
pub fn save_named_image(
    ctx: &EvalContext,
    roots: &[(String, Value)],
    limits: &ImageLimits,
) -> Result<wire::Image, EvalError> {
    let values: Vec<Value> = roots.iter().map(|(_, value)| value.clone()).collect();
    let mut image = save_image(ctx, &values, limits)?;
    image.root_names = roots.iter().map(|(name, _)| name.clone()).collect();
    image.validate(limits)?;
    Ok(image)
}
