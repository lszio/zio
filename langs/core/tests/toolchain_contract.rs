//! Consumer contracts: AST, explicitly compiled VM, and Zio metacircular evaluation.
//! Each engine owns a fresh language context and a separate buffered host.

use std::path::PathBuf;
use std::sync::Arc;

use zio_core::bootstrap::{self, ModuleRoots};
use zio_core::bytecode;
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::error::{EvalError, ReaderError};
use zio_core::io::BufferIoHost;
use zio_core::span::Span;
use zio_core::value::Value;

#[derive(Clone, Copy, Debug)]
enum Engine {
    Ast,
    Vm,
    Meta,
}

const ENGINES: [Engine; 3] = [Engine::Ast, Engine::Vm, Engine::Meta];
const SOURCE: &str = "toolchain-contract.zio";

fn context(files: &[(&str, &str)]) -> (EvalContext, Arc<BufferIoHost>) {
    let io = Arc::new(BufferIoHost::with_files(
        files
            .iter()
            .map(|(path, body)| ((*path).into(), (*body).into()))
            .collect(),
    ));
    let roots = ModuleRoots::new_with_io(vec![PathBuf::from("/contracts")], io.as_ref())
        .expect("virtual module root");
    let ctx = bootstrap::language_context_with_io(roots, io.clone()).expect("language context");
    io.clear_output();
    (ctx, io)
}

fn execute(engine: Engine, ctx: &EvalContext, source: &str) -> Result<Value, EvalError> {
    match engine {
        Engine::Ast => bootstrap::eval_source(ctx, SOURCE, source),
        Engine::Vm => {
            let module = bytecode::compile_source(ctx, SOURCE, source)?;
            bytecode::execute_module(ctx, &module)
        }
        Engine::Meta => zio_core::interpreter::eval_source(ctx, SOURCE, source),
    }
}

fn contract(source: &str, expected: Value, output: &str) {
    contract_with_files(source, expected, output, &[]);
}

fn contract_with_files(source: &str, expected: Value, output: &str, files: &[(&str, &str)]) {
    for engine in ENGINES {
        let (ctx, io) = context(files);
        let actual = execute(engine, &ctx, source)
            .unwrap_or_else(|error| panic!("{engine:?}: {source}\n{error:?}"));
        assert_eq!(actual, expected, "{engine:?}: {source}");
        assert_eq!(io.get_output(), output, "{engine:?} output: {source}");
    }
}

fn vector(values: impl IntoIterator<Item = Value>) -> Value {
    Value::Vector(values.into_iter().collect())
}

fn integers(values: &[i64]) -> Value {
    vector(values.iter().copied().map(Value::Integer))
}

#[test]
fn lexical_bindings_and_captured_mutation() {
    for binding in ["let", "let*"] {
        contract(
            &format!("(let [a 40] [({binding} [a 1 b (+ a 1)] (set! a 3) [a b]) a])"),
            vector([integers(&[3, 2]), Value::Integer(40)]),
            "",
        );
    }
    contract(
        "(let [n 0 bump (fn [] (set! n (+ n 1))) read (fn [] n)]\n\
         (bump) (bump) [(read) n])",
        integers(&[2, 2]),
        "",
    );
}

#[test]
fn call_arguments_are_left_to_right() {
    contract(
        "(defn mark [x] (println x) x)\n\
         (defn pack [a b c] [a b c])\n\
         (pack (mark 1) (mark 2) (mark 3))",
        integers(&[1, 2, 3]),
        "1\n2\n3\n",
    );
}

#[test]
fn truthiness_and_short_circuit() {
    contract(
        "[(if nil 1 2) (if false 1 2) (if 0 1 2) (if \"\" 1 2)\n\
         (if [] 1 2) (if '() 1 2) (if {} 1 2)]",
        integers(&[2, 2, 1, 1, 1, 1, 1]),
        "",
    );
    contract(
        "[(and) (or) (and 1 7) (or nil false 9)\n\
         (and false (println \"skipped\")) (or 0 (println \"skipped\"))\n\
         (and (do (println \"and\") true) 8)\n\
         (or (do (println \"or\") nil) 6)]",
        vector([
            Value::Boolean(true),
            Value::Nil,
            Value::Integer(7),
            Value::Integer(9),
            Value::Boolean(false),
            Value::Integer(0),
            Value::Integer(8),
            Value::Integer(6),
        ]),
        "and\nor\n",
    );
}

#[test]
fn rest_higher_order_and_closures() {
    contract(
        "(defn total [first & rest] (+ first (reduce + 0 rest)))\n\
         (defn adder [n] (fn [x] (+ n x)))\n\
         [(total 1 2 3) (total 4) (apply total [4 5 6])\n\
          (reduce + 0 (map (adder 10) [1 2 3])) ((adder 20) 2)]",
        integers(&[6, 4, 15, 36, 22]),
        "",
    );
}

#[test]
fn loop_recur_and_tail_calls() {
    contract(
        "(loop [n 0 acc 0] (if (< n 1000) (recur (+ n 1) (+ acc n)) acc))",
        Value::Integer(499500),
        "",
    );
    // Loop initializers currently see the outer scope, unlike sequential let.
    contract(
        "(let [a 10] (loop [a 1 b a] [a b]))",
        integers(&[1, 10]),
        "",
    );
    contract(
        "(defn sum-tail [n acc] (if (= n 0) acc (sum-tail (- n 1) (+ acc n))))\n\
         (sum-tail 1000 0)",
        Value::Integer(500500),
        "",
    );
}

#[test]
fn quote_and_nested_quasiquote() {
    contract(
        "'(+ 1 2)",
        Value::List(im::vector![
            Value::Symbol("+".into()),
            Value::Integer(1),
            Value::Integer(2),
        ]),
        "",
    );
    contract(
        "(let [x 5 xs [6 7]] `(a ~x ~@xs `(b ~unbound)))",
        Value::List(im::vector![
            Value::Symbol("a".into()),
            Value::Integer(5),
            Value::Integer(6),
            Value::Integer(7),
            Value::List(im::vector![
                Value::Symbol("quasiquote".into()),
                Value::List(im::vector![
                    Value::Symbol("b".into()),
                    Value::List(im::vector![
                        Value::Symbol("unquote".into()),
                        Value::Symbol("unbound".into())
                    ]),
                ]),
            ]),
        ]),
        "",
    );
    contract("`[0 ~@(list 1 2) ~(+ 1 2)]", integers(&[0, 1, 2, 3]), "");
    contract("(get `{:k ~(+ 2 2)} :k)", Value::Integer(4), "");
}

#[test]
fn procedural_macros_expand_once() {
    // No pre-expansion in the harness: macro expansion itself is observable.
    contract(
        "(defmacro once [expr] (println \"expand\") `(do ~expr ~expr))\n\
         (once (do (println \"execute\") 7))",
        Value::Integer(7),
        "expand\nexecute\nexecute\n",
    );
}

#[test]
fn syntax_rules_supported_subset() {
    contract(
        "(defmacro choose (syntax-rules (else)\n\
           ((_ else x) x) ((_ test x) (if test x nil))))\n\
         (defmacro collect (syntax-rules () ((_ head rest ...) (list head rest ...))))\n\
         [(choose else 9) (choose true 8) (choose false 7) (collect 1 2 3)]",
        vector([
            Value::Integer(9),
            Value::Integer(8),
            Value::Nil,
            Value::List(im::vector![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3)
            ]),
        ]),
        "",
    );
}

#[test]
fn syntax_rules_substituted_boolean_keys_are_merged() {
    contract(
        "(defmacro clash (syntax-rules () ((_ x) {x 1 true 2}))) (count (clash true))",
        Value::Integer(1),
        "",
    );
}

#[test]
fn try_catch_and_finally() {
    contract(
        "(try (do (println \"body\") 7) (catch any (println \"not reached\") 0))",
        Value::Integer(7),
        "body\n",
    );
    contract(
        "(try (println \"body\") (/ 1 0)\n\
         (catch division-by-zero (println \"caught\") 42))",
        Value::Integer(42),
        "body\ncaught\n",
    );
    error_contract(
        "(try (println \"body\") (/ 1 0) (catch type-error 42))",
        ErrorPayload::DivisionByZero,
        "body\n",
        None,
        &[],
    );
    error_contract(
        "(try (println \"body\") (/ 1 0)\n\
         (catch any (println \"handler\") handler-missing))",
        ErrorPayload::Symbol("handler-missing".into()),
        "body\nhandler\n",
        None,
        &[],
    );
    // Finally is planned; it is currently an unresolved ordinary operator.
    error_contract(
        "(try 7 (finally (println \"must not execute\")))",
        ErrorPayload::Symbol("finally".into()),
        "",
        None,
        &[],
    );
}

#[test]
fn modules_export_qualified_refer_and_isolation() {
    let files = [(
        "/contracts/math.zio",
        "(println \"loaded\")\n(def hidden 40)\n(defn add [x] (+ hidden x))\n(def exposed 7)\n(export add exposed)",
    )];
    contract_with_files(
        "(def hidden 100)\n(require :math :refer [add])\n\
         (require :math :refer [])\n[(add 2) (math/add 3) math/exposed hidden]",
        integers(&[42, 43, 7, 100]),
        "loaded\n",
        &files,
    );
    for (source, symbol) in [
        ("(require :math :refer [add])\nexposed", "exposed"),
        ("(require :math :refer [])\nmath/hidden", "math/hidden"),
        ("(require :math :refer [])\nhidden", "hidden"),
    ] {
        error_contract(
            source,
            ErrorPayload::Symbol(symbol.into()),
            "loaded\n",
            None,
            &files,
        );
    }
}

#[test]
fn zos_dispatch_and_next_method() {
    contract(
        "(defclass shape nil ((x :initarg :x)))\n\
         (defclass point shape ())\n(defgeneric identify (obj))\n\
         (defmethod identify ((obj shape)) 10)\n\
         (defmethod identify ((obj point)) (slot-value obj :x))\n\
         (defmethod identify :around ((obj point))\n\
           (do (println \"around\") (+ 1 (call-next-method))))\n\
         [(identify (make-instance shape :x 3)) (identify (make-instance point :x 41))]",
        integers(&[10, 42]),
        "around\n",
    );
}

#[test]
fn integer_precision_and_division() {
    contract(
        "[(+ 9007199254740992 1) (- 9007199254740993 9007199254740992)\n\
         (/ 7 2) (/ -7 2) (/ 6 3) (/ 1)]",
        vector([
            Value::Integer(9007199254740993),
            Value::Integer(1),
            Value::Integer(3),
            Value::Integer(-3),
            Value::Integer(2),
            Value::Float(1.0),
        ]),
        "",
    );
    contract(
        "[(> 9007199254740993 9007199254740992.0)\n\
         (< 9007199254740992.0 9007199254740993)]",
        vector([Value::Boolean(true), Value::Boolean(true)]),
        "",
    );
}

// Compare structured categories and actionable payloads, not Display wording.
#[derive(Debug, PartialEq)]
enum ErrorPayload {
    Symbol(String),
    NotFunction(String),
    Arity(usize, usize),
    MinArity(usize, usize),
    RangeArity(usize, usize, usize),
    Index(usize, usize),
    Type(&'static str, String),
    DivisionByZero,
    InvalidForm,
    Macro,
    RecurNotTail,
    RecurWithoutLoop,
    Custom,
}

fn error_parts(error: &EvalError) -> (ErrorPayload, Option<Span>) {
    use EvalError::*;
    let (payload, context) = match error {
        SymbolNotFound(name, span) => (ErrorPayload::Symbol(name.clone()), span),
        NotAFunction { value, span } => (ErrorPayload::NotFunction(value.clone()), span),
        WrongArgCount {
            expected,
            got,
            span,
        } => (ErrorPayload::Arity(*expected, *got), span),
        WrongArgCountMin { min, got, span } => (ErrorPayload::MinArity(*min, *got), span),
        WrongArgCountRange {
            min,
            max,
            got,
            span,
        } => (ErrorPayload::RangeArity(*min, *max, *got), span),
        IndexOutOfBounds {
            index,
            length,
            span,
        } => (ErrorPayload::Index(*index, *length), span),
        TypeError {
            expected,
            got,
            span,
        } => (ErrorPayload::Type(expected, got.clone()), span),
        DivisionByZero { span } => (ErrorPayload::DivisionByZero, span),
        InvalidForm(_, span) => (ErrorPayload::InvalidForm, span),
        MacroError(_, span) => (ErrorPayload::Macro, span),
        RecurNotTail { span } => (ErrorPayload::RecurNotTail, span),
        RecurWithoutLoop { span } => (ErrorPayload::RecurWithoutLoop, span),
        Cancelled { span, .. } => (ErrorPayload::Custom, span), // harness-only: no structured variant here
        CapabilityDenied { span, .. } => (ErrorPayload::Custom, span),
        TimedOut { span, .. } => (ErrorPayload::Custom, span),
        LoadError(_, span) => (ErrorPayload::Custom, span),
        Custom(_, span) => (ErrorPayload::Custom, span),
    };
    (payload, context.span)
}

fn error_contract(
    source: &str,
    expected: ErrorPayload,
    output: &str,
    location: Option<(usize, usize)>,
    files: &[(&str, &str)],
) {
    for engine in ENGINES {
        let (ctx, io) = context(files);
        let error = execute(engine, &ctx, source).expect_err("program must fail");
        let (payload, span) = error_parts(&error);
        assert_eq!(payload, expected, "{engine:?}: {error:?}");
        assert_eq!(io.get_output(), output, "{engine:?} error output");
        if let Some(location) = location {
            let span = span.unwrap_or_else(|| panic!("{engine:?}: missing span: {error:?}"));
            assert_eq!((span.line, span.col), location, "{engine:?}: {error:?}");
            assert_eq!(
                ctx.source_map().source_name(span.source_id),
                SOURCE,
                "{engine:?}"
            );
        }
    }
}

#[test]
fn runtime_errors_preserve_payload_and_location() {
    for (expression, expected) in [
        ("missing", ErrorPayload::Symbol("missing".into())),
        ("(/ 1 0)", ErrorPayload::DivisionByZero),
        ("(1 2)", ErrorPayload::NotFunction("integer 1".into())),
        ("((fn [x] x))", ErrorPayload::Arity(1, 0)),
        ("(+ 1 \"x\")", ErrorPayload::Type("number", "string".into())),
    ] {
        error_contract(
            &format!("(println \"before\")\n{expression}"),
            expected,
            "before\n",
            Some((2, 1)),
            &[],
        );
    }
}

#[test]
fn reader_errors_preserve_category_and_location() {
    for (source, expected, location) in [
        ("42\n{:x}", ReaderError::OddMapElements, (2, 1)),
        ("42\n'", ReaderError::MissingQuoteExpr, (2, 1)),
    ] {
        // The public evaluator currently wraps reader failures in Custom.
        // Check the structured reader cause independently, never its message.
        let (ctx, _) = context(&[]);
        let source_id = ctx.source_map().register(SOURCE.into(), source.into());
        let reader_error = zio_core::reader::reader::read_program_with_source(source, source_id)
            .expect_err("malformed input");
        assert_eq!(reader_error.root_cause(), expected);
        let span = reader_error.span().expect("reader span");
        assert_eq!((span.line, span.col), location);
        assert_eq!(ctx.source_map().source_name(span.source_id), SOURCE);
        error_contract(source, ErrorPayload::Custom, "", Some(location), &[]);
    }
}

#[test]
fn conditional_and_local_definition_results() {
    contract(
        "(def n 40)\n\
         [(let [n 1] (def n 2) n) n\n\
          (cond ((< 1 0) :negative) ((= 1 0) :zero) (else :positive))\n\
          (cond (false 7))]",
        vector([
            Value::Integer(2),
            Value::Integer(40),
            Value::Keyword("positive".into()),
            Value::Nil,
        ]),
        "",
    );
    let source = format!("(cond {} (true 42) (else 99))", "(false 0) ".repeat(100));
    contract(&source, Value::Integer(42), "");
}

#[test]
fn equal_datum_syntax_keys_compile_deterministically() {
    let source = "(let [n 0]\n\
                    (def result {(do (set! n (+ n 1)) :k) 10\n\
                                 (do (set! n (+ n 1)) :k) 20})\n\
                    [n (get result :k)])";
    let (first, _) = context(&[]);
    let module = bytecode::compile_source(&first, SOURCE, source).unwrap();
    let wire = bytecode::serialize_module(&module).unwrap();
    assert_eq!(
        bytecode::execute_module(&first, &module).unwrap(),
        integers(&[2, 20])
    );
    let (second, _) = context(&[]);
    let rebuilt = bytecode::compile_source(&second, SOURCE, source).unwrap();
    assert_eq!(bytecode::serialize_module(&rebuilt).unwrap(), wire);
    assert_eq!(
        bytecode::execute_module(&second, &rebuilt).unwrap(),
        integers(&[2, 20])
    );
}

#[test]
fn inline_module_and_alias_maps_preserve_exports() {
    contract(
        "(def hidden 40)\n\
         (module :local :export [exposed] (def exposed 7) (def hidden 9))\n\
         (require :local :as local-api :refer [])\n\
         [local/exposed (get local-api :exposed) hidden]",
        integers(&[7, 7, 40]),
        "",
    );
}

#[test]
fn errors_inside_functions_keep_definition_locations() {
    error_contract(
        "(defn broken []\n  (/ 1 0))\n(broken)",
        ErrorPayload::DivisionByZero,
        "",
        Some((2, 3)),
        &[],
    );
}

#[test]
fn nested_handlers_preserve_outer_error_binding() {
    contract(
        "(def *error* :outer)\n\
         [(try (try (/ 1 0) (catch division-by-zero (error \"replacement\")))\n\
            (catch error (string? *error*)))\n\
          *error*\n\
          (try (/ 1 0) (catch any 1) (catch division-by-zero 2))]",
        vector([
            Value::Boolean(true),
            Value::Keyword("outer".into()),
            Value::Integer(2),
        ]),
        "",
    );
}

#[test]
fn macro_effects_obey_entry_phase() {
    let source = "(println \"before\")\n\
                  (defmacro marker [] (println \"expand\") 7)\n\
                  (println (marker))";
    let (ast, ast_io) = context(&[]);
    assert_eq!(execute(Engine::Ast, &ast, source).unwrap(), Value::Nil);
    assert_eq!(ast_io.get_output(), "before\nexpand\n7\n");

    let (meta, meta_io) = context(&[]);
    assert_eq!(execute(Engine::Meta, &meta, source).unwrap(), Value::Nil);
    assert_eq!(meta_io.get_output(), "before\nexpand\n7\n");

    let (vm, vm_io) = context(&[]);
    let module = bytecode::compile_source(&vm, SOURCE, source).unwrap();
    assert_eq!(vm_io.get_output(), "expand\n", "compile-time effects");
    vm_io.clear_output();
    assert_eq!(bytecode::execute_module(&vm, &module).unwrap(), Value::Nil);
    assert_eq!(vm_io.get_output(), "before\n7\n", "runtime effects");
}

#[test]
fn mutual_tail_calls_and_variadic_rebinding() {
    contract(
        "(defn even-tail [n] (if (= n 0) true (odd-tail (- n 1))))\n\
         (defn odd-tail [n] (if (= n 0) false (even-tail (- n 1))))\n\
         (defn rest-tail [n & xs] (if (= n 0) xs (rest-tail (- n 1) 1 2)))\n\
         [(even-tail 1000) (odd-tail 1000) (rest-tail 1000 8)]",
        vector([
            Value::Boolean(true),
            Value::Boolean(false),
            Value::List(im::vector![Value::Integer(1), Value::Integer(2)]),
        ]),
        "",
    );
}

#[test]
fn local_definitions_shadow_outer_bindings() {
    for source in [
        "(let [x 1] ((fn [] (defn x [] 2))) x)",
        "(let [x 1] ((fn [] (def x 2))) x)",
        "(let [x 1] (let [] (def x 2)) x)",
    ] {
        contract(source, Value::Integer(1), "");
    }
    contract("(let [x 1] (defn x [] 2) (x))", Value::Integer(2), "");
    contract("(defn f [x] (def x 2) x) (f 1)", Value::Integer(2), "");
    contract(
        "(let [x 1] (let [] (def x (+ x 1)) x))",
        Value::Integer(2),
        "",
    );
    contract(
        "(let [x 1] (let [] (defn f [] x) (def before (f)) (def x 2) [before (f)]))",
        integers(&[1, 2]),
        "",
    );
    contract(
        "(def g 1) (let [] (defn f [] g) (def before (f)) (def g 2) [before (f)])",
        integers(&[1, 2]),
        "",
    );
    contract(
        "(let [x 1] (let [] (if false (def x 2) nil) (set! x 3)) x)",
        Value::Integer(3),
        "",
    );
    contract(
        "(let [x 1] (let [] (if true (def x 2) nil) (set! x 3)) x)",
        Value::Integer(1),
        "",
    );
}

#[test]
fn local_forward_definitions_share_bindings() {
    contract(
        "(let [] (defn f [] (g)) (defn g [] 7) (f))",
        Value::Integer(7),
        "",
    );
    contract(
        "(let [] (defn f [] g) (def g 7) (f))",
        Value::Integer(7),
        "",
    );
    contract(
        "(let []\n\
           (defn even-local [n] (if (= n 0) true (odd-local (- n 1))))\n\
           (defn odd-local [n] (if (= n 0) false (even-local (- n 1))))\n\
           (even-local 1000))",
        Value::Boolean(true),
        "",
    );
    for source in [
        "(let [] (defn f [] g) (if true (def g 7) nil) (f))",
        "(let [] (defn f [] g) (do (def g 7)) (f))",
        "(let [] (defn f [] g) `(~(def g 7)) (f))",
        "(let [] (defn f [] g) `(~@(do (def g 7) [1])) (f))",
        "(let [] (defn f [] g) `(~`(~(def g 7))) (f))",
    ] {
        contract(source, Value::Integer(7), "");
    }
    contract(
        "(def g 1) (let [] (defn f [] g) `(`(~(def g 7))) (f))",
        Value::Integer(1),
        "",
    );
}

#[test]
fn vm_non_tail_calls_return_and_unwind_without_rust_recursion() {
    let (ctx, _) = context(&[]);
    let source = "(defn descend [n] (if (= n 0) 0 (+ 1 (descend (- n 1)))))\n\
                  (defn fail-deep [n] (if (= n 0) (/ 1 0) (+ 1 (fail-deep (- n 1)))))\n\
                  [(descend 1000) (try (fail-deep 1000) (catch division-by-zero 42))]";
    assert_eq!(
        execute(Engine::Vm, &ctx, source).unwrap(),
        integers(&[1000, 42])
    );
}

#[test]
fn forward_bindings_remain_unbound_until_definition() {
    for source in [
        "(let [] g (def g 7))",
        "(let [] (defn f [] (g)) (f) (defn g [] 7))",
    ] {
        error_contract(source, ErrorPayload::Symbol("g".into()), "", None, &[]);
    }
}

#[test]
fn compiled_errors_keep_the_source_revision() {
    let source = "(/ 1 0)";
    let (ctx, _) = context(&[]);
    let module = bytecode::compile_source(&ctx, SOURCE, source).unwrap();
    let wire = bytecode::serialize_module(&module).unwrap();
    bytecode::compile_source(&ctx, SOURCE, "42").unwrap();
    for (runtime, artifact) in [(ctx, module), {
        let (restored, _) = context(&[]);
        bootstrap::eval_source(&restored, SOURCE, "42").unwrap();
        (restored, bytecode::deserialize_module(&wire).unwrap())
    }] {
        let error = bytecode::execute_module(&runtime, &artifact).unwrap_err();
        let (payload, span) = error_parts(&error);
        assert_eq!(payload, ErrorPayload::DivisionByZero);
        let span = span.expect("retained revision span");
        assert_eq!(
            (span.line, span.col, span.start.0, span.end.0),
            (1, 1, 0, 7)
        );
        let file = runtime
            .source_map()
            .get(span.source_id)
            .expect("registered revision");
        assert_eq!(file.name, SOURCE);
        assert_eq!(
            file.source, source,
            "must not point at newer same-name source"
        );
    }
}

#[test]
fn legacy_artifact_does_not_guess_error_source() {
    let (compiler, _) = context(&[]);
    let Value::Map(mut legacy) = bytecode::compile_source(&compiler, SOURCE, "(/ 1 0)").unwrap()
    else {
        panic!("compiled module must be a map");
    };
    legacy.remove(&Value::Keyword("source".into()));
    let wire = bytecode::serialize_module(&Value::Map(legacy)).unwrap();
    let module = bytecode::deserialize_module(&wire).unwrap();
    let (runtime, _) = context(&[]);
    bootstrap::eval_source(&runtime, SOURCE, "42").unwrap();
    let error = bytecode::execute_module(&runtime, &module).unwrap_err();
    assert_eq!(error_parts(&error), (ErrorPayload::DivisionByZero, None));
}

#[test]
fn compiled_macro_errors_keep_definition_source() {
    let original = "(defmacro broken [] (/ 1 0))";
    let (ctx, _) = context(&[]);
    bytecode::compile_source(&ctx, SOURCE, original).unwrap();
    bootstrap::eval_source(&ctx, SOURCE, "42").unwrap();
    let error = bytecode::compile_source(&ctx, SOURCE, "(broken)").unwrap_err();
    let (payload, span) = error_parts(&error);
    assert_eq!(payload, ErrorPayload::DivisionByZero);
    let span = span.expect("macro definition source");
    assert_eq!((span.line, span.col), (1, 21));
    let file = ctx.source_map().get(span.source_id).unwrap();
    assert_eq!(file.name, SOURCE);
    assert_eq!(file.source, original);
}
