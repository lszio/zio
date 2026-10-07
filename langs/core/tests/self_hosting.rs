//! Self-hosting evidence, run as its own binary.
//!
//! These checks exist as an integration test rather than a unit test for
//! one reason: `EvalContext` is deliberately `!Send` — a context owns
//! mutable registries and must not cross a thread — so the work cannot be
//! moved onto a thread with a bigger stack. A test harness thread gets
//! 2 MiB, and compiling the compiler recursively needs more than that.
//!
//! The main thread of a test binary gets the platform's default stack,
//! which is enough. If this ever starts overflowing, the fix is a depth
//! bound in the compiler's form walk, not a bigger stack.

use zio_core::bootstrap::{self, ModuleRoots};
use zio_core::bytecode;
use zio_core::value::Value;

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
    assert_eq!(build.hashes.len(), 2, "expected two compile stages");
    assert_eq!(
        build.hashes[0], build.hashes[1],
        "stage 1 and stage 2 differ"
    );
    assert!(build.equal, "self-build reported unequal stages");
    assert!(
        build.normalized[0].len() > 1024,
        "normalized artifact is suspiciously small ({} bytes)",
        build.normalized[0].len()
    );
}

#[test]
fn the_compiled_compiler_compiles_programs() {
    // Stage 2 only matters if it can do the work. Every case here is a
    // construct the compiler has to lower, so a failure names the form
    // that regressed rather than a general "compilation broke".
    let ctx = context();
    let source = include_str!("../../compiler/compiler.zio");
    let stage1 = bytecode::compile_source(&ctx, "compiler.zio", source).expect("stage 1");
    bytecode::install_compiled_for_test(&ctx, &stage1).expect("install stage 1");

    for (label, program) in [
        ("defn", "(defn sq [x] (* x x))"),
        ("let*", "(let* [a 1 b 2] (+ a b))"),
        (
            "loop/recur",
            "(loop [n 0 acc 1] (if (< n 5) (recur (+ n 1) (* acc 2)) acc))",
        ),
        ("closure", "(defn mk [k] (fn [x] (+ x k)))"),
        ("vector", "#(1 2 3)"),
        ("map", "{:a 1 :b 2}"),
        ("quote", "'(1 2)"),
        // A `cond` clause is a list: `(cond (test expr) ... (else expr))`.
        // A bare `test expr` pair is not a clause, and both engines
        // refuse it — the check is here to keep the two in agreement,
        // not to bless a new syntax.
        ("cond", "(cond (true 1) (else 2))"),
        ("and/or", "(and true false)"),
    ] {
        assert!(
            bytecode::compile_source(&ctx, "trivial.zio", program).is_ok(),
            "compiled compiler failed on {label}"
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

    let interpreted = bootstrap::eval_source(&ctx, "prog.zio", program).expect("interpret");
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
