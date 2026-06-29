use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::sync::Arc;

use zio_core::builtins;
use zio_core::context::EvalContext;
use zio_core::env::Env;
use zio_core::eval;
use zio_core::reader;
use zio_core::value::Value;

fn make_ctx() -> EvalContext {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    EvalContext::new(env)
}

fn eval_str(ctx: &EvalContext, input: &str) -> Value {
    let sexp = reader::read(input).unwrap();
    eval::eval_in_context(&sexp, ctx).unwrap()
}

fn load_stdlib(ctx: &EvalContext) {
    let source = zio_core::stdlib_source();
    let wrapped = format!("(do\n{source}\n)");
    let sexp = reader::read(&wrapped).unwrap();
    eval::eval_in_context(&sexp, ctx).unwrap();
}

fn define_helpers(ctx: &EvalContext) {
    eval_str(ctx, "(defn zero? [n] (= n 0))");
    eval_str(ctx, "(defn dec [n] (- n 1))");
}

// ── Arithmetic ───────────────────────────────────────────────────────

fn bench_arithmetic(c: &mut Criterion) {
    let ctx = make_ctx();

    c.bench_function("add_10_ints", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(+ 1 2 3 4 5 6 7 8 9 10)")))
    });

    define_helpers(&ctx);
    eval_str(&ctx, "(defn countdown [n] (if (zero? n) n (countdown (dec n))))");

    c.bench_function("loop_1k", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(countdown 1000)")))
    });

    c.bench_function("loop_10k", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(countdown 10000)")))
    });
}

// ── Function calls ──────────────────────────────────────────────────

fn bench_function_call(c: &mut Criterion) {
    let ctx = make_ctx();
    eval_str(&ctx, "(defn add [a b] (+ a b))");

    c.bench_function("fn_call", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(add 1 2)")))
    });

    eval_str(&ctx, "(defn make-add [x] (fn [y] (+ x y)))");
    eval_str(&ctx, "(def add5 (make-add 5))");

    c.bench_function("closure_call", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(add5 3)")))
    });
}

// ── List operations ─────────────────────────────────────────────────

fn bench_list_ops(c: &mut Criterion) {
    let ctx = make_ctx();
    eval_str(&ctx, "(def lst (list 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15))");
    eval_str(&ctx, "(defn double [x] (* x 2))");

    c.bench_function("map_over_15", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(map double lst)")))
    });

    c.bench_function("filter_over_15", |b| {
        eval_str(&ctx, "(defn even-check [x] (= (mod x 2) 0))");
        b.iter(|| black_box(eval_str(&ctx, "(filter even-check lst)")))
    });

    c.bench_function("cons_chain_100", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(cons 1 (cons 2 (cons 3 (cons 4 (cons 5 nil)))))")))
    });
}

// ── Macro expansion ─────────────────────────────────────────────────

fn bench_macro_expansion(c: &mut Criterion) {
    let ctx = make_ctx();
    eval_str(&ctx, "(defmacro unless [test body] (list 'if test nil body))");

    c.bench_function("macro_expand", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(unless false 42)")))
    });
}

// ── GF Dispatch ─────────────────────────────────────────────────────

fn bench_gf_dispatch(c: &mut Criterion) {
    let ctx = make_ctx();
    eval_str(&ctx, "(defclass shape nil ())");
    eval_str(&ctx, "(defclass circle (shape) ())");
    eval_str(&ctx, "(defclass square (shape) ())");
    eval_str(&ctx, "(defgeneric draw (x))");
    eval_str(&ctx, "(defmethod draw ((x shape)) (str \"shape\"))");
    eval_str(&ctx, "(defmethod draw ((x circle)) (str \"circle\"))");
    eval_str(&ctx, "(defmethod draw ((x square)) (str \"square\"))");
    eval_str(&ctx, "(def ci (make-instance circle))");
    eval_str(&ctx, "(def si (make-instance square))");

    c.bench_function("gf_dispatch_circle", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(draw ci)")))
    });

    c.bench_function("gf_dispatch_square", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(draw si)")))
    });

    // Multi-dispatch
    eval_str(&ctx, "(defclass vehicle nil ())");
    eval_str(&ctx, "(defclass car (vehicle) ())");
    eval_str(&ctx, "(defclass truck (vehicle) ())");
    eval_str(&ctx, "(defgeneric collide (a b))");
    eval_str(&ctx, "(defmethod collide ((a vehicle) (b vehicle)) (str \"generic\"))");
    eval_str(&ctx, "(defmethod collide ((a car) (b truck)) (str \"car-truck\"))");
    eval_str(&ctx, "(def ci2 (make-instance car))");
    eval_str(&ctx, "(def ti (make-instance truck))");

    c.bench_function("gf_multi_dispatch", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(collide ci2 ti)")))
    });
}

// ── defstruct ───────────────────────────────────────────────────────

fn bench_defstruct(c: &mut Criterion) {
    let ctx = make_ctx();
    load_stdlib(&ctx);
    eval_str(&ctx, "(defstruct point [x y])");
    eval_str(&ctx, "(def p (point 10 20))");

    c.bench_function("defstruct_construction", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(point 1 2)")))
    });

    c.bench_function("defstruct_access", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(point-x p)")))
    });
}

// ── Error handling ──────────────────────────────────────────────────

fn bench_error_handling(c: &mut Criterion) {
    let ctx = make_ctx();

    c.bench_function("try_catch_no_error", |b| {
        b.iter(|| black_box(eval_str(&ctx, "(try 42)")))
    });
}

criterion_group!(
    benches,
    bench_arithmetic,
    bench_function_call,
    bench_list_ops,
    bench_macro_expansion,
    bench_gf_dispatch,
    bench_defstruct,
    bench_error_handling,
);
criterion_main!(benches);
