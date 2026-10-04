//! W14: three indexes over one experience fact base, plus abstraction
//! extraction with a measured reuse result.
//!
//! ## One fact base, three views
//!
//! The experience records already in [`Store`] (observations, signals,
//! predictions, evaluations) are the only fact base. An *index entry* is
//! an [`ExperienceIndexEntry`] that names those records and carries the
//! fields one particular index needs; it stores no second copy of the
//! experience itself. Concretely an entry names:
//!
//! * `ast` — the program source the structural index reads;
//! * `probe_set` / `fingerprint` — a behaviour fingerprint valid **only**
//!   under that named probe set;
//! * optional `vector` + `encoder` + `space_version` — a semantic hint.
//!
//! ## The four refusals
//!
//! Each is a hard error with an [`ErrorKind`], not a filtered-out row:
//!
//! 1. **Semantic near-neighbour cannot merge into a different AST.**
//!    [`merge_structural`] accepts only an AST-identical pair; vector
//!    similarity never enters the decision.
//! 2. **Different probe sets are never compared.**
//!    [`behaviour_match`] returns a refusal string for a cross-probe-set
//!    pair and never a similarity number.
//! 3. **Retrieval cannot cross a data licence.** An entry whose own
//!    `usage_permitted` is false, or any of whose source observations is
//!    not `training_permitted`, makes the retrieval fail with
//!    [`ErrorKind::CapabilityDenied`] — the whole query, so a filtered row
//!    can never be mistaken for an empty result.
//! 4. **A capability derived from a retracted source refuses to load.**
//!    Before an abstraction is promoted, every source signal must still
//!    be `accepted`; a retracted one is [`ErrorKind::IncompatibleState`].
//!
//! ## Abstraction promotion is gated, not asserted
//!
//! [`promote_abstraction`] requires (a) regression to still pass on the
//! original tasks and (b) a *new* task the abstraction never saw. The
//! new task is mandatory: an abstraction that only ever re-solves its own
//! discovery set is not reuse. When a macro is involved, the expanded
//! form is re-checked through the zio `macroexpand` path, because
//! expansion can reintroduce hidden dependencies.
//!
//! ## The zio boundary
//!
//! The index and abstraction logic actually runs in `lib/zio/memory.zio`
//! and `lib/zio/vector.zio`, driven from this module through a real
//! [`EvalContext`] (the pattern from `ai/tests/learn3_contract.rs`). The
//! Rust side is the licence/retraction gate and the durable record
//! writer; it does not re-implement the index arithmetic in a second
//! place.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::value::Value;

use crate::contracts::{
    Actor, ActorRole, Error, ErrorKind, Observation, Result, SchemaVersion, SCHEMA_VERSION,
};
use crate::store::Store;

// ── the index entry (one fact base, three views) ─────────────────

/// A version-stamped index entry. It *names* experience records; it does
/// not duplicate them. `schema` participates in the stored identity, so an
/// index written under a different schema cannot be read as if it were
/// the current one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperienceIndexEntry {
    pub schema: SchemaVersion,
    pub id: String,
    pub task_id: String,
    /// The program text the structural index reads. This is the fact the
    /// structural index is allowed to merge on; nothing else may.
    pub ast: String,
    /// Behavioural view. A fingerprint is meaningful only under the named
    /// probe set that produced it.
    pub probe_set: String,
    pub fingerprint: String,
    /// Optional semantic view. When present, `encoder` and
    /// `space_version` travel with the coordinates and are enforced on
    /// every query.
    pub vector: Option<Vec<i64>>,
    pub encoder: Option<String>,
    pub space_version: Option<i64>,
    /// The experience records this entry indexes. Empty for a pure
    /// program, but non-empty for anything learned from data — and that
    /// is exactly the list the licence and retraction gates walk.
    pub source_observation_ids: Vec<String>,
    pub source_signal_ids: Vec<String>,
    /// Licence: may this entry be used for retrieval and training at all?
    pub usage_permitted: bool,
    /// True when the entry was verified against a real task.
    pub verified: bool,
    pub created_at_ms: i64,
}

impl ExperienceIndexEntry {
    /// Reject an entry whose semantic view is half-declared: a vector
    /// with no encoder or space version is a coordinate set whose meaning
    /// is unknown, and accepting it would let a later query reinterpret
    /// it silently.
    pub fn validate(&self) -> Result<()> {
        match (&self.vector, &self.encoder, self.space_version) {
            (None, None, None) => Ok(()),
            (Some(v), Some(_), Some(_)) => {
                if v.is_empty() {
                    return Err(Error::invalid(format!(
                        "index entry {} carries an empty semantic vector",
                        self.id
                    )));
                }
                Ok(())
            }
            _ => Err(Error::invalid(format!(
                "index entry {} declares a semantic vector without a full \
                 (encoder, space_version) binding; a half-declared space is \
                 refused, not defaulted",
                self.id
            ))),
        }
    }

    pub fn semantic_space(&self) -> Option<(&str, i64)> {
        match (&self.encoder, self.space_version) {
            (Some(e), Some(v)) => Some((e.as_str(), v)),
            _ => None,
        }
    }
}

// ── the zio side: a real EvalContext driving the real libs ───────

/// A zio context with the core stdlib plus `memory.zio` / `vector.zio`
/// loaded. Building it is the Rust↔zio boundary; every index computation
/// below really runs the shipped library source.
pub fn memory_context() -> Result<EvalContext> {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = EvalContext::new(env);
    load_source(&ctx, "core.zio", zio_core::stdlib_source());
    for lib in ["memory.zio", "vector.zio"] {
        let path = lib_path(lib);
        let source = std::fs::read_to_string(&path).map_err(|e| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("read {lib}: {e}"),
            )
        })?;
        load_source(&ctx, lib, &source);
    }
    Ok(ctx)
}

fn lib_path(lib: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("learning crate lives below workspace root")
        .join("lib")
        .join("zio")
        .join(lib)
}

fn load_source(ctx: &EvalContext, name: &str, source: &str) {
    let source_id = ctx.source_map().register(name.into(), source.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(source, source_id)
        .expect("shipped library source must parse");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, ctx)
            .unwrap_or_else(|e| panic!("loading {name}: {e}"));
    }
}

fn eval_str(ctx: &EvalContext, src: &str) -> std::result::Result<Value, EvalError> {
    let source_id = ctx.source_map().register("memory-host".into(), src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(src, source_id)
        .map_err(|e| EvalError::custom(format!("parse error: {e}")))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = zio_core::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

/// Evaluate a zio expression against the loaded memory libraries and
/// return the raw [`Value`]. This is the escape hatch the contract test
/// uses to assert on library behaviour directly (probe-set bucketing,
/// macro expansion) without duplicating the logic in Rust.
pub fn eval_public(ctx: &EvalContext, src: &str) -> Value {
    eval_str(ctx, src).unwrap_or_else(|e| panic!("zio memory eval failed: {e}\n  source: {src}"))
}

// ── refusal 1: semantic near-neighbour cannot merge into a new AST ──

/// The structural merge. Two entries merge only when their ASTs are
/// byte-identical. Vector proximity is deliberately not consulted: a
/// near-neighbour is a *hint*, and a hint that could rewrite a program's
/// identity is exactly the failure this refuses.
pub fn merge_structural(ctx: &EvalContext, a: &ExperienceIndexEntry, b: &ExperienceIndexEntry) -> Result<ExperienceIndexEntry> {
    if a.ast != b.ast {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "refuse merge: semantic proximity cannot rewrite a different AST \
                 ({}: {:?} vs {}: {:?})",
                a.id, a.ast, b.id, b.ast
            ),
        ));
    }
    // The merge itself runs in the zio library so the rule has exactly
    // one implementation.
    let merged = eval_str(
        ctx,
        &format!(
            "(memory--merge-structural {{:memory/id {:?} :memory/ast {:?}}} \
             {{:memory/id {:?} :memory/ast {:?}}})",
            a.id, a.ast, b.id, b.ast
        ),
    )
    .map_err(eval_err)?;
    let new_id = value_string_field(&merged, "memory/id")
        .ok_or_else(|| Error::new(ErrorKind::BackendFailed, "merge produced no id"))?;
    Ok(ExperienceIndexEntry { id: new_id, ..a.clone() })
}

// ── refusal 2: different probe sets are never compared ───────────

/// Compare two behaviour fingerprints. Same probe set → a boolean.
/// Different probe set → an error, because a similarity across probe
/// sets would be a number that describes nothing.
pub fn behaviour_match(a: &ExperienceIndexEntry, b: &ExperienceIndexEntry) -> Result<bool> {
    if a.probe_set != b.probe_set {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "refuse compare: fingerprints are under different probe sets \
                 ({}: {:?} vs {}: {:?}); cross-probe-set similarity is not a measurement",
                a.id, a.probe_set, b.id, b.probe_set
            ),
        ));
    }
    Ok(a.fingerprint == b.fingerprint)
}

// ── refusal 3: retrieval cannot cross a data licence ─────────────

/// The licence gate. A retrieval returns nothing at all if any candidate
/// would cross a licence — an error, so a forbidden row can never be
/// mistaken for an empty result set.
pub fn check_retrievable(
    store: &Store,
    entry: &ExperienceIndexEntry,
) -> Result<()> {
    if !entry.usage_permitted {
        return Err(Error::denied(format!(
            "index entry {} carries usage_permitted=false; retrieval is refused",
            entry.id
        )));
    }
    for obs_id in &entry.source_observation_ids {
        let obs = store.get_observation(obs_id)?;
        if !obs.training_permitted {
            return Err(Error::denied(format!(
                "index entry {} derives from observation {obs_id}, which is not \
                 training-permitted; retrieval is refused",
                entry.id
            )));
        }
    }
    for sig_id in &entry.source_signal_ids {
        let status = store.signal_status(sig_id)?;
        if status != "accepted" {
            return Err(Error::denied(format!(
                "index entry {} derives from signal {sig_id}, whose licence is \
                 {status}; retrieval is refused",
                entry.id
            )));
        }
    }
    Ok(())
}

/// Licence resolution from the *store*, not from the entry's own copy:
/// a signal's live status is the fact, an entry's cached flag is a
/// claim. This reads the real store.
pub fn resolve_licence(
    store: &Store,
    entry: &ExperienceIndexEntry,
) -> Result<LicenseView> {
    check_retrievable(store, entry)?;
    let mut observation_permitted = true;
    for obs_id in &entry.source_observation_ids {
        let obs = store.get_observation(obs_id)?;
        if !obs.training_permitted {
            observation_permitted = false;
        }
    }
    Ok(LicenseView {
        entry_id: entry.id.clone(),
        usage_permitted: true,
        observation_permitted,
    })
}

/// The resolved licence of one index entry, read from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct LicenseView {
    pub entry_id: String,
    pub usage_permitted: bool,
    pub observation_permitted: bool,
}

// ── semantic index with a bound space ────────────────────────────

/// A semantic search. The query's `(encoder, space_version)` must match
/// the indexed entries', or the search is refused — changing the space
/// invalidates the index rather than silently reinterpreting old
/// coordinates. Real arithmetic runs in `lib/zio/vector.zio`.
pub fn semantic_nearest(
    ctx: &EvalContext,
    entries: &[ExperienceIndexEntry],
    query: &SemanticQuery,
) -> Result<SemanticHit> {
    for e in entries {
        e.validate()?;
    }
    let candidates: Vec<&ExperienceIndexEntry> =
        entries.iter().filter(|e| e.vector.is_some()).collect();
    if candidates.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "semantic query over an index with no vector entries",
        ));
    }
    // Pre-check the space in Rust so the refusal is a typed grove error
    // carrying which versions collided, then let the zio library do the
    // nearest-neighbour arithmetic under that same bound.
    for c in &candidates {
        match c.semantic_space() {
            Some((enc, ver)) if enc == query.encoder && ver == query.space_version => {}
            other => {
                return Err(Error::new(
                    ErrorKind::IncompatibleState,
                    format!(
                        "refuse semantic query: entry {} is in space {:?}, query is in \
                         ({}, {}); a space change invalidates the index unless it was \
                         migrated explicitly",
                        c.id, other, query.encoder, query.space_version
                    ),
                ))
            }
        }
    }
    let records_zio: Vec<String> = candidates
        .iter()
        .map(|e| {
            format!(
                "{{:memory/id {:?} :memory/encoder {:?} :memory/space-version {} :memory/vector {}}}",
                e.id,
                e.encoder.as_deref().unwrap_or_default(),
                e.space_version.unwrap_or_default(),
                vector_literal(e.vector.as_deref().unwrap_or(&[]))
            )
        })
        .collect();
    let src = format!(
        "(vector--nearest [{}] {{:memory/encoder {:?} :memory/space-version {} :memory/vector {}}} {})",
        records_zio.join(" "),
        query.encoder,
        query.space_version,
        vector_literal(&query.vector),
        query.space_version
    );
    let out = eval_str(ctx, &src).map_err(eval_err)?;
    parse_hit(&out)
}

fn vector_literal(v: &[i64]) -> String {
    let parts: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("[{}]", parts.join(" "))
}

/// A semantic query, bound to one space.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticQuery {
    pub vector: Vec<i64>,
    pub encoder: String,
    pub space_version: i64,
}

/// The nearest neighbour with its integer basis-point similarity.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticHit {
    pub id: String,
    pub similarity_bp: i64,
}

fn parse_hit(value: &Value) -> Result<SemanticHit> {
    // A refusal comes back as a string; a hit as a [bp record] pair.
    match value {
        Value::String(msg) => Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("semantic query refused: {msg}"),
        )),
        Value::List(items) | Value::Vector(items) => {
            let bp = match items.get(0) {
                Some(Value::Integer(i)) => *i,
                _ => {
                    return Err(Error::new(
                        ErrorKind::BackendFailed,
                        "semantic hit has no integer similarity",
                    ))
                }
            };
            let rec = items.get(1).ok_or_else(|| {
                Error::new(ErrorKind::BackendFailed, "semantic hit has no record")
            })?;
            let id = value_string_field(rec, "memory/id").ok_or_else(|| {
                Error::new(ErrorKind::BackendFailed, "semantic hit record has no id")
            })?;
            Ok(SemanticHit { id, similarity_bp: bp })
        }
        other => Err(Error::new(
            ErrorKind::BackendFailed,
            format!("semantic query returned an unexpected value: {other:?}"),
        )),
    }
}

fn value_string_field(value: &Value, field: &str) -> Option<String> {
    match value {
        Value::Map(m) => match m.get(&Value::Keyword(field.into())) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

// ── the durable index ────────────────────────────────────────────

/// A version-stamped index over the experience base. Entries are stored
/// through the existing store's manifest table (content-addressed +
/// version-checked) plus a small local table for the fields retrieval
/// needs to scan. Neither duplicates the experience records themselves.
pub struct MemoryIndex {
    ctx: EvalContext,
}

impl MemoryIndex {
    /// Build the index and its zio context.
    pub fn new() -> Result<Self> {
        Ok(Self { ctx: memory_context()? })
    }

    /// The zio context, so callers can run further zio-side checks.
    pub fn context(&self) -> &EvalContext {
        &self.ctx
    }

    /// Commit one index entry durably (bytes first, then the reference),
    /// exactly like any other grove record.
    pub fn commit(&self, store: &Store, actor: &Actor, entry: &ExperienceIndexEntry) -> Result<()> {
        actor.require(ActorRole::Operator, "committing an experience index entry")?;
        entry.validate()?;
        store.commit_manifest("ExperienceIndexEntry", &actor.id, entry)?;
        Ok(())
    }

    /// The structural index over the committed entries: exact-shape
    /// neighbours, licence-checked. A licence-crossing neighbour is an
    /// error, not a gap in the list.
    pub fn structural_search(
        &self,
        store: &Store,
        entries: &[ExperienceIndexEntry],
        query_ast: &str,
    ) -> Result<Vec<ExperienceIndexEntry>> {
        let shape = ast_shape(query_ast);
        let mut hits = Vec::new();
        for e in entries {
            if ast_shape(&e.ast) != shape {
                continue;
            }
            check_retrievable(store, e)?;
            hits.push(e.clone());
        }
        Ok(hits)
    }
}

/// The canonical shape of a program: the operator/arity skeleton with
/// names and literals erased. Mirrors `memory--ast-shape` so the Rust
/// pre-filter and the zio library agree on what "same shape" means.
pub fn ast_shape(ast: &str) -> String {
    let forms = zio_core::reader::reader::read_program(ast)
        .unwrap_or_else(|e| panic!("indexed program must parse: {e}"));
    if forms.len() != 1 {
        return format!("<{}-forms>", forms.len());
    }
    shape_of(&forms[0])
}

fn shape_of(sexp: &zio_core::sexp::Sexp) -> String {
    use zio_core::sexp::Sexp;
    match sexp {
        Sexp::List(items, _) => {
            let inner: Vec<String> = items.iter().map(shape_of).collect();
            format!("({})", inner.join(","))
        }
        Sexp::Vector(items, _) => {
            let inner: Vec<String> = items.iter().map(shape_of).collect();
            format!("[{}]", inner.join(","))
        }
        _ => "atom".to_string(),
    }
}

// ── abstraction extraction and gated promotion ───────────────────

/// A candidate abstraction: the shared call spine plus the function
/// definition that would ship with it. Ordinary functions are preferred
/// over macros; the macro route is only proposed for a deeper spine.
#[derive(Debug, Clone, PartialEq)]
pub struct AbstractionCandidate {
    /// The shared operator spine, e.g. `["+", "*"]`.
    pub spine: Vec<String>,
    /// The function definition (a zio form) that realises the spine.
    pub definition: String,
    /// The description length of the definition — the cost of *including*
    /// the library, not just of the call site.
    pub definition_dl: i64,
    /// How many leaf positions the abstraction binds. Programs sharing
    /// the spine have this many leaves in the same positions, which is
    /// what makes one definition cover all of them.
    pub leaf_count: usize,
    /// The programs it was discovered from.
    pub discovered_from: Vec<String>,
    /// The *task ids* it was discovered from. The new-task gate compares
    /// against these, not against the program sources — a task that fed
    /// discovery is a task that cannot also be the held-out validation.
    pub discovered_from_tasks: Vec<String>,
    /// True when the candidate is a macro and must be re-checked after
    /// expansion.
    pub is_macro: bool,
}

/// Extract the minimal common abstraction from several verified
/// programs. Runs the real `memory--common-abstraction` in the zio
/// library, so the rule has one implementation.
///
/// `task_ids` names the tasks these programs came from. They travel with
/// the candidate so the promotion gate can refuse a "new task" that was
/// actually part of discovery.
pub fn extract_abstraction(
    ctx: &EvalContext,
    programs: &[String],
    task_ids: &[String],
) -> Result<AbstractionCandidate> {
    if programs.len() < 2 {
        return Err(Error::invalid(
            "abstraction needs at least two verified programs to find a commonality",
        ));
    }
    // The programs are program *source*, so they are parsed here and
    // re-rendered as forms before being spliced into the zio call.
    // Splicing the raw text would be a reader hazard: the reader folds
    // `,` into the following symbol, and a hand-built literal is exactly
    // the place that trap bites.
    let literal: Vec<String> = programs
        .iter()
        .map(|p| {
            let forms = zio_core::reader::reader::read_program(p).map_err(|e| {
                Error::invalid(format!("program {p:?} does not parse: {e}"))
            })?;
            match forms.as_slice() {
                [one] => Ok(format!("'{}", one)),
                other => Err(Error::invalid(format!(
                    "program {p:?} must be exactly one form, got {}",
                    other.len()
                ))),
            }
        })
        .collect::<Result<Vec<String>>>()?;
    let spine_val = eval_str(
        ctx,
        &format!("(memory--common-abstraction [{}])", literal.join(" ")),
    )
    .map_err(eval_err)?;
    let spine = string_vec(&spine_val);
    if spine.is_empty() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "these verified programs share no call spine; there is no common abstraction",
        ));
    }
    // Ordinary function first: the definition is the spine applied
    // left-to-right to a parameter. A macro is only proposed deeper.
    let is_macro = spine.len() > 2;
    let macro_candidate = if is_macro {
        let m = eval_str(
            ctx,
            &format!(
                "(memory--macro-candidate [{}])",
                literal.join(" ")
            ),
        )
        .map_err(eval_err)?;
        match &m {
            Value::List(_) | Value::Vector(_) => true,
            _ => false,
        }
    } else {
        false
    };
    // Every discovery program must have the same *shape*, not merely the
    // same spine: the definition binds one parameter per leaf position,
    // and a program with a different arity would silently mis-bind.
    let leaf_count = leaves_of(&programs[0])?.len();
    for p in &programs[1..] {
        let n = leaves_of(p)?.len();
        if n != leaf_count {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!(
                    "programs on a shared spine disagree on leaf count \
                     ({} vs {}); the commonality is not a real abstraction",
                    leaf_count, n
                ),
            ));
        }
    }
    if leaf_count == 0 {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "a program with no leaves has no abstraction to extract",
        ));
    }

    let definition = build_function(&spine, programs)?;
    let definition_dl = count_nodes(&definition);
    Ok(AbstractionCandidate {
        spine,
        definition,
        definition_dl,
        leaf_count,
        discovered_from: programs.to_vec(),
        discovered_from_tasks: task_ids.to_vec(),
        is_macro: macro_candidate,
    })
}

/// Build the ordinary-function definition for a spine.
///
/// The spine is the operator chain and the leaves are its data. Since
/// every discovery program shares the spine, they also share the leaf
/// *positions*, so the abstraction is exactly: the operator chain with
/// one parameter per leaf. `(+ (* 2 x) 1)` and `(+ (* 3 y) 2)` both
/// become `(defn apply-spine [p0 p1 p2] (+ (* p0 p1) p2))` called as
/// `(apply-spine 2 x 1)` and `(apply-spine 3 y 2)` — which generalizes
/// to any program of that shape, and is what makes it reusable rather
/// than a rename of one program.
///
/// The *arity* of each nested call comes from the programs themselves,
/// not from the spine length: the spine says which operators, the
/// programs say how many arguments each one takes.
fn build_function(spine: &[String], programs: &[String]) -> Result<String> {
    // The arity of each nested call, read off the first program walking
    // the spine from the OUTERMOST call inwards — the same order the
    // spine names its operators. A call's argument 0 may itself be the
    // next call in the spine, and then it is NOT a leaf: only the
    // remaining arguments bind parameters.
    let mut arities: Vec<usize> = Vec::new();
    let mut form = first_form(&programs[0])?;
    while let Some(items) = call_items(&form) {
        let rest = &items[1..];
        // argument 0 continues the chain when it is a call itself
        let leads = matches!(rest.first(), Some(next) if call_items(next).is_some());
        arities.push(rest.len() - if leads { 1 } else { 0 });
        if !leads {
            break;
        }
        form = rest[0].clone();
    }
    if arities.len() != spine.len() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "spine has {} operators but the program nests {} calls; \
                 the shared shape is not what it looked like",
                spine.len(),
                arities.len()
            ),
        ));
    }
    let leaf_count: usize = arities.iter().sum();
    let params: Vec<String> = (0..leaf_count).map(|i| format!("p{i}")).collect();
    let mut idx = 0usize;
    // Innermost call first. A call that has a deeper call in the spine
    // takes it as its FIRST argument; the rest are parameters.
    let mut body = String::new();
    for (depth, (op, arity)) in spine.iter().zip(arities.iter()).enumerate().rev() {
        let mut args: Vec<String> = Vec::new();
        if depth + 1 < spine.len() {
            args.push(body.clone());
        }
        for _ in 0..*arity {
            args.push(
                params
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| "0".to_string()),
            );
            idx += 1;
        }
        body = format!("({op} {})", args.join(" "));
    }
    Ok(format!("(defn apply-spine [{}] {body})", params.join(" ")))
}

fn first_form(program: &str) -> Result<zio_core::sexp::Sexp> {
    zio_core::reader::reader::read_program(program)
        .map_err(|e| Error::invalid(format!("program {program:?} does not parse: {e}")))?
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid(format!("program {program:?} is empty")))
}

/// The items of a call form, or `None` for a leaf. A list whose head is
/// not a symbol is a datum, not a call.
fn call_items(sexp: &zio_core::sexp::Sexp) -> Option<Vec<zio_core::sexp::Sexp>> {
    match sexp {
        zio_core::sexp::Sexp::List(items, _) if !items.is_empty() => match items.get(0) {
            Some(zio_core::sexp::Sexp::Symbol(_, _)) => {
                Some(items.iter().cloned().collect())
            }
            _ => None,
        },
        _ => None,
    }
}

/// The leaf values of a program form, left to right.
///
/// Position 0 of a call form is its operator — part of the spine, not a
/// datum — so the walk skips it. For `(+ (* 2 x) 1)` the leaves are
/// `[2 x 1]`, exactly the arguments the abstraction binds.
fn leaves_of(program: &str) -> Result<Vec<String>> {
    use zio_core::sexp::Sexp;
    let forms = zio_core::reader::reader::read_program(program)
        .map_err(|e| Error::invalid(format!("program {program:?} does not parse: {e}")))?;
    let one = forms
        .into_iter()
        .next()
        .ok_or_else(|| Error::invalid(format!("program {program:?} is empty")))?;
    fn walk(sexp: &Sexp, out: &mut Vec<String>) {
        match sexp {
            Sexp::List(items, _) | Sexp::Vector(items, _) => {
                for (i, item) in items.iter().enumerate() {
                    if i == 0 && matches!(item, Sexp::Symbol(_, _)) {
                        continue; // the operator belongs to the spine
                    }
                    walk(item, out);
                }
            }
            other => out.push(other.to_string()),
        }
    }
    let mut out = Vec::new();
    walk(&one, &mut out);
    Ok(out)
}

/// The real call site for one program: the abstraction's definition with
/// that program's leaves substituted for the parameters. This is what
/// reuse actually costs, and it is what gets evaluated.
pub fn instantiate(candidate: &AbstractionCandidate, program: &str) -> Result<String> {
    let leaves = leaves_of(program)?;
    if leaves.len() != candidate.leaf_count {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "program {program:?} has {} leaves, the abstraction binds {}",
                leaves.len(),
                candidate.leaf_count
            ),
        ));
    }
    Ok(format!("(apply-spine {})", leaves.join(" ")))
}

/// A fresh zio context with `definition` installed, for evaluating the
/// abstracted call sites against the original programs.
pub fn with_abstraction(base: &AbstractionCandidate) -> Result<EvalContext> {
    let ctx = memory_context()?;
    eval_str(&ctx, &base.definition).map_err(eval_err)?;
    Ok(ctx)
}

/// Run the original program and its abstracted call site on the same
/// bindings in the same interpreter. `true` means the abstraction really
/// is the program — the regression a promotion gate needs.
pub fn regression_holds(
    base: &AbstractionCandidate,
    program: &str,
) -> Result<bool> {
    let ctx = with_abstraction(base)?;
    let call = instantiate(base, program)?;
    // The bindings come from the ORIGINAL program, so both sides see
    // the same values for the same variables.
    let leaves = leaves_of(program)?;
    let want = eval_bound(&ctx, program, &leaves)?;
    let got = eval_bound(&ctx, &call, &leaves)?;
    Ok(want == got)
}

/// Evaluate `form` with every free symbol bound to its leaf index, so
/// both the original and the abstracted form run on identical values.
fn eval_bound(ctx: &EvalContext, form: &str, leaves: &[String]) -> Result<i64> {
    let mut bindings: Vec<String> = Vec::new();
    for (i, leaf) in leaves.iter().enumerate() {
        if leaf
            .chars()
            .next()
            .map(|c| c.is_alphabetic() || c == '_')
            .unwrap_or(false)
            && !leaf.contains('-')
        {
            bindings.push(format!("(def {leaf} {})", i + 1));
        }
    }
    let out = eval_str(
        ctx,
        &format!(
            "(do {} (try (eval {}) (catch any :error)))",
            bindings.join(" "),
            form
        ),
    )
    .map_err(eval_err)?;
    match out {
        Value::Integer(i) => Ok(i),
        other => Err(Error::new(
            ErrorKind::BackendFailed,
            format!("{form} produced {other:?}, not an integer"),
        )),
    }
}

/// Count a form's total nodes with the *library's* counter, so the
/// before/after numbers in a [`DescriptionLengthReport`] are one
/// measure rather than two vocabularies.
fn zio_count(ctx: &EvalContext, form: &str) -> Result<i64> {
    match eval_str(
        ctx,
        &format!("(count (read-string {:?}))", form),
    )
    .map_err(eval_err)? {
        Value::Integer(i) => Ok(i),
        other => Err(Error::new(
            ErrorKind::BackendFailed,
            format!("count did not return an integer for {form}: {other:?}"),
        )),
    }
}

/// Count the top-level nodes of a form's definition — the description
/// length of the abstraction itself.
fn count_nodes(form: &str) -> i64 {
    zio_core::reader::reader::read_program(form)
        .map(|forms| {
            forms
                .iter()
                .map(|f| count_sexp_nodes(f) as i64)
                .sum::<i64>()
        })
        .unwrap_or(0)
}

fn count_sexp_nodes(sexp: &zio_core::sexp::Sexp) -> usize {
    use zio_core::sexp::Sexp;
    match sexp {
        Sexp::List(items, _) | Sexp::Vector(items, _) => {
            1 + items.iter().map(count_sexp_nodes).sum::<usize>()
        }
        Sexp::Map(m, _) => 1 + m.iter().map(|(_, v)| count_sexp_nodes(v)).sum::<usize>(),
        _ => 1,
    }
}

/// A reusable task abstraction: the total description length is the
/// call-site length PLUS the library definition length. Reporting only
/// the call site would hide the definition in a function name, which is
/// the exact thing the plan forbids.
#[derive(Debug, Clone, PartialEq)]
pub struct DescriptionLengthReport {
    /// Call-site description length before reuse (no abstraction).
    pub before_dl: i64,
    /// Call-site description length after replacing with the abstraction.
    pub after_call_dl: i64,
    /// The abstraction's own definition cost.
    pub definition_dl: i64,
    /// Honest total = after_call_dl + definition_dl.
    pub total_dl: i64,
    /// True when including the library actually pays for itself.
    pub net_saving: bool,
}

/// Measure reuse on task programs that did **not** participate in
/// abstraction discovery. The real numbers come from the real zio
/// computation, not from "the name got shorter".
///
/// Several call sites are the interesting case: the definition is paid
/// for ONCE, so the honest total is (sum of the abstracted call sites)
/// + the definition, and whether that beats the raw programs is a
/// RESULT to be measured, not a win to be assumed.
pub fn measure_reuse(
    ctx: &EvalContext,
    candidate: &AbstractionCandidate,
    fresh_task_bodies: &[&str],
) -> Result<DescriptionLengthReport> {
    if fresh_task_bodies.is_empty() {
        return Err(Error::invalid("reuse measurement needs at least one task body"));
    }
    // Before: the raw programs, counted with the SAME counter the
    // library uses, so before/after are one measure and not two
    // vocabularies.
    let mut before_dl = 0i64;
    for body in fresh_task_bodies {
        before_dl += zio_count(ctx, body)?;
    }
    // After: the real instantiated calls, with the actual substituted
    // arguments — not an assumed "one call" placeholder.
    let mut calls = Vec::with_capacity(fresh_task_bodies.len());
    for body in fresh_task_bodies {
        calls.push(instantiate(candidate, body)?);
    }
    let call_src = format!(
        "[{}]",
        calls
            .iter()
            .map(|c| format!("(read-string {c:?})"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let call_dl = eval_str(
        ctx,
        &format!(
            "(reduce (fn [acc c] (+ acc (count c))) 0 {})",
            call_src
        ),
    )
    .map_err(eval_err)?;
    let after_call_dl = as_int(&call_dl, "call-site node count")?;
    // The honest total adds the abstraction's own definition ONCE, on
    // top of every call site. The addition itself is the library's
    // `memory--description-length` — charging the definition is the
    // whole point of the measurement, and doing it here means a caller
    // cannot report the call sites and quietly drop the library.
    let total_val = eval_str(
        ctx,
        &format!(
            "(memory--description-length {} {})",
            call_src, candidate.definition_dl
        ),
    )
    .map_err(eval_err)?;
    let total_dl = as_int(&total_val, "total description length")?;
    Ok(DescriptionLengthReport {
        before_dl,
        after_call_dl,
        definition_dl: candidate.definition_dl,
        total_dl,
        net_saving: total_dl < before_dl,
    })
}

fn as_int(value: &Value, what: &str) -> Result<i64> {
    match value {
        Value::Integer(i) => Ok(*i),
        other => Err(Error::new(
            ErrorKind::BackendFailed,
            format!("{what} did not return an integer: {other:?}"),
        )),
    }
}

fn string_vec(value: &Value) -> Vec<String> {
    match value {
        Value::List(items) | Value::Vector(items) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => format!("{other}"),
            })
            .collect(),
        _ => Vec::new(),
    }
}

// ── refusal 4 + gated promotion ──────────────────────────────────

/// The evidence a promotion must be measured against.
///
/// Both gates name real programs, and both are *checked by evaluation*
/// inside [`promote_abstraction`] — a flag the caller sets proves
/// nothing, so the caller supplies the programs and the host runs them.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionEvidence {
    /// The discovery programs the abstraction must still reproduce.
    /// A candidate that no longer matches its own discovery set is not
    /// an abstraction of it.
    pub regression_programs: Vec<String>,
    /// The NEW task's program — one the abstraction never saw, and
    /// never will again. Empty means no new-task evidence, and that is
    /// a refusal, not a pass.
    pub new_task_id: String,
    pub new_task_program: String,
}

/// A promoted capability: an abstraction that passed regression and a
/// new task, and whose source licences are all still in force.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotedCapability {
    pub schema: SchemaVersion,
    pub id: String,
    pub spine: Vec<String>,
    pub definition: String,
    pub definition_dl: i64,
    pub discovered_from: Vec<String>,
    pub validated_on: String,
    pub promoted_at_ms: i64,
}

/// Promote a candidate abstraction to a capability module — but only
/// after it passes regression **and** a new task, **and** only when every
/// source experience is still licensed for use. Any failure is a refusal,
/// not a partial promotion.
pub fn promote_abstraction(
    store: &Store,
    actor: &Actor,
    candidate: &AbstractionCandidate,
    sources: &[ExperienceIndexEntry],
    evidence: &PromotionEvidence,
    now_ms: i64,
) -> Result<PromotedCapability> {
    actor.require(ActorRole::Operator, "promoting an abstraction to a capability")?;

    // Gate (b) is checked first because it is the one that cannot be
    // faked by a caller-supplied flag: a "new task" that is actually a
    // discovery task would make the whole promotion circular.
    // The comparison is against task *ids*, not program sources — an
    // abstraction that merely re-solves its own discovery set is not
    // reuse, and checking the wrong field would let that pass.
    if evidence.new_task_id.is_empty()
        || evidence.new_task_program.is_empty()
        || candidate
            .discovered_from_tasks
            .contains(&evidence.new_task_id)
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "promotion needs evaluation on a NEW task with a real program, \
                 not a discovery task (asked for {:?}, discovered from {:?})",
                evidence.new_task_id, candidate.discovered_from_tasks
            ),
        ));
    }

    // Gates (a) and (b') are *measured*: the abstracted call site and
    // the original program are both evaluated in the same interpreter
    // with the abstraction installed, and the results must match.
    // Building the context here also proves the definition is a real,
    // installable definition — an unparseable one fails before any gate.
    with_abstraction(candidate)?;
    for program in &evidence.regression_programs {
        if !regression_holds(candidate, program)? {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!(
                    "candidate abstraction does not reproduce its own discovery \
                     program {program:?}; it fails regression and is not promoted"
                ),
            ));
        }
    }
    if !regression_holds(candidate, &evidence.new_task_program)? {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "candidate abstraction does not reproduce the new task {}; \
                 it is not promoted",
                evidence.new_task_id
            ),
        ));
    }
    // Refusal 4: a source that is no longer in force blocks the load, so
    // a capability derived from retracted knowledge refuses to promote
    // rather than silently serving stale ability.
    for src in sources {
        check_retrievable(store, src)?;
    }

    let capability = PromotedCapability {
        schema: SCHEMA_VERSION,
        id: format!("cap-{}", candidate.spine.join("-")),
        spine: candidate.spine.clone(),
        definition: candidate.definition.clone(),
        definition_dl: candidate.definition_dl,
        discovered_from: candidate.discovered_from.clone(),
        validated_on: evidence.new_task_id.clone(),
        promoted_at_ms: now_ms,
    };
    store.commit_manifest("PromotedCapability", &actor.id, &capability)?;
    Ok(capability)
}

// ── convenience for observation construction in tests ────────────

/// A minimal observation whose training permission and modality mask are
/// explicit, for building a fact base to index over.
pub fn observation(
    id: &str,
    task_id: &str,
    source: &str,
    training_permitted: bool,
) -> Observation {
    Observation {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        owner: "trainer".to_string(),
        task_id: task_id.to_string(),
        session_id: "s-1".to_string(),
        source: source.to_string(),
        occurred_at_ms: 0,
        blocks: vec![],
        modality_mask: vec![],
        training_permitted,
    }
}

fn eval_err(e: EvalError) -> Error {
    Error::new(ErrorKind::BackendFailed, format!("zio memory lib: {e}"))
}
