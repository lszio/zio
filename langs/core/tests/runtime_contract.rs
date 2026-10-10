//! T00 runtime contract: the shared assembly and numeric semantics the
//! CLI, the compiler front end, and Grove all depend on.
//!
//! Every check here is a consumer-visible failure: a value the program
//! must print, or an error it must refuse. Nothing compares source text.

use std::path::PathBuf;
use std::sync::Arc;

use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
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
    assert!(
        err.to_string().contains("type error"),
        "unexpected error: {err}"
    );
}

#[test]
fn integer_division_contract_is_preserved() {
    assert_eq!(run("(/ 7 2)").unwrap().to_string(), "3");
    assert_eq!(run("(/ 6 3)").unwrap().to_string(), "2");
    assert_eq!(run("(/ 1)").unwrap().to_string(), "1");
    let err = run("(/ 1 0)").expect_err("integer division by zero");
    assert!(
        err.to_string().contains("division by zero"),
        "unexpected: {err}"
    );
}

// ── scalar sqrt ───────────────────────────────────────────────────

#[test]
fn sqrt_is_a_scalar_primitive() {
    assert_eq!(run("(sqrt 4.0)").unwrap().to_string(), "2");
    assert_eq!(run("(sqrt 9)").unwrap().to_string(), "3");
    assert_eq!(run("(sqrt 0)").unwrap().to_string(), "0");

    for src in ["(sqrt -1.0)", "(sqrt -1)", "(sqrt (/ 0.0))", "(sqrt \"4\")"] {
        let err = run(src).expect_err(&format!("{src} must be rejected"));
        assert!(
            !err.to_string().is_empty(),
            "{src} rejected with an empty error"
        );
    }
    let err = run("(sqrt 1 2)").expect_err("sqrt takes one argument");
    assert!(
        err.to_string().contains("argument count"),
        "unexpected: {err}"
    );
}

// ── assembly ──────────────────────────────────────────────────────

#[test]
fn bootstrap_installs_the_core_standard_library() {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    assert_eq!(
        eval_source(&ctx, "probe", "(inc 41)").unwrap().to_string(),
        "42"
    );
    assert_eq!(
        eval_source(&ctx, "probe", "(count (take 2 [1 2 3]))")
            .unwrap()
            .to_string(),
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
    fx.write(
        "boom.zio",
        "(defn fail [] (undefined-name))\n(export fail)\n",
    );
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
    fx.write(
        "outside/secret.zio",
        "(defn leak [] :secret)\n(export leak)\n",
    );
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
    let span = zio_core::span::Span::new(
        id,
        zio_core::span::BytePos(0),
        zio_core::span::BytePos(17),
        1,
        1,
    );
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
    assert_eq!(
        back.to_sexp().unwrap(),
        form,
        "spans must survive the round trip"
    );
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

#[test]
fn json_codec_refuses_lossy_boundaries_and_preserves_json_values() {
    use zio_core::builtins::json::{from_json, to_json};
    assert_eq!(
        run("(nil? (json-parse \"null\"))").unwrap(),
        Value::Boolean(true)
    );
    for text in ["42 43", "(+ 1 2)", "nil", "NaN", "1e999", "{\"x\":}"] {
        let source = format!("(json-parse {})", serde_json::to_string(text).unwrap());
        assert!(run(&source).is_err(), "{text}");
    }
    let text = "\0\u{0001}\n\"\\😀";
    let encoded = to_json(&Value::String(text.into())).unwrap();
    assert_eq!(from_json(encoded).unwrap(), Value::String(text.into()));
    assert!(to_json(&Value::Float(f64::INFINITY)).is_err());
    // A keyword in MAP-VALUE position is tagged, because nothing else
    // in that position distinguishes it from a string. In ARRAY position
    // it is unambiguous, so it encodes as its plain string — which is
    // what every existing consumer of `json-stringify` already reads,
    // and tagging it would break `[:teacher-label]`.
    let mut with_keyword = im::HashMap::new();
    with_keyword.insert(
        Value::String("kind".into()),
        Value::Keyword("teacher-label".into()),
    );
    let encoded = to_json(&Value::Map(with_keyword)).expect("should encode");
    assert_eq!(
        encoded
            .get("kind")
            .and_then(|v| v.get("$keyword"))
            .and_then(|v| v.as_str()),
        Some("teacher-label"),
        "a keyword in value position must be tagged: {encoded}"
    );
    assert_eq!(
        to_json(&Value::Vector(im::vector![Value::Keyword("a".into())])).unwrap(),
        serde_json::json!(["a"]),
        "a keyword in array position is unambiguous and stays a string"
    );
    // A key using the reserved tag prefix is refused: it would decode
    // back as a tagged value rather than as a map.
    let mut reserved = im::HashMap::new();
    reserved.insert(
        Value::String("$keyword".into()),
        Value::String("sneaky".into()),
    );
    assert!(to_json(&Value::Map(reserved)).is_err());
    assert!(from_json(serde_json::json!(u64::MAX)).is_err());
    let mut map = im::HashMap::new();
    map.insert(Value::Keyword("x".into()), Value::Integer(1));
    map.insert(Value::String("x".into()), Value::Integer(2));
    assert!(to_json(&Value::Map(map)).is_err());
}

#[test]
fn pure_inspection_locates_malformed_input_without_executing() {
    use zio_core::span::SourceMap;
    use zio_core::syntax::inspect_source;
    let map = SourceMap::new();
    for (source, start, end) in [
        ("\"unterminated", 0, 13),
        ("' ;nothing", 0, 1),
        ("[1)", 2, 3),
        ("#\\unknown", 0, 9),
        ("{:x}", 0, 4),
    ] {
        let err = inspect_source(&map, "malformed.zio", source).unwrap_err();
        let span = err.span().expect("located reader error");
        assert_eq!((span.start.0, span.end.0), (start, end), "{source}");
        assert_eq!(map.source_name(span.source_id), "malformed.zio");
    }
    let nodes = inspect_source(&map, "safe.zio", "(spit \"never\" \"written\") #\\界").unwrap();
    assert_eq!(
        nodes[1].to_sexp().unwrap(),
        zio_core::sexp::Sexp::Char('界', nodes[1].span)
    );
    let ctx = language_context(ModuleRoots::empty()).unwrap();
    eval_source(
        &ctx,
        "inspect.zio",
        "(read-syntax \"(def executed 1)\" \"document.zio\")",
    )
    .unwrap();
    assert!(ctx.env.get("executed").is_none());
}

#[test]
fn forged_syntax_never_defaults_or_truncates() {
    use zio_core::syntax::{NodeKind, Origin, SyntaxNode};
    let node = SyntaxNode {
        kind: NodeKind::Integer,
        value: Some("bad".into()),
        children: im::Vector::new(),
        span: None,
        origin: Origin::Source,
    };
    assert!(node.to_sexp().is_err());
    assert!(SyntaxNode::from_value(&node.to_value()).is_none());
    let mut map_node = node.clone();
    map_node.kind = NodeKind::Map;
    map_node.value = None;
    map_node.children.push_back(SyntaxNode {
        kind: NodeKind::Integer,
        value: Some("1".into()),
        ..node
    });
    assert!(map_node.to_sexp().is_err());
    let valid = zio_core::syntax::inspect_source(&zio_core::span::SourceMap::new(), "x", "42")
        .unwrap()
        .remove(0);
    let Value::Map(mut tagged) = valid.to_value() else {
        unreachable!()
    };
    let Value::Map(mut span) = tagged.get(&Value::Keyword("span".into())).unwrap().clone() else {
        unreachable!()
    };
    span.insert(Value::Keyword("start".into()), Value::Integer(-1));
    tagged.insert(Value::Keyword("span".into()), Value::Map(span));
    assert!(SyntaxNode::from_value(&Value::Map(tagged)).is_none());
}

#[test]
fn virtual_require_shares_io_cache_cycles_and_cleans_failures() {
    use zio_core::bootstrap::language_context_with_io;
    use zio_core::io::{BufferIoHost, IoHost};
    let io = Arc::new(BufferIoHost::with_files(vec![
        (
            "/modules/inner.zio".into(),
            "(println \"loaded\") (def answer 42) (export answer)".into(),
        ),
        (
            "/modules/outer.zio".into(),
            "(require :inner) (def result answer) (export result)".into(),
        ),
        ("/modules/a.zio".into(), "(require :b)".into()),
        ("/modules/b.zio".into(), "(require :a)".into()),
        ("/modules/bad.zio".into(), "(missing)".into()),
        ("/modules/hidden.zio".into(), "(def secret 7)".into()),
        ("/modules/inherited.zio".into(), "(export inc)".into()),
    ]));
    let roots = ModuleRoots::new_with_io(vec!["/modules".into()], io.as_ref()).unwrap();
    let ctx = language_context_with_io(roots, io.clone()).unwrap();
    assert_eq!(
        eval_source(
            &ctx,
            "main",
            "(require :outer) (require :inner) outer/result"
        )
        .unwrap(),
        Value::Integer(42)
    );
    assert_eq!(io.get_output(), "loaded\n");
    assert!(eval_source(&ctx, "main", "(require :a)").is_err());
    assert!(ctx.modules.borrow().loading_stack.is_empty());
    assert!(eval_source(&ctx, "main", "(require :bad)").is_err());
    assert!(ctx.modules.borrow().loading_stack.is_empty());
    io.write_file("/modules/bad.zio", "(def fixed 9) (export fixed)")
        .unwrap();
    assert_eq!(
        eval_source(&ctx, "main", "(require :bad) bad/fixed").unwrap(),
        Value::Integer(9)
    );
    assert!(eval_source(&ctx, "main", "(require :hidden :refer [secret])").is_err());
    assert!(eval_source(&ctx, "main", "hidden/secret").is_err());
    assert!(eval_source(&ctx, "main", "(require :inherited)").is_err());
    assert!(eval_source(&ctx, "main", "(require :../secret)").is_err());
}

// ── reader literals: sets and quasiquote ──────────────────────────

#[test]
fn set_literal_is_a_map_backed_set_from_the_bootstrap() {
    for (src, expected) in [
        ("(count #{1 2 3})", "3"),
        ("(count #{1 1 2})", "2"),
        ("(get #{:a :b} :a)", "true"),
        ("(get #{:a :b} :z nil)", "nil"),
        ("(str #{})", "\"{}\""),
        // Map-backed equality is order-insensitive: the same members are
        // the same set, however the literal was written.
        ("(= #{1 2 3} #{3 2 1})", "true"),
        ("(= #{1 2} #{1 2 3})", "false"),
    ] {
        let value = run(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
        assert_eq!(value.to_string(), expected, "{src} produced {value}");
    }
}

#[test]
fn quasiquote_evaluates_unquote_and_splices() {
    for (src, expected) in [
        ("(str `(a b))", "\"(a b)\""),
        ("(let [x 5] (str `(a ~x)))", "\"(a 5)\""),
        ("(str `(~@(list 1 2) 3))", "\"(1 2 3)\""),
        ("(str `[0 ~@(vector :a)])", "\"[0 :a]\""),
        ("(get `{:k ~(+ 2 2)} :k)", "4"),
        ("(defmacro twice [e] `(inc ~e)) (twice 1)", "2"),
        // A nested backquote protects its own unquotes.
        ("(str `(a `(b ~c)))", "\"(a (quasiquote (b (unquote c))))\""),
    ] {
        let value = run(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
        assert_eq!(value.to_string(), expected, "{src} produced {value}");
    }
}

#[test]
fn misplaced_unquote_and_bad_splice_are_refused() {
    let bare = run("(unquote 1)").unwrap_err().to_string();
    assert!(bare.contains("outside quasiquote"), "{bare}");
    let spliced = run("`(a ~@5)").unwrap_err().to_string();
    assert!(spliced.contains("sequential"), "{spliced}");
    let dangling = run("`(a ~)").unwrap_err().to_string();
    assert!(!dangling.is_empty());
}

// ── threading macros (chaining) ───────────────────────────────────

#[test]
fn threading_threads_first_and_last_arguments() {
    for (src, expected) in [
        ("(-> 5 inc inc)", "7"),
        ("(-> {:a 1} (get :a) inc)", "2"),
        ("(-> [1 2 3] first)", "1"),
        // A bare form is called with the threaded value.
        ("(-> [1 2 3] rest (first))", "2"),
        // ->> threads as the last argument.
        ("(->> [1 2 3] (map inc) (reduce + 0))", "9"),
        ("(->> 10 (- 3))", "-7"),
        // Zero forms is identity.
        ("(-> 7)", "7"),
        ("(->> 7)", "7"),
    ] {
        let value = run(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
        assert_eq!(value.to_string(), expected, "{src} produced {value}");
    }
}

#[test]
fn threading_composes_with_macros_and_zos_calls() {
    // A macro call in the chain expands before threading continues.
    let expanded = run("(defmacro twice [e] `(inc ~e)) (-> 1 twice twice)")
        .unwrap_or_else(|e| panic!("macro chain failed: {e}"));
    assert_eq!(expanded.to_string(), "3");
    // The ZOS convention (gf obj args) threads the object first.
    let zos = run(
        "(defclass pt nil ((x :initarg :x))) \
         (-> (make-instance pt :x 5) (slot-value :x) (+ 5))",
    )
    .unwrap_or_else(|e| panic!("zos chain failed: {e}"));
    assert_eq!(zos.to_string(), "10");
}
