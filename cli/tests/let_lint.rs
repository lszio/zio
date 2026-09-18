use std::collections::HashSet;
use std::path::PathBuf;
use zio_core::reader;
use zio_core::sexp::Sexp;

/// Static lint for the parallel-binding trap in `let` and `loop`.
///
/// Both forms evaluate every init in the OUTER environment and then bind
/// (like Scheme `let`), so an init that references an earlier sibling
/// binding is unbound at evaluation time. All three lib/zio libraries
/// shipped broken because of exactly this pattern (see the 2026-09
/// architecture review). This lint scans the repo's Zio sources and fails
/// on new occurrences; use `let*` for sequential bindings.
///
/// Shielded constructs (not scanned for references):
/// - `fn` bodies: parameter bindings shadow, and the body is lazy anyway
/// - `quote` forms: data, not code
/// - `defmacro` definitions: templates need macro-level semantics

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below workspace root")
        .to_path_buf()
}

fn binding_pairs(items: &im::Vector<Sexp>) -> Vec<(String, Sexp)> {
    let bindings = match items.get(1) {
        Some(Sexp::Vector(v, _)) => v.clone(),
        _ => return Vec::new(),
    };
    let mut pairs = Vec::new();
    let mut iter = bindings.iter();
    while let (Some(name), Some(init)) = (iter.next(), iter.next()) {
        if let Sexp::Symbol(n, _) = name {
            pairs.push((n.clone(), init.clone()));
        }
    }
    pairs
}

fn head_symbol(items: &im::Vector<Sexp>) -> Option<String> {
    match items.front() {
        Some(Sexp::Symbol(s, _)) => Some(s.clone()),
        _ => None,
    }
}

/// Return flagged symbol references in `expr` that are in `names` and not
/// shadowed by `shadowed`.
fn flagged_refs(
    expr: &Sexp,
    names: &HashSet<String>,
    shadowed: &mut HashSet<String>,
) -> Vec<String> {
    match expr {
        Sexp::Symbol(s, _) if names.contains(s) && !shadowed.contains(s) => vec![s.clone()],
        Sexp::List(items, _) => {
            match head_symbol(items).as_deref() {
                // Lazy or data contexts: references inside are captured, not read now.
                Some("fn") | Some("quote") | Some("defmacro") => return Vec::new(),
                // A nested binding form shadows its own names for its contents.
                Some("let") | Some("let*") | Some("loop") => {
                    let mut inner = shadowed.clone();
                    for (name, _) in binding_pairs(items) {
                        inner.insert(name);
                    }
                    let mut found = Vec::new();
                    for item in items.iter().skip(1) {
                        found.extend(flagged_refs(item, names, &mut inner));
                    }
                    return found;
                }
                _ => {}
            }
            items.iter().flat_map(|i| flagged_refs(i, names, shadowed)).collect()
        }
        Sexp::Vector(items, _) => {
            items.iter().flat_map(|i| flagged_refs(i, names, shadowed)).collect()
        }
        _ => Vec::new(),
    }
}

fn check_parallel_form(rel: &str, items: &im::Vector<Sexp>, out: &mut Vec<String>) {
    let pairs = binding_pairs(items);
    for (i, (name, init)) in pairs.iter().enumerate() {
        let earlier: HashSet<String> =
            pairs[..i].iter().map(|(n, _)| n.clone()).collect();
        for violated in flagged_refs(init, &earlier, &mut HashSet::new()) {
            out.push(format!(
                "{rel}: binding '{name}' references earlier sibling '{violated}' \
                 — 'let'/'loop' bind in parallel; use 'let*' for sequential bindings"
            ));
        }
    }
}

fn walk(rel: &str, expr: &Sexp, out: &mut Vec<String>) {
    match expr {
        Sexp::List(items, _) => {
            if let Some(head) = head_symbol(items) {
                if head == "let" || head == "loop" {
                    check_parallel_form(rel, items, out);
                }
            }
            for item in items.iter() {
                walk(rel, item, out);
            }
        }
        Sexp::Vector(items, _) => {
            for item in items.iter() {
                walk(rel, item, out);
            }
        }
        _ => {}
    }
}

fn lint_file(rel: &str) -> Vec<String> {
    let path = workspace_root().join(rel);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let mut out = Vec::new();
    for form in reader::reader::read_program(&source)
        .unwrap_or_else(|e| panic!("parse {rel}: {e}"))
    {
        walk(rel, &form, &mut out);
    }
    out
}

#[test]
fn zio_sources_have_no_parallel_let_misuse() {
    let mut violations = Vec::new();
    for rel in [
        "lib/zio/persistent.zio",
        "lib/zio/datalog.zio",
        "lib/zio/agent.zio",
        "lib/zio/entity.zio",
        "lib/zio/learn.zio",
        "lib/zio/pipeline.zio",
        "lib/zio/protocol.zio",
        "examples/basics.zio",
        "examples/macros.zio",
        "examples/datalog-concept.zio",
        "examples/zos-concept.zio",
        "core/stdlib/zio/core.zio",
    ] {
        violations.extend(lint_file(rel));
    }
    assert!(
        violations.is_empty(),
        "parallel-binding misuse found:\n{}",
        violations.join("\n")
    );
}
