//! Consumer contracts for the Zio reader and explicit metacircular execution.
use std::sync::Arc;
use zio_core::{bootstrap, context::EvalContext, io::BufferIoHost, value::Value};

fn context() -> (EvalContext, Arc<BufferIoHost>) {
    let io = Arc::new(BufferIoHost::default());
    let ctx =
        bootstrap::language_context_with_io(bootstrap::ModuleRoots::empty(), io.clone()).unwrap();
    io.clear_output();
    (ctx, io)
}

fn invoke(ctx: &EvalContext, entry: &str, source: &str) -> Value {
    let call = format!(
        "({entry} {} \"meta-contract.zio\")",
        serde_json::to_string(source).unwrap()
    );
    bootstrap::eval_source(ctx, "meta-driver.zio", &call).unwrap()
}

#[test]
fn meta_execution_preserves_captured_binding_and_output() {
    let (ctx, io) = context();
    let result = invoke(
        &ctx,
        "zio-meta-eval",
        "(let [n 40 bump (fn [] (set! n (+ n 1))) read (fn [] n)] (bump) (println (read)) (bump))",
    );
    assert_eq!(result, Value::Integer(42));
    assert_eq!(io.get_output(), "41\n");
}

#[test]
fn meta_reader_preserves_unicode_source_positions() {
    let (ctx, _) = context();
    let nodes = invoke(&ctx, "zio-meta-read", "\"中\"\n(+ 1 2)");
    let Value::Vector(nodes) = nodes else {
        panic!("syntax vector expected")
    };
    let second = zio_core::syntax::SyntaxNode::from_value(&nodes[1]).unwrap();
    let span = second.span.unwrap();
    assert_eq!(
        (span.start.0, span.end.0, span.line, span.col),
        (6, 13, 2, 1)
    );
    assert_eq!(
        ctx.source_map.source_name(span.source_id),
        "meta-contract.zio"
    );
}

#[test]
fn meta_reader_reconstructs_current_literals_without_execution() {
    let (ctx, io) = context();
    let source = r#"; comment
nil true false -42 1.5 1e3 :ready "中\n\t\\\q"
'x `(a ~b ~@c) #(1 2) #{:x :y}
#\界 #\space #\newline #\( {:a [1 2] :b '(x)}"#;
    let Value::Vector(nodes) = invoke(&ctx, "zio-meta-read", source) else {
        panic!("syntax vector expected");
    };
    let expected = zio_core::reader::reader::read_program(source).unwrap();
    let actual: Vec<Value> = nodes
        .iter()
        .map(|node| {
            Value::from(
                zio_core::syntax::SyntaxNode::from_value(node)
                    .unwrap()
                    .to_sexp()
                    .unwrap(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        expected.into_iter().map(Value::from).collect::<Vec<_>>()
    );
    assert_eq!(io.get_output(), "");
}

#[test]
fn meta_reader_errors_retain_original_byte_ranges() {
    for source in [
        "\"unterminated",
        "' ;nothing",
        "[1)",
        "#\\unknown",
        "{:x}",
        "(",
    ] {
        let (ctx, _) = context();
        let source_id = ctx
            .source_map
            .register("meta-contract.zio".into(), source.into());
        let expected = zio_core::reader::reader::read_program_with_source(source, source_id)
            .unwrap_err()
            .span()
            .unwrap();
        let error =
            zio_core::interpreter::read_source(&ctx, "meta-contract.zio", source).unwrap_err();
        let zio_core::error::EvalError::Custom(_, context) = error else {
            panic!("reader failure category changed: {error:?}");
        };
        let actual = context.span.unwrap();
        assert_eq!(
            (actual.start, actual.end, actual.line, actual.col),
            (expected.start, expected.end, expected.line, expected.col),
            "{source}"
        );
        assert_eq!(
            ctx.source_map.source_name(actual.source_id),
            "meta-contract.zio"
        );
    }
}

#[test]
fn meta_tail_and_non_tail_calls_preserve_return_and_handler_state() {
    let (ctx, _) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "deep-meta.zio",
        "(defn tail [n sum] (if (= n 0) sum (tail (- n 1) (+ sum n))))
         (defn descend [n] (if (= n 0) 0 (+ 1 (descend (- n 1)))))
         (defn fail-deep [n] (if (= n 0) (/ 1 0) (+ 1 (fail-deep (- n 1)))))
         [(tail 10000 0) (descend 1000)
          (try (fail-deep 1000) (catch division-by-zero 42))]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(50005000),
            Value::Integer(1000),
            Value::Integer(42)
        ])
    );
}

#[test]
fn meta_source_revisions_survive_redefinition_and_macro_calls() {
    let (ctx, _) = context();
    for definition in ["(defn broken [] (/ 1 0))", "(defmacro broken [] (/ 1 0))"] {
        zio_core::interpreter::eval_source(&ctx, "definition.zio", definition).unwrap();
        zio_core::interpreter::eval_source(&ctx, "definition.zio", "42").unwrap();
        let error = zio_core::interpreter::eval_source(&ctx, "caller.zio", "(broken)").unwrap_err();
        let zio_core::error::EvalError::DivisionByZero { span } = error else {
            panic!("definition error category changed: {error:?}");
        };
        let source = ctx.source_map.get(span.span.unwrap().source_id).unwrap();
        assert_eq!(source.name, "definition.zio");
        assert_eq!(source.source, definition);
    }
}

#[test]
fn meta_reader_and_eval_aliases_keep_third_surface_through_higher_order_calls() {
    let (ctx, _) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "aliases.zio",
        "(defn through [f x] (f x))
         [(through eval '(+ 1 2)) (apply eval ['(+ 2 3)])
          (first (map eval ['(+ 3 4)]))
          (eval (apply read-string [\"(+ 4 5)\"])) (read-string \"\")]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(3),
            Value::Integer(5),
            Value::Integer(7),
            Value::Integer(9),
            Value::Nil
        ])
    );
}

#[test]
fn meta_macroexpand_callbacks_keep_expansion_effects_and_values() {
    let (ctx, io) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "macro-callbacks.zio",
        "(defmacro echo [x] (println \"expand\") x)
         [(eval '(echo 42)) (= (macroexpand-1 '(echo 43)) 43)
          (= (first (map macroexpand ['(echo 44)])) 44)]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(42),
            Value::Boolean(true),
            Value::Boolean(true)
        ])
    );
    assert_eq!(io.get_output(), "expand\nexpand\nexpand\n");
}

#[test]
fn meta_synchronous_future_promise_and_channel_semantics_are_preserved() {
    let (ctx, _) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "sync.zio",
        "(let [n 0 f (future-call (fn [] (set! n 1) 42))
               p (promise) c (chan 1)]
           (deliver p 7) (deliver p 9) (send! c 8)
           [n (deref f) (deref p) (recv! c)])",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(1),
            Value::Integer(42),
            Value::Integer(7),
            Value::Integer(8)
        ])
    );
}

#[test]
fn client_native_named_eval_is_not_replaced_by_intrinsic_dispatch() {
    let (ctx, _) = context();
    ctx.env.set(
        "client".into(),
        Value::NativeFunction(zio_core::value::NativeFn::new("eval", |args, _| {
            Ok(args[0].clone())
        })),
    );
    assert_eq!(
        zio_core::interpreter::eval_source(&ctx, "client.zio", "(first (map client [42]))")
            .unwrap(),
        Value::Integer(42)
    );
}

#[test]
fn dynamic_reader_failures_keep_the_call_site_location() {
    for input in ["(", "\"", "1 2"] {
        let source = format!("(read-string {})", serde_json::to_string(input).unwrap());
        let (ast, _) = context();
        let expected = bootstrap::eval_source(&ast, "dynamic-reader.zio", &source).unwrap_err();
        let (meta, _) = context();
        let actual =
            zio_core::interpreter::eval_source(&meta, "dynamic-reader.zio", &source).unwrap_err();
        let (
            zio_core::error::EvalError::Custom(_, expected),
            zio_core::error::EvalError::Custom(_, actual),
        ) = (expected, actual)
        else {
            panic!("dynamic reader changed failure category");
        };
        let (expected, actual) = (expected.span.unwrap(), actual.span.unwrap());
        assert_eq!(
            (actual.start, actual.end, actual.line, actual.col),
            (expected.start, expected.end, expected.line, expected.col)
        );
        assert_eq!(
            meta.source_map.source_name(actual.source_id),
            "dynamic-reader.zio"
        );
    }
}

#[test]
fn ordinary_higher_order_calls_reject_macro_values_without_expanding() {
    for call in [
        "(map echo [42])",
        "(filter echo [42])",
        "(reduce echo 0 [42])",
        "(apply echo [42])",
    ] {
        let source = format!("(defmacro echo [x] (println \"expand\") x) {call}");
        for meta in [false, true] {
            let (ctx, io) = context();
            let error = if meta {
                zio_core::interpreter::eval_source(&ctx, "macro-value.zio", &source)
            } else {
                bootstrap::eval_source(&ctx, "macro-value.zio", &source)
            }
            .unwrap_err();
            assert!(
                matches!(error, zio_core::error::EvalError::NotAFunction { .. }),
                "meta={meta}: {call}: {error:?}"
            );
            assert_eq!(io.get_output(), "", "meta={meta}: {call}");
        }
    }
}

struct RejectTargetAstEvents(Arc<zio_core::span::SourceMap>);

impl zio_core::observer::Observer for RejectTargetAstEvents {
    fn on_event(&self, event: &zio_core::observer::Event) {
        if let Some(span) = event.span {
            assert_ne!(
                self.0.source_name(span.source_id),
                "guarded-meta.zio",
                "target expression reentered the Stage 0 AST observer: {event:?}"
            );
        }
    }
}

#[test]
fn native_callbacks_and_module_initializers_keep_the_meta_engine() {
    let io = Arc::new(BufferIoHost::with_files(vec![(
        "/modules/guarded.zio".into(),
        "(def answer (probe-stage)) (export answer)".into(),
    )]));
    let roots = bootstrap::ModuleRoots::new_with_io(vec!["/modules".into()], io.as_ref()).unwrap();
    let ctx = bootstrap::language_context_with_io(roots, io).unwrap();
    let Value::NativeFunction(eval) = ctx.env.get("eval").unwrap() else {
        panic!("native eval expected");
    };
    ctx.env.set(
        "probe-stage".into(),
        Value::NativeFunction(zio_core::value::NativeFn::new(
            "probe-stage",
            move |_, engine| {
                engine
                    .intercept_native(
                        &eval,
                        &im::vector![Value::List(im::vector![
                            Value::Symbol("+".into()),
                            Value::Integer(40),
                            Value::Integer(2)
                        ])],
                    )
                    .unwrap_or_else(|| {
                        Err(zio_core::error::EvalError::custom(
                            "host callback reentered the Stage 0 engine",
                        ))
                    })
            },
        )),
    );
    ctx.attach_observer(Arc::new(RejectTargetAstEvents(ctx.source_map.clone())));
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "guarded-meta.zio",
        "(defn target [x] (if (> x 0) (probe-stage) 0))
         (defmacro target-m [] (probe-stage))
         (defclass Guarded nil ())
         (defgeneric identify [x])
         (defmethod identify ((x Guarded)) (probe-stage))
         (require :guarded)
         [(target 1) (first (map target [1])) (target-m)
          (identify (make-instance Guarded)) guarded/answer]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(42),
            Value::Integer(42),
            Value::Integer(42),
            Value::Integer(42),
            Value::Integer(42)
        ])
    );
}

#[test]
fn failed_meta_module_loads_clean_cache_and_keep_root_authorization() {
    use zio_core::io::IoHost;
    let io = Arc::new(BufferIoHost::with_files(vec![
        ("/modules/bad.zio".into(), "(missing)".into()),
        ("/modules/a.zio".into(), "(require :b)".into()),
        ("/modules/b.zio".into(), "(require :a)".into()),
        ("/modules/inherited.zio".into(), "(export inc)".into()),
        (
            "/outside.zio".into(),
            "(def secret 99) (export secret)".into(),
        ),
    ]));
    let roots = bootstrap::ModuleRoots::new_with_io(vec!["/modules".into()], io.as_ref()).unwrap();
    let ctx = bootstrap::language_context_with_io(roots, io.clone()).unwrap();
    for source in [
        "(require :bad)",
        "(require :a)",
        "(require :inherited)",
        "(require :../outside)",
        "(load \"relative.zio\")",
    ] {
        assert!(
            zio_core::interpreter::eval_source(&ctx, "authorization.zio", source).is_err(),
            "{source}"
        );
        assert!(ctx.modules.borrow().loading_stack.is_empty(), "{source}");
    }
    assert!(ctx.env.get("secret").is_none());
    io.write_file("/modules/bad.zio", "(def fixed 9) (export fixed)")
        .unwrap();
    assert_eq!(
        zio_core::interpreter::eval_source(&ctx, "fixed.zio", "(require :bad) bad/fixed").unwrap(),
        Value::Integer(9)
    );
}

#[test]
fn remaining_public_forms_keep_ast_values_and_effects() {
    for (source, expected) in [
        (
            "[(do) ((fn [])) (count #{1 1 2})]",
            Value::Vector(im::vector![Value::Nil, Value::Nil, Value::Integer(2)]),
        ),
        (
            "(defmacro with-tmp (syntax-rules () ((_ x) (let [tmp 7] (+ tmp x)))))
          (let [tmp 1] (with-tmp tmp))",
            Value::Integer(8),
        ),
        (
            "(defmacro choose (syntax-rules () ((_ [x y]) (+ x y)))) (choose [20 22])",
            Value::Integer(42),
        ),
        (
            "(defpackage :sample (:use :core) (:export :answer))",
            Value::Keyword("sample".into()),
        ),
        (
            "(defpackage :sample (::use :core) (::export :answer))
             [(first *package-uses-sample*) (first *package-exports-sample*)]",
            Value::Vector(im::vector![
                Value::Keyword("core".into()),
                Value::Keyword("answer".into())
            ]),
        ),
        (
            "(let [x 1] (let [] (set! missing 42) missing))",
            Value::Integer(42),
        ),
    ] {
        for meta in [false, true] {
            let (ctx, io) = context();
            let result = if meta {
                zio_core::interpreter::eval_source(&ctx, "public-forms.zio", source)
            } else {
                bootstrap::eval_source(&ctx, "public-forms.zio", source)
            }
            .unwrap();
            assert_eq!(result, expected, "meta={meta}: {source}");
            assert_eq!(io.get_output(), "", "meta={meta}: {source}");
        }
    }
}

#[test]
fn meta_method_combination_keeps_captured_side_effect_order() {
    let (ctx, _) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "combination.zio",
        "(def order [])
         (defclass Box nil ())
         (defgeneric combine [x])
         (defmethod combine :before ((x Box)) (set! order (vector-conj order :before)))
         (defmethod combine ((x Box)) (do (set! order (vector-conj order :primary)) 41))
         (defmethod combine :after ((x Box)) (set! order (vector-conj order :after)))
         (defmethod combine :around ((x Box))
           (do (set! order (vector-conj order :around)) (+ 1 (call-next-method))))
         [(combine (make-instance Box)) order]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(42),
            Value::Vector(im::vector![
                Value::Keyword("around".into()),
                Value::Keyword("before".into()),
                Value::Keyword("primary".into()),
                Value::Keyword("after".into())
            ])
        ])
    );
}

#[test]
fn recursive_higher_order_calls_share_the_explicit_control_stack() {
    let (ctx, _) = context();
    let result = zio_core::interpreter::eval_source(
        &ctx,
        "deep-hof.zio",
        "(defn map-hop [n]
           (if (= n 0) 0 (+ 1 (first (map map-hop [(- n 1)])))))
         (defn reduce-hop [n]
           (if (= n 0) 0
             (+ 1 (reduce (fn [acc x] (reduce-hop (- x 1))) 0 [n]))))
         (defn apply-tail [n sum]
           (if (= n 0) sum (apply apply-tail [(- n 1) (+ sum n)])))
         [(map-hop 1000) (reduce-hop 1000) (apply-tail 3000 0)]",
    )
    .unwrap();
    assert_eq!(
        result,
        Value::Vector(im::vector![
            Value::Integer(1000),
            Value::Integer(1000),
            Value::Integer(4501500)
        ])
    );
}
