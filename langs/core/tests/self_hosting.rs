//! Self-hosting contracts on the ordinary Cargo test harness stack.
//!
//! Compiler construction and syntax decoding must not require a larger stack
//! or a separate execution surface just to pass the staged compiler checks.

use std::sync::Arc;
use zio_core::bootstrap::{self, ModuleRoots};
use zio_core::bytecode;
use zio_core::value::Value;

struct NoInterpretedFrontend(Arc<zio_core::span::SourceMap>);

impl zio_core::observer::Observer for NoInterpretedFrontend {
    fn on_event(&self, event: &zio_core::observer::Event) {
        if let Some(span) = event.span {
            let name = self.0.source_name(span.source_id);
            assert!(
                !matches!(
                    name.as_str(),
                    "<compiler>"
                        | "compiler.zio"
                        | "reader.zio"
                        | "expand.zio"
                        | "evaluator.zio"
                        | "analyze.zio"
                ) && !name.starts_with("<zio-interpreter/")
                    && name != "trivial.zio",
                "compiled frontend reentered Stage 0 AST: {event:?} in {name}"
            );
        }
    }
}

fn context() -> zio_core::context::EvalContext {
    bootstrap::language_context(ModuleRoots::empty()).expect("language context")
}

#[test]
fn the_compiler_compiles_itself_to_a_fixed_point() {
    // The claim is not "the compiler runs" but "the compiled compiler
    // reproduces the same compiler". Stage 0 is the interpreter running
    // the compiler source, stage 1 is that compiler compiled, and stage 2
    // is stage 1's own compiler compiling the source again. Unequal
    // stages would mean every later build stands on sand.
    let build = bytecode::self_build(&context()).expect("self-build");
    assert_eq!(
        build.normalized[0], build.normalized[1],
        "stage 1 and stage 2 artifacts differ"
    );
    assert_eq!(
        build.normalized.get(2),
        Some(&build.normalized[1]),
        "stage 3 must rebuild the same canonical compiler artifact"
    );
    assert!(build.equal, "self-build reported unequal stages");
}

#[test]
fn the_compiled_compiler_compiles_programs() {
    // Stage 2 only matters if it can do the work. Every case here is a
    // construct the compiler has to lower, so a failure names the form
    // that regressed rather than a general "compilation broke".
    let bootstrap_ctx = context();
    let stage1 =
        bytecode::compile_source(&bootstrap_ctx, "compiler.zio", bytecode::compiler_source())
            .expect("stage 1");
    let ctx = context();
    let portable = bytecode::deserialize_module(&bytecode::serialize_module(&stage1).unwrap())
        .expect("portable compiler");
    bytecode::install_compiled_compiler(&ctx, &portable).expect("install fresh stage 1");
    ctx.attach_observer(Arc::new(NoInterpretedFrontend(ctx.source_map.clone())));

    for (label, program, expected) in [
        ("defn", "(defn sq [x] (* x x)) (sq 7)", Value::Integer(49)),
        ("let*", "(let* [a 1 b 2] (+ a b))", Value::Integer(3)),
        (
            "loop/recur",
            "(loop [n 0 acc 1] (if (< n 5) (recur (+ n 1) (* acc 2)) acc))",
            Value::Integer(32),
        ),
        (
            "closure",
            "(defn mk [k] (fn [x] (+ x k))) ((mk 10) 5)",
            Value::Integer(15),
        ),
        (
            "vector",
            "#(1 2 3)",
            Value::Vector(im::vector![
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3)
            ]),
        ),
        (
            "map",
            "{:a 1 :b 2}",
            Value::Map(
                im::hashmap! {Value::Keyword("a".into()) => Value::Integer(1), Value::Keyword("b".into()) => Value::Integer(2)},
            ),
        ),
        (
            "quote",
            "'(1 2)",
            Value::List(im::vector![Value::Integer(1), Value::Integer(2)]),
        ),
        ("cond", "(cond (true 1) (else 2))", Value::Integer(1)),
        ("and/or", "(and true false)", Value::Boolean(false)),
        (
            "procedural macro",
            "(defmacro add-one [x] (list '+ x 1)) (add-one 41)",
            Value::Integer(42),
        ),
        (
            "syntax-rules",
            "(defmacro sum-two (syntax-rules () ((_ x y) (+ x y)))) (sum-two 20 22)",
            Value::Integer(42),
        ),
    ] {
        let module = bytecode::compile_source(&ctx, "trivial.zio", program)
            .unwrap_or_else(|error| panic!("compiled compiler failed on {label}: {error}"));
        let actual = bytecode::execute_module(&ctx, &module)
            .unwrap_or_else(|error| panic!("compiled program failed on {label}: {error}"));
        assert_eq!(actual, expected, "{label}");
    }
    let source = "(def label \"中\")\n(defn broken []\n  (/ 1 0))\n(broken)";
    let ast_ctx = context();
    let ast_error = bootstrap::eval_source(&ast_ctx, "locations.zio", source).unwrap_err();
    let module = bytecode::compile_source(&ctx, "locations.zio", source).unwrap();
    let runtime = context();
    let portable =
        bytecode::deserialize_module(&bytecode::serialize_module(&module).unwrap()).unwrap();
    let vm_error = bytecode::execute_module(&runtime, &portable).unwrap_err();
    let zio_core::error::EvalError::DivisionByZero { span: ast_span } = ast_error else {
        panic!("expected AST division error");
    };
    let zio_core::error::EvalError::DivisionByZero { span: vm_span } = vm_error else {
        panic!("expected VM division error");
    };
    let ast_span = ast_span.span.unwrap();
    let vm_span = vm_span.span.unwrap();
    assert_eq!(
        (ast_span.start, ast_span.end, ast_span.line, ast_span.col),
        (vm_span.start, vm_span.end, vm_span.line, vm_span.col)
    );
    assert_eq!(
        runtime.source_map.get(vm_span.source_id).unwrap().source,
        source
    );

    // A copied function with trusted spans is not the registered callback.
    let Value::Function(callback) = ctx.env.get("zi-e-step").unwrap() else {
        panic!("missing frontend step");
    };
    ctx.env.set(
        "forged-frontend".into(),
        Value::Function(Arc::new((*callback).clone())),
    );
    let rejected = bootstrap::eval_source(
        &ctx,
        "boundary.zio",
        "(interpreter/protect forged-frontend [] nil)",
    )
    .unwrap_err();
    assert!(matches!(
        rejected,
        zio_core::error::EvalError::InvalidForm(..)
    ));

    // An incomplete artifact must not borrow its missing helper from Stage 0.
    let mut incomplete: serde_json::Value =
        serde_json::from_str(&bytecode::serialize_module(&stage1).unwrap()).unwrap();
    for function in incomplete["functions"].as_array_mut().unwrap() {
        for instruction in function["code"].as_array_mut().unwrap() {
            if instruction["op"] == "define" && instruction["arg"] == "zc-capture-plan" {
                instruction["arg"] = serde_json::json!("omitted-capture-plan");
            }
        }
    }
    let incomplete = bytecode::deserialize_module(&incomplete.to_string()).unwrap();
    let fresh = context();
    if bytecode::install_compiled_compiler(&fresh, &incomplete).is_ok() {
        assert!(
            bytecode::compile_source(&fresh, "missing.zio", "(fn [x] x)").is_err(),
            "missing compiled helper fell back to the interpreted frontend"
        );
    }
}

#[test]
fn compiled_and_interpreted_agree_on_behavior() {
    // Two engines, one program, one answer. A difference here is the bug
    // this whole slice exists to prevent: a compiler that is merely
    // self-consistent can still be wrong.
    let ctx = context();
    let program = r#"
        (defn classify [n] (cond ((< n 0) :neg) ((= n 0) :zero) (else :pos)))
        (defn total [xs] (loop [i 0 acc 0]
          (if (>= i (count xs)) acc (recur (+ i 1) (+ acc (nth xs i))))))
        (defn make-adder [k] (fn [x] (+ x k)))
        {:labels (map classify [-1 0 1])
         :sum (total [1 2 3 4])
         :closure ((make-adder 10) 5)
         :nested {:a [1 #(2 3)]}}
    "#;
    let expected = bytecode::execute_module(
        &ctx,
        &bytecode::compile_source(&ctx, "prog.zio", program).expect("compile"),
    )
    .expect("execute");

    let interpreted = bootstrap::eval_source(&context(), "prog.zio", program).expect("interpret");
    assert_eq!(
        expected, interpreted,
        "compiled and interpreted forms disagree"
    );
    // And the answer is the one the program says it is, so agreement on
    // the wrong value is not mistaken for success.
    let Value::Map(map) = &expected else {
        panic!("expected a map")
    };
    let get = |key: &str| map.get(&Value::Keyword(key.into())).cloned();
    assert_eq!(get("sum"), Some(Value::Integer(10)));
    assert_eq!(get("closure"), Some(Value::Integer(15)));
}

#[test]
fn malformed_portable_compiler_inputs_are_rejected() {
    let ctx = context();
    let module = bytecode::compile_source(&ctx, "literal.zio", "42").unwrap();
    let wire: serde_json::Value =
        serde_json::from_str(&bytecode::serialize_module(&module).unwrap()).unwrap();
    for corruption in ["version", "instruction", "constant"] {
        let mut invalid = wire.clone();
        match corruption {
            "version" => invalid["version"] = serde_json::json!(999),
            "instruction" => {
                invalid["functions"][0]["code"][0]["op"] = serde_json::json!("invalid-op")
            }
            "constant" => invalid["constants"][0]["kind"] = serde_json::json!("invalid-kind"),
            _ => unreachable!(),
        }
        assert!(
            bytecode::deserialize_module(&invalid.to_string()).is_err(),
            "{corruption}"
        );
    }
}
