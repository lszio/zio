//! L3 contract (ADR-016 / synthesis-plan §3): the generalized learning
//! loop under a scripted mock — the key acceptance is that a depth-3
//! task, structurally out of the enumerator's reach, is solved when the
//! LLM proposer proposes the answer. Offline by construction.

use std::path::PathBuf;
use std::sync::Arc;

use zio_ai::mock::{RecordingLlmHost, ScriptedLlmHost};
use zio_ai::install;
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::value::Value;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("ai crate lives below workspace root")
        .to_path_buf()
}

fn eval_str(ctx: &EvalContext, src: &str) -> Result<Value, EvalError> {
    let source_id = ctx.source_map().register("learn3-test".into(), src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(src, source_id)
        .map_err(|e| EvalError::custom(format!("parse error: {e}")))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = zio_core::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

fn load_source(ctx: &EvalContext, name: &str, source: &str) {
    let source_id = ctx.source_map().register(name.into(), source.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(source, source_id)
        .expect("test source must parse");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, ctx)
            .unwrap_or_else(|e| panic!("loading {name}: {e}"));
    }
}

/// stdlib + proposer.zio + learn.zio, with a recording-wrapped scripted
/// llm host installed.
fn learn_ctx(
    scripted: ScriptedLlmHost,
) -> (EvalContext, Arc<RecordingLlmHost>, Arc<ScriptedLlmHost>) {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = EvalContext::new(env);
    load_source(&ctx, "core.zio", zio_core::stdlib_source());
    let scripted = Arc::new(scripted);
    let recorder = Arc::new(RecordingLlmHost::new(scripted.clone()));
    install(&ctx, Some(recorder.clone()), None);
    for lib in ["proposer.zio", "learn.zio"] {
        let path = workspace_root().join("lib").join("zio").join(lib);
        let source = std::fs::read_to_string(&path).expect("lib exists");
        load_source(&ctx, lib, &source);
    }
    eval_str(&ctx, "(def propose (make-llm-proposer {:k 3 :retries 1}))").unwrap();
    (ctx, recorder, scripted)
}

/// Value Display is full read syntax, so a synthesized form renders back
/// to zio source directly.
fn render(value: &Value) -> String {
    format!("{value}")
}

fn bind_wrapped(ctx: &EvalContext, name: &str, expr: &Value) {
    eval_str(
        ctx,
        &format!("(def {name} (learn--wrap-function '{}))", render(expr)),
    )
    .expect("wrap def");
}

fn learn_with_proposer(ctx: &EvalContext, samples: &str, extra: &str) -> Result<Value, EvalError> {
    eval_str(
        ctx,
        &format!(
            "(learn-function '{samples} {{:ops '[+ * /] :constants '[0 1 2] \
             :max-depth 3 :beam-width 16 :proposer propose {extra}}})"
        ),
    )
}

#[test]
fn hybrid_proposer_merges_llm_and_enumeration() {
    // 混合提议器 = 多路候选 merge:LLM 的答案与枚举批一起进闸。
    // (in-script hybrid runs hit the core-eval defect noted in the learn
    // contract; the Rust-driven context is stable.)
    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new(["(+ (* 2 x) 1)".into()]));
    eval_str(
        &ctx,
        "(def hyb (make-hybrid-proposer propose (make-enum-proposer {})))",
    )
    .unwrap();
    let expr = eval_str(
        &ctx,
        "(learn-function '[[1 3] [2 5] [3 7]] {:ops '[+ - *] :constants '[0 1 2] \
         :beam-width 8 :max-depth 2 :max-generations 2 :proposer hyb})",
    )
    .unwrap();
    bind_wrapped(&ctx, "h", &expr);
    for (x, want) in [(1, 3), (2, 5), (10, 21)] {
        let got = eval_str(&ctx, &format!("(h {x})")).unwrap();
        assert_eq!(got, Value::Integer(want), "h({x})");
    }
    assert_eq!(recorder.calls(), 1);
}

#[test]
fn mock_proposer_solves_depth2_regression() {
    // the design-spec example: [[1 3] [2 5] [3 7]] → 2x+1, via an LLM
    // proposer instead of enumeration
    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new(["(+ (* 2 x) 1)".into()]));
    let expr = learn_with_proposer(&ctx, "[[1 3] [2 5] [3 7]]", "").unwrap();
    bind_wrapped(&ctx, "f", &expr);
    for (x, want) in [(1, 3), (2, 5), (3, 7), (10, 21)] {
        let got = eval_str(&ctx, &format!("(f {x})")).unwrap();
        assert_eq!(got, Value::Integer(want), "f({x})");
    }
    assert_eq!(recorder.calls(), 1);
}

#[test]
fn llm_solves_what_enumeration_cannot_reach() {
    // y = x³+1 is a pure depth-3 chain — it has no shallow (depth ≤ 2)
    // rewrite, so the enumerator at :max-depth 3 must NOT reach zero
    // loss; the scripted proposer must.
    let samples = "[[1 2] [2 9] [3 28]]";

    let (ctx, _, _) = learn_ctx(ScriptedLlmHost::new(["(+ 0 0)".into()]));
    let expr = eval_str(
        &ctx,
        &format!(
            "(learn-function '{samples} {{:ops '[+ * /] :constants '[0 1 2] \
             :max-depth 3 :beam-width 8 :max-evals 500}})"
        ),
    )
    .unwrap();
    bind_wrapped(&ctx, "f", &expr);
    let f1 = eval_str(&ctx, "(f 1)").unwrap();
    let f2 = eval_str(&ctx, "(f 2)").unwrap();
    let f3 = eval_str(&ctx, "(f 3)").unwrap();
    let solved = f1 == Value::Integer(2) && f2 == Value::Integer(9) && f3 == Value::Integer(28);
    assert!(!solved, "enumeration must not reach x³+1");

    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new(["(+ (* x (* x x)) 1)".into()]));
    let expr = learn_with_proposer(&ctx, samples, "").unwrap();
    assert!(
        matches!(expr, Value::List(_)),
        "expected a synthesized form, got {expr:?}"
    );
    bind_wrapped(&ctx, "g", &expr);
    for (x, want) in [(1, 2), (2, 9), (3, 28), (4, 65)] {
        let got = eval_str(&ctx, &format!("(g {x})")).unwrap();
        assert_eq!(got, Value::Integer(want), "g({x})");
    }
    assert_eq!(recorder.calls(), 1);
}

#[test]
fn generation_budget_caps_proposer_calls() {
    // two generations max → exactly two proposer calls, no zero loss
    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new([
        "(* x x)".into(),
        "(* (* x x) x)".into(),
    ]));
    let expr = learn_with_proposer(&ctx, "[[1 3] [2 5] [3 7]]", ":max-generations 2").unwrap();
    assert!(matches!(expr, Value::List(_)), "best-effort form, got {expr:?}");
    assert_eq!(recorder.calls(), 2);
}

#[test]
fn eval_budget_stops_the_loop() {
    // one candidate per generation × 2 samples = 2 evals per generation;
    // :max-evals 4 must stop after the second generation
    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new([
        "(* x x)".into(),
        "(* (* x x) x)".into(),
        "(* (* (* x x) x) x)".into(),
    ]));
    let _ = learn_with_proposer(&ctx, "[[1 3] [2 5]]", ":max-evals 4").unwrap();
    assert_eq!(recorder.calls(), 2, "eval budget must stop after generation 2");
}

#[test]
fn whitelist_rejects_undeclared_constants_from_the_proposer() {
    // 99 is not a declared constant: every candidate dies at the gate,
    // the run returns nil, and both generations still ask the proposer
    let (ctx, recorder, _) = learn_ctx(ScriptedLlmHost::new([
        "(+ x 99)".into(),
        "(* x 99)".into(),
    ]));
    let got = learn_with_proposer(&ctx, "[[1 3] [2 5]]", ":max-generations 2").unwrap();
    assert_eq!(got, Value::Nil, "no candidate may survive the gate");
    assert_eq!(recorder.calls(), 2);
}

#[test]
fn candidate_eval_errors_are_isolated() {
    // (/ 0 0) detonates on every sample; the run must survive, score the
    // candidate worst, and return its best-effort form instead of an error
    let (ctx, _, _) = learn_ctx(ScriptedLlmHost::new(["(+ (/ 0 0) x)".into()]));
    let got = learn_with_proposer(&ctx, "[[1 3] [2 5]]", ":max-generations 2");
    assert!(got.is_ok(), "candidate crash must not kill the run: {got:?}");
}

#[test]
fn canonical_dedup_collapses_commutative_twins() {
    // "(+ x 1)" and "(+ 1 x)" are distinct strings, one canonical form:
    // only 2 fresh forms survive from the 3-line batch
    let (ctx, _, _) = learn_ctx(ScriptedLlmHost::new(["(+ x 1)".into()]));
    let got = eval_str(
        &ctx,
        r#"(count (learn--check-batch '["(+ x 1)" "(+ 1 x)" "(* x 2)"] {}
                    (learn--make-task '[[1 3]] {:ops '[+ *] :constants '[0 1 2]})))"#,
    )
    .unwrap();
    assert_eq!(got, Value::Integer(2));
}
