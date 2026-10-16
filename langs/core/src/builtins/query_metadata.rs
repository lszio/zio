//! Read-only metadata extraction for the query service (ZIO-004).
//!
//! `describe_value` renders one runtime value's callable shape without
//! executing the target, expanding macros, loading modules, invoking
//! getters/printers, or touching source I/O. Documentation is read from
//! the original registered source *passively* with the Rust reader
//! (`reader::reader::read_program_with_source`) — never through the
//! installed `zi-read`, `eval`, or macro expansion — so a query stays
//! inert even when the Zi reader is poisoned or a module was only
//! wire-restored. Source identity is span containment against the actual
//! value's own registration, never a filename's newest revision.

use crate::bytecode::Closure;
use crate::context::EvalEngine;
use crate::im::{HashMap, Vector};
use crate::sexp::Sexp;
use crate::span::{SourceFile, SourceId, Span, line_starts_of};
use crate::value::{NativeFn, Value, ZosInstance};
use crate::zos::gf::GFObject;
use crate::zos::object::ZosObject;

fn keyword(name: &str) -> Value {
    Value::Keyword(name.into())
}

/// The description keys owned by this module. Every description must
/// carry all of them (values differ only).
const KEYS: [&str; 8] = [
    "kind",
    "origin",
    "params",
    "rest",
    "doc",
    "documentation",
    "source",
    "examples",
];

/// Explicitly unknown callable shape: never a zero-arity claim.
fn unknown_shape() -> HashMap<Value, Value> {
    let mut map = HashMap::new();
    for key in KEYS {
        map.insert(keyword(key), Value::Nil);
    }
    map.insert(keyword("kind"), keyword("value"));
    map.insert(keyword("origin"), keyword("unknown"));
    map.insert(keyword("documentation"), keyword("unknown"));
    map.insert(keyword("examples"), Value::Vector(Vector::new()));
    map
}

/// All keys present with concrete non-nil values (callable basics).
fn shape_with(kind: &str, params: &Vector<String>, rest: &Option<String>) -> HashMap<Value, Value> {
    let mut map = HashMap::new();
    map.insert(keyword("kind"), keyword(kind));
    map.insert(keyword("origin"), keyword("source"));
    map.insert(
        keyword("params"),
        Value::Vector(params.iter().map(|p| Value::String(p.clone())).collect()),
    );
    map.insert(
        keyword("rest"),
        rest.clone().map(Value::String).unwrap_or(Value::Nil),
    );
    map
}

/// Render a value's callable shape for the query service.
pub(super) fn describe_value(value: &Value, engine: &dyn EvalEngine) -> HashMap<Value, Value> {
    match value {
        Value::Function(function) => {
            let mut map = shape_with("function", &function.params, &function.rest_param);
            let info = match &function.compiled {
                Some(closure) => compiled_source(closure, engine),
                None => ast_source(&function.body, engine, None),
            };
            fill_source_docs(&mut map, info);
            map
        }
        Value::Macro(mac) => {
            let mut map = shape_with("macro", &mac.params, &mac.rest_param);
            let info = match &mac.compiled {
                Some(closure) => compiled_source(closure, engine),
                None => ast_source(&mac.body, engine, Some(&mac.name)),
            };
            fill_source_docs(&mut map, info);
            map
        }
        Value::NativeFunction(native) => native_metadata(native),
        Value::Object(object) => object_metadata(object.as_ref()),
        // Plain values are not callable: the shape stays explicitly
        // unknown and the value itself never enters the description.
        _ => unknown_shape(),
    }
}

// ── Native functions ──────────────────────────────────────────────

fn native_metadata(native: &NativeFn) -> HashMap<Value, Value> {
    let mut map = HashMap::new();
    map.insert(keyword("kind"), keyword("native"));
    map.insert(keyword("origin"), keyword("native"));
    map.insert(keyword("source"), Value::Nil);
    match native.metadata() {
        Some(meta) => {
            map.insert(
                keyword("params"),
                Value::Vector(
                    meta.params
                        .iter()
                        .map(|p| Value::String((*p).to_string()))
                        .collect(),
                ),
            );
            map.insert(
                keyword("rest"),
                meta.rest
                    .map(|r| Value::String(r.to_string()))
                    .unwrap_or(Value::Nil),
            );
            let (doc_text, examples) =
                doc_and_examples(meta.doc.lines().map(str::to_string).collect());
            if doc_text.trim().is_empty() {
                map.insert(keyword("doc"), Value::Nil);
                map.insert(keyword("documentation"), keyword("missing"));
            } else {
                map.insert(
                    keyword("doc"),
                    Value::String(doc_text.trim_end().to_string()),
                );
                map.insert(keyword("documentation"), keyword("available"));
            }
            map.insert(
                keyword("examples"),
                Value::Vector(
                    examples
                        .iter()
                        .map(|block| example_map(block, Value::Nil))
                        .collect(),
                ),
            );
        }
        None => {
            map.insert(keyword("params"), Value::Nil);
            map.insert(keyword("rest"), Value::Nil);
            map.insert(keyword("doc"), Value::Nil);
            map.insert(keyword("documentation"), keyword("unknown"));
            map.insert(keyword("examples"), Value::Vector(Vector::new()));
        }
    }
    map
}

// ── ZOS objects (passive downcasts, no printers) ──────────────────

fn object_metadata(object: &dyn ZosObject) -> HashMap<Value, Value> {
    let mut map = unknown_shape();
    let (kind, origin) = if object.as_any().downcast_ref::<GFObject>().is_some() {
        ("generic-function", "zos")
    } else if object.as_any().downcast_ref::<ZosInstance>().is_some_and(|instance| {
        instance.header.flags.bits() == crate::zos::object::ObjectFlags::MUTABLE.bits()
            && matches!(instance.slots.get("__class_name__"), Some(Value::Symbol(name)) if name == &instance.header.class.name)
    }) {
        ("class", "zos")
    } else {
        ("value", "zos")
    };
    map.insert(keyword("kind"), keyword(kind));
    map.insert(keyword("origin"), keyword(origin));
    map
}

// ── Source-backed documentation ───────────────────────────────────

/// One resolved source location for a callable: the registered file, its
/// revision identity, and the enclosing definition's own span.
struct SourceInfo {
    file: SourceFile,
    revision: usize,
    definition: Span,
}

/// Locate a callable's source from a compiled closure, using only the
/// artifact's own registered revision (exact map identity) and its debug
/// table. Never decodes spans, never runs the installed Zi reader.
fn compiled_source(closure: &Closure, engine: &dyn EvalEngine) -> Option<SourceInfo> {
    let module = &closure.module;
    let revision = module.source_revision(engine.source_map())?;
    let file = engine.source_map().get(SourceId(revision))?;
    // Only trust the artifact's own registration: a same-name redefinition
    // must never leak its text into this artifact's docs.
    if file.source != module.source_text()? {
        return None;
    }
    let anchor = compiled_anchor(closure, &file)?;
    let name_hint = module
        .functions
        .get(closure.fn_index)
        .and_then(|record| record.name.clone());
    let definition = find_definition(&file, anchor, name_hint.as_deref())?;
    Some(SourceInfo {
        file,
        revision,
        definition,
    })
}

/// Convert the first real `(line, col)` debug entry of a compiled
/// function into a byte anchor into `file.source`. The debug positions
/// come from the compiler's own span table, so the anchor only needs to
/// land inside the definition's span (containment is the identity).
fn compiled_anchor(closure: &Closure, file: &SourceFile) -> Option<usize> {
    let (line, col) = closure
        .module
        .functions
        .get(closure.fn_index)?
        .debug
        .iter()
        .find_map(|entry| *entry)?;
    byte_from_line_col(&file.source, line, col)
}

fn byte_from_line_col(source: &str, line: usize, col: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let starts = line_starts_of(source);
    let line_start = starts.get(line - 1)?.0;
    let line_end = starts
        .get(line)
        .map(|offset| offset.0)
        .unwrap_or(source.len());
    // Clamp the column to the line's own byte length; a debug entry may
    // point past the final line when the body is the file's last form.
    let col = col.clamp(1, line_end.saturating_sub(line_start).max(1));
    Some(line_start + col - 1)
}

/// Locate a callable's source from its AST body span. The span carries
/// the exact registration the value was created under, so a saved alias
/// keeps its original revision across redefinition of the same file.
fn ast_source(body: &Sexp, engine: &dyn EvalEngine, name_hint: Option<&str>) -> Option<SourceInfo> {
    let anchor_span = first_real_span(body)?;
    let file = engine.source_map().get(anchor_span.source_id)?;
    let revision = anchor_span.source_id.0;
    let definition = find_definition(&file, anchor_span.start.0, name_hint)?;
    Some(SourceInfo {
        file,
        revision,
        definition,
    })
}

/// The first span that is genuinely from source, descending a synthetic
/// `do` wrapper (multi-form bodies build `(do ...)` with no span of their
/// own). Nil/Boolean nodes have no span: never guessed.
fn first_real_span(sexp: &Sexp) -> Option<Span> {
    match sexp {
        Sexp::Nil | Sexp::Boolean(_) => None,
        Sexp::List(items, span) => (*span).or_else(|| items.iter().find_map(first_real_span)),
        _ => sexp.span(),
    }
}

/// Find the smallest enclosing `defn`/`defmacro`/`fn` definition whose
/// span contains `anchor`, preferring a candidate whose defined name
/// matches `name_hint` (compiled records carry their source-level name).
fn find_definition(file: &SourceFile, anchor: usize, name_hint: Option<&str>) -> Option<Span> {
    let forms = crate::reader::reader::read_program_with_source(&file.source, file.id).ok()?;
    let mut candidates: Vec<(Span, Option<String>)> = Vec::new();
    for form in &forms {
        collect_definitions(form, &mut candidates);
    }

    if let Some(hint) = name_hint
        && let Some(best) = candidates
            .iter()
            .filter(|(span, name)| name.as_deref() == Some(hint) && contains(*span, anchor))
            .min_by_key(|(span, _)| span.end.0 - span.start.0)
    {
        return Some(best.0);
    }
    candidates
        .iter()
        .filter(|(span, _)| contains(*span, anchor))
        .min_by_key(|(span, _)| span.end.0 - span.start.0)
        .map(|(span, _)| *span)
}

fn contains(span: Span, offset: usize) -> bool {
    span.start.0 < offset && offset < span.end.0
}

fn collect_definitions(sexp: &Sexp, out: &mut Vec<(Span, Option<String>)>) {
    match sexp {
        Sexp::List(items, span) => {
            if let Some(span) = span
                && let Some(name) = definition_name(sexp)
            {
                out.push((*span, name));
            }
            for item in items.iter() {
                collect_definitions(item, out);
            }
        }
        Sexp::Vector(items, _) => {
            for item in items.iter() {
                collect_definitions(item, out);
            }
        }
        Sexp::Map(entries, _) => {
            for (key, value) in entries.iter() {
                collect_definitions(key, out);
                collect_definitions(value, out);
            }
        }
        _ => {}
    }
}

/// The defined name of a `defn`/`defmacro` form; anonymous `fn` has none.
fn definition_name(sexp: &Sexp) -> Option<Option<String>> {
    let Sexp::List(items, _) = sexp else {
        return None;
    };
    let Sexp::Symbol(head, _) = items.get(0)? else {
        return None;
    };
    match head.as_str() {
        "defn" | "defmacro" => match items.get(1) {
            Some(Sexp::Symbol(name, _)) => Some(Some(name.clone())),
            _ => Some(None),
        },
        "fn" => Some(None),
        _ => None,
    }
}

// ── Doc blocks and examples ───────────────────────────────────────

struct DocBlock {
    doc_text: String,
    examples: Vec<Vec<String>>,
    has_doc_marker: bool,
}

/// Adjacent symbol comments follow the same convention as the site:
/// `;;` and `;; doc:` are equivalent; file headers and banners are not docs.
/// A definition inside another expression cannot inherit that line's docs.
fn doc_block(source: &str, definition: Span) -> DocBlock {
    let prefix = &source[..definition.start.0];
    let line_start = prefix.rfind('\n').map_or(0, |offset| offset + 1);
    if !prefix[line_start..].trim().is_empty() {
        return DocBlock {
            doc_text: String::new(),
            examples: Vec::new(),
            has_doc_marker: false,
        };
    }
    let prefix = &prefix[..line_start];
    let mut block: Vec<&str> = prefix
        .lines()
        .rev()
        .take_while(|line| line.trim_start().starts_with(";;"))
        .collect();
    block.reverse();
    let begins_file = block.len() == prefix.lines().count();
    let mut include = !begins_file;
    let mut lines = Vec::new();
    for raw in block {
        let content = raw.trim_start().strip_prefix(";;").unwrap();
        let content = content.strip_prefix(' ').unwrap_or(content);
        if content.trim_start().starts_with('─') {
            lines.clear();
            include = true;
            continue;
        }
        let content = if let Some(doc) = content.strip_prefix("doc:") {
            include = true;
            doc.strip_prefix(' ').unwrap_or(doc)
        } else {
            content
        };
        if include {
            lines.push(content.to_string());
        }
    }
    let (doc_text, examples) = doc_and_examples(lines);
    let has_doc_marker = !doc_text.trim().is_empty() || !examples.is_empty();
    DocBlock {
        doc_text,
        examples,
        has_doc_marker,
    }
}

/// Join an example block, dedenting by its minimum indentation so nested
/// code keeps its shape.
fn dedent(lines: &[String]) -> String {
    let min_indent = lines
        .iter()
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| line[min_indent.min(line.len())..].to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn example_map(block: &[String], source: Value) -> Value {
    let mut map = HashMap::new();
    map.insert(keyword("text"), Value::String(dedent(block)));
    map.insert(keyword("source"), source);
    Value::Map(map)
}

/// Split plain text (a native doc string) into doc text and example
/// blocks under the same indentation rule.
fn doc_and_examples(lines: Vec<String>) -> (String, Vec<Vec<String>>) {
    let mut doc_lines: Vec<String> = Vec::new();
    let mut examples: Vec<Vec<String>> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    for content in lines {
        let indent = content.len() - content.trim_start().len();
        if indent >= 2 && !content.trim().is_empty() {
            pending.push(content);
        } else {
            if !pending.is_empty() {
                examples.push(std::mem::take(&mut pending));
            }
            doc_lines.push(content);
        }
    }
    if !pending.is_empty() {
        examples.push(pending);
    }
    (doc_lines.join("\n"), examples)
}

fn fill_source_docs(map: &mut HashMap<Value, Value>, info: Option<SourceInfo>) {
    let Some(info) = info else {
        map.insert(keyword("doc"), Value::Nil);
        map.insert(keyword("documentation"), keyword("unknown"));
        map.insert(keyword("source"), Value::Nil);
        map.insert(keyword("examples"), Value::Vector(Vector::new()));
        return;
    };

    let mut source_map = HashMap::new();
    source_map.insert(keyword("name"), Value::String(info.file.name.clone()));
    source_map.insert(keyword("revision"), Value::Integer(info.revision as i64));
    source_map.insert(
        keyword("start"),
        Value::Integer(info.definition.start.0 as i64),
    );
    source_map.insert(keyword("end"), Value::Integer(info.definition.end.0 as i64));
    source_map.insert(keyword("line"), Value::Integer(info.definition.line as i64));
    source_map.insert(
        keyword("column"),
        Value::Integer(info.definition.col as i64),
    );
    let source_value = Value::Map(source_map);

    let block = doc_block(&info.file.source, info.definition);
    if block.has_doc_marker {
        map.insert(keyword("documentation"), keyword("available"));
        map.insert(
            keyword("doc"),
            if block.doc_text.trim().is_empty() {
                Value::Nil
            } else {
                Value::String(block.doc_text.trim_end().to_string())
            },
        );
    } else {
        map.insert(keyword("documentation"), keyword("missing"));
        map.insert(keyword("doc"), Value::Nil);
    }
    map.insert(
        keyword("examples"),
        Value::Vector(
            block
                .examples
                .iter()
                .map(|block| example_map(block, source_value.clone()))
                .collect(),
        ),
    );
    map.insert(keyword("source"), source_value);
}
