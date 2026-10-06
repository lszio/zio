//! T00 runtime contract: the shared assembly and numeric semantics the
//! CLI, the compiler front end, and Grove all depend on.
//!
//! Every check here is a consumer-visible failure: a value the program
//! must print, or an error it must refuse. Nothing compares source text.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bootstrap::{eval_source, language_context, ModuleRoots};
use zio_core::context::EvalRuntime;
use zio_core::error::EvalError;
use zio_core::value::Value;

// ── numeric comparison ────────────────────────────────────────────

fn run(src: &str) -> Result<Value, EvalError> {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    eval_source(&ctx, "probe", src)
}

#[test]
fn mixed_number_comparison_keeps_integers_exact() {
    // The three fixed results. `9007199254740993 > 9007199254740992.0`
    // fails when every i64 is widened to f64 first: both sides then read
    // as 9007199254740992.0.
    for (src, expected) in [
        ("(< 1.5 2.0)", "true"),
        ("(> 9007199254740993 9007199254740992.0)", "true"),
        ("(= (+ 1 2) 3)", "true"),
    ] {
        let value = run(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
        assert_eq!(value.to_string(), expected, "{src} produced {value}");
    }
}

#[test]
fn integer_comparison_stays_integral() {
    for (src, expected) in [
        ("(< 9007199254740993 9007199254740994)", "true"),
        ("(>= -1 0)", "false"),
        ("(<= 2.5 2.5)", "true"),
        ("(< 1 1.0)", "false"),
        ("(> 9007199254740993 9007199254740993.0)", "true"),
        ("(< -9007199254740993 -9007199254740992.0)", "true"),
    ] {
        let value = run(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
        assert_eq!(value.to_string(), expected, "{src} produced {value}");
    }
}

#[test]
fn mixed_ordering_respects_operand_direction_and_integer_boundaries() {
    use std::cmp::Ordering::{Equal, Greater, Less};

    for (float, integer, order) in [
        ("0.95", "0", Greater),
        ("-1.5", "-1", Less),
        ("-1.5", "-2", Greater),
        ("2.0", "2", Equal),
        ("9007199254740992.0", "9007199254740993", Less),
        ("9223372036854775808.0", "9223372036854775807", Greater),
        ("-9223372036854775808.0", "-9223372036854775808", Equal),
        ("-9223372036854777856.0", "-9223372036854775808", Less),
    ] {
        for (op, expected, reverse_expected) in [
            ("<", order == Less, order == Greater),
            (">", order == Greater, order == Less),
            ("<=", order != Greater, order != Less),
            (">=", order != Less, order != Greater),
        ] {
            for (left, right, expected) in [
                (float, integer, expected),
                (integer, float, reverse_expected),
            ] {
                let source = format!("({op} {left} {right})");
                assert_eq!(run(&source).unwrap(), Value::Boolean(expected), "{source}");
            }
        }
    }
}

#[test]
fn float_ordering_rejects_non_finite_operands() {
    // `(/ 0.0)` is unary float division, so this is a real infinity.
    let err = run("(< (/ 0.0) 1.0)").expect_err("infinity must not order");
    assert!(
        err.to_string().contains("finite number"),
        "unexpected error: {err}"
    );
}

#[test]
fn non_numeric_ordering_is_a_type_error() {
    let err = run("(< \"a\" \"b\")").expect_err("strings are not ordered");
    assert!(err.to_string().contains("type error"), "unexpected error: {err}");
}

#[test]
fn integer_division_contract_is_preserved() {
    assert_eq!(run("(/ 7 2)").unwrap().to_string(), "3");
    assert_eq!(run("(/ 6 3)").unwrap().to_string(), "2");
    assert_eq!(run("(/ 1)").unwrap().to_string(), "1");
    let err = run("(/ 1 0)").expect_err("integer division by zero");
    assert!(err.to_string().contains("division by zero"), "unexpected: {err}");
}

// ── scalar sqrt ───────────────────────────────────────────────────

#[test]
fn sqrt_is_a_scalar_primitive() {
    assert_eq!(run("(sqrt 4.0)").unwrap().to_string(), "2");
    assert_eq!(run("(sqrt 9)").unwrap().to_string(), "3");
    assert_eq!(run("(sqrt 0)").unwrap().to_string(), "0");

    for src in ["(sqrt -1.0)", "(sqrt -1)", "(sqrt (/ 0.0))", "(sqrt \"4\")"] {
        let err = run(src).expect_err(&format!("{src} must be rejected"));
        assert!(!err.to_string().is_empty(), "{src} rejected with an empty error");
    }
    let err = run("(sqrt 1 2)").expect_err("sqrt takes one argument");
    assert!(err.to_string().contains("argument count"), "unexpected: {err}");
}

// ── assembly ──────────────────────────────────────────────────────

#[test]
fn bootstrap_installs_the_core_standard_library() {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    assert_eq!(eval_source(&ctx, "probe", "(inc 41)").unwrap().to_string(), "42");
    assert_eq!(
        eval_source(&ctx, "probe", "(count (take 2 [1 2 3]))").unwrap().to_string(),
        "2"
    );
}

#[test]
fn stdlib_assembly_failure_is_an_error_not_a_warning() {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    let err = zio_core::bootstrap::bootstrap_source(&ctx, "broken.zio", "(defn")
        .expect_err("a half-parsed stdlib must fail the bootstrap");
    assert!(
        err.to_string().contains("broken.zio"),
        "error must name the source: {err}"
    );
}

#[test]
fn eval_source_reports_the_failing_line() {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    let err = eval_source(&ctx, "lines.zio", "(println \"ok\")\n(undefined-name)")
        .expect_err("undefined symbol must fail");
    let span = expect_span(&err);
    assert_eq!(span.line, 2, "unexpected location in {err}");
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("zio_runtime_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        Fixture { dir }
    }

    fn write(&self, rel: &str, body: &str) -> PathBuf {
        let path = self.dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, body).expect("write fixture file");
        path
    }

    fn roots(&self) -> ModuleRoots {
        ModuleRoots::new(vec![self.dir.clone()]).expect("granted roots")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn nested_require_shares_source_registration() {
    let fx = Fixture::new("nested");
    fx.write(
        "outer.zio",
        "(require :inner :refer [double])\n(defn twice [x] (double x))\n(export twice)\n",
    );
    fx.write("inner.zio", "(defn double [x] (* 2 x))\n(export double)\n");

    let ctx = language_context(fx.roots()).expect("bootstrap");
    let value = eval_source(&ctx, "script.zio", "(require :outer)\n(outer/twice 21)")
        .expect("outer must load");
    assert_eq!(value.to_string(), "42");

    // The required module's body lives in the context's own SourceMap:
    // a runtime error inside it names the module file, not the script.
    fx.write("boom.zio", "(defn fail [] (undefined-name))\n(export fail)\n");
    let err = eval_source(&ctx, "script.zio", "(require :boom)\n(boom/fail)")
        .expect_err("undefined symbol inside module must fail");
    let span = expect_span(&err);
    assert_eq!(span.line, 1, "error must point at the module body");
    assert_eq!(
        ctx.source_map().source_name(span.source_id),
        fx.dir.join("boom.zio").to_string_lossy().into_owned(),
        "error must be registered under the module's own source"
    );
}

fn expect_span(err: &EvalError) -> zio_core::span::Span {
    let ctx = match err {
        EvalError::SymbolNotFound(_, c)
        | EvalError::NotAFunction { span: c, .. }
        | EvalError::TypeError { span: c, .. }
        | EvalError::InvalidForm(_, c)
        | EvalError::MacroError(_, c)
        | EvalError::Custom(_, c) => c,
        other => panic!("expected a located error, got {other}"),
    };
    ctx.span.expect("error must carry a source location")
}

#[cfg(unix)]
#[test]
fn granted_roots_reject_a_module_escaping_them() {
    let fx = Fixture::new("escape");
    fx.write("allowed/ok.zio", "(defn one [] 1)\n(export one)\n");
    fx.write("outside/secret.zio", "(defn leak [] :secret)\n(export leak)\n");
    std::os::unix::fs::symlink(
        fx.dir.join("outside").join("secret.zio"),
        fx.dir.join("allowed").join("leak.zio"),
    )
    .expect("symlink escape fixture");

    let ctx = language_context(fx.roots()).expect("bootstrap");
    let err = eval_source(&ctx, "script.zio", "(require :leak)")
        .expect_err("a symlink out of the granted roots must be refused");
    assert!(
        err.to_string().contains("granted roots"),
        "unexpected error: {err}"
    );
}

#[test]
fn ungranted_module_is_not_found() {
    let fx = Fixture::new("ungranted");
    fx.write("allowed/kept.zio", "(defn one [] 1)\n(export one)\n");
    let empty = Fixture::new("empty_roots");

    let ctx = language_context(empty.roots()).expect("bootstrap");
    let err = eval_source(&ctx, "script.zio", "(require :kept)")
        .expect_err("a module outside every granted root must be refused");
    assert!(
        err.to_string().contains("module not found"),
        "unexpected error: {err}"
    );
    let _ = fx;
}

// ── source-preserving syntax bridge ───────────────────────────────

#[test]
fn syntax_bridge_round_trips_spans_and_origins() {
    use zio_core::sexp::Sexp;
    use zio_core::syntax::{Origin, SyntaxNode};

    let sm = Arc::new(zio_core::span::SourceMap::new());
    let id = sm.register("prog.zio".into(), "(defn f [x] (g x))".into());
    let span = zio_core::span::Span::new(id, zio_core::span::BytePos(0), zio_core::span::BytePos(17), 1, 1);
    let form = Sexp::List(
        im::Vector::from(vec![
            Sexp::Symbol("defn".into(), Some(span)),
            Sexp::Symbol("f".into(), Some(span)),
            Sexp::Vector(
                im::Vector::from(vec![Sexp::Symbol("x".into(), Some(span))]),
                Some(span),
            ),
            Sexp::List(
                im::Vector::from(vec![
                    Sexp::Symbol("g".into(), Some(span)),
                    Sexp::Symbol("x".into(), Some(span)),
                ]),
                Some(span),
            ),
        ]),
        Some(span),
    );

    let node = SyntaxNode::from_sexp(&form);
    let tagged = node.to_value();
    let back = SyntaxNode::from_value(&tagged).expect("tagged map must convert back");
    assert_eq!(back.to_sexp(), form, "spans must survive the round trip");
    assert_eq!(back.origin, Origin::Source);
    assert_eq!(back.to_value(), tagged, "tagging must be stable");

    // Quoted data, generated nodes, and macro expansions stay apart.
    let quoted = SyntaxNode::from_sexp(&form).with_origin(Origin::Quoted);
    assert!(quoted.to_value().to_string().contains("quoted"));
    let generated = SyntaxNode::from_sexp(&form).with_origin(Origin::Generated {
        by: "expand".into(),
    });
    assert!(generated.to_value().to_string().contains("expand"));
    let expanded = SyntaxNode::from_sexp(&form).with_origin(Origin::MacroCall {
        macro_name: "unless".into(),
        call_site: Some(span),
    });
    let restored = SyntaxNode::from_value(&expanded.to_value()).expect("macro origin survives");
    assert_eq!(restored.origin, expanded.origin);
}

#[test]
fn syntax_bridge_refuses_values_it_cannot_prove_are_syntax() {
    use zio_core::syntax::SyntaxNode;
    use zio_core::value::Value;

    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    // An ordinary list is data, not a tagged syntax node.
    let plain = eval_source(&ctx, "probe", "(list 1 2 3)").unwrap();
    assert!(SyntaxNode::from_value(&plain).is_none());

    // A map that claims to be syntax but lies about its shape is refused
    // rather than silently treated as a node.
    let lying = eval_source(&ctx, "probe", "(assoc {} :kind :list :value 42)").unwrap();
    assert!(SyntaxNode::from_value(&lying).is_none());
    let wrong_children = eval_source(
        &ctx,
        "probe",
        "(assoc (assoc {} :kind :list :value nil :children nil :span nil :origin nil) :children 7)",
    )
    .unwrap();
    assert!(SyntaxNode::from_value(&wrong_children).is_none());

    let not_a_map = Value::Integer(7);
    assert!(SyntaxNode::from_value(&not_a_map).is_none());
}

// ── helper kept honest ────────────────────────────────────────────

#[test]
fn fixture_dir_is_a_real_directory() {
    let fx = Fixture::new("helper");
    assert!(Path::new(&fx.dir).is_dir());
}
