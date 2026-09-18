//! L2 contract (ADR-016 / synthesis-plan §3): `make-llm-proposer` end to
//! end under scripted and replay mocks — offline by construction. Also
//! the 对拍 between the pure-Zio answer parser and its Rust mirror.

use std::path::PathBuf;
use std::sync::Arc;

use zio_ai::mock::test_support::parse_answer_lines;
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
    let source_id = ctx.source_map().register("proposer-test".into(), src.to_string());
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

fn bare_ctx() -> EvalContext {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = EvalContext::new(env);
    load_source(&ctx, "core.zio", zio_core::stdlib_source());
    ctx
}

/// A context with stdlib + proposer.zio loaded and a recording-wrapped
/// scripted llm host installed. Returns the scripted host handle so tests
/// can inspect the last prompt/options.
fn proposer_ctx(scripted: ScriptedLlmHost) -> (EvalContext, Arc<RecordingLlmHost>, Arc<ScriptedLlmHost>) {
    let ctx = bare_ctx();
    let scripted = Arc::new(scripted);
    let recorder = Arc::new(RecordingLlmHost::new(scripted.clone()));
    install(&ctx, Some(recorder.clone()), None);
    let path = workspace_root().join("lib").join("zio").join("proposer.zio");
    let source = std::fs::read_to_string(&path).expect("proposer.zio exists");
    load_source(&ctx, "proposer.zio", &source);
    eval_str(&ctx, "(def propose (make-llm-proposer {:k 3 :retries 1}))").unwrap();
    (ctx, recorder, scripted)
}

const TASK: &str = "{:samples '[[1 3] [2 5]] :ops '[+ *] :constants '[0 1] :max-depth 2}";

fn propose(ctx: &EvalContext, history: &str) -> Result<Value, EvalError> {
    eval_str(ctx, &format!("(propose {TASK} {history})"))
}

fn as_strings(value: Value) -> Vec<String> {
    match value {
        Value::Vector(rows) | Value::List(rows) => rows
            .into_iter()
            .map(|v| match v {
                Value::String(s) => s,
                other => panic!("candidate must be a string, got {other:?}"),
            })
            .collect(),
        other => panic!("expected candidates vector, got {other:?}"),
    }
}

#[test]
fn proposer_success_is_a_single_call() {
    let (ctx, recorder, _) = proposer_ctx(ScriptedLlmHost::new(["(+ x 1)".into()]));
    let got = propose(&ctx, "[]").unwrap();
    assert_eq!(as_strings(got), vec!["(+ x 1)"]);
    assert_eq!(recorder.calls(), 1);
}

#[test]
fn proposer_parses_lines_and_drops_noise() {
    let response = "```lisp\n(+ x 1)\n;; a note\n\n   (* (+ x 1) 2)   \n```\n";
    let (ctx, recorder, _) = proposer_ctx(ScriptedLlmHost::new([response.into()]));
    let got = propose(&ctx, "[]").unwrap();
    assert_eq!(as_strings(got), vec!["(+ x 1)", "(* (+ x 1) 2)"]);
    assert_eq!(recorder.calls(), 1);
}

/// Format-level garbage: every line is a comment or a fence, so nothing
/// survives the line protocol. (Semantic garbage — prose that looks like
/// a line — is the loop's whitelist problem in L3, not the proposer's.)
const FORMAT_GARBAGE: &str = "```text\n;; thinking out loud\n;; still thinking\n```";

#[test]
fn proposer_retries_then_succeeds() {
    let (ctx, recorder, _) =
        proposer_ctx(ScriptedLlmHost::new([FORMAT_GARBAGE.into(), "(+ x 1)".into()]));
    let got = propose(&ctx, "[]").unwrap();
    assert_eq!(as_strings(got), vec!["(+ x 1)"]);
    assert_eq!(recorder.calls(), 2);
}

#[test]
fn proposer_returns_empty_after_exhausted_retries() {
    let (ctx, recorder, _) =
        proposer_ctx(ScriptedLlmHost::new([FORMAT_GARBAGE.into(), FORMAT_GARBAGE.into()]));
    let got = propose(&ctx, "[]").unwrap();
    assert_eq!(as_strings(got), Vec::<String>::new());
    assert_eq!(recorder.calls(), 2); // initial attempt + 1 retry
}

#[test]
fn proposer_caps_at_k_and_dedups_first_occurrence() {
    let response = "(+ x 1)\n(+ x 1)\n(* x 2)\n(- x 1)\n(+ 0 x)";
    let (ctx, _, _) = proposer_ctx(ScriptedLlmHost::new([response.into()]));
    let got = propose(&ctx, "[]").unwrap();
    assert_eq!(as_strings(got), vec!["(+ x 1)", "(* x 2)", "(- x 1)"]);
}

#[test]
fn proposer_is_deterministic_across_calls() {
    let (ctx, _, handle) =
        proposer_ctx(ScriptedLlmHost::new(["(+ x 1)".into(), "(+ x 1)".into()]));
    let a = propose(&ctx, "[]").unwrap();
    let b = propose(&ctx, "[]").unwrap();
    assert_eq!(a, b);
    // the two prompts were the plain rendered prompt (no correction
    // block, which always starts with ";; correction:")
    assert!(!handle.last_prompt.borrow().contains(";; correction:"));
}

#[test]
fn history_and_task_render_into_the_prompt() {
    let (ctx, _, handle) =
        proposer_ctx(ScriptedLlmHost::new(["(+ x 1)".into(), "(* x 2)".into()]));
    let _ = propose(&ctx, "[]").unwrap();
    let without_history = handle.last_prompt.borrow().clone();
    assert!(!without_history.contains("generation 0"));

    let _ = propose(
        &ctx,
        r#"'({:generation 0 :candidates ["(+ x 1)"] :scores [1002]})"#,
    )
    .unwrap();
    let with_history = handle.last_prompt.borrow().clone();
    assert!(with_history.contains("generation 0"), "{with_history}");
    assert!(with_history.contains("(+ x 1)"), "{with_history}");
    assert!(with_history.contains("(allowed-ops"), "{with_history}");
}

#[test]
fn missing_host_surfaces_capability_denied() {
    // stdlib + proposer loaded, hosts installed as None: the bindings
    // exist but deny. llm--ask must re-raise that configuration error
    // instead of burning retries on it.
    let ctx = bare_ctx();
    install(&ctx, None, None);
    let path = workspace_root().join("lib").join("zio").join("proposer.zio");
    let source = std::fs::read_to_string(&path).unwrap();
    load_source(&ctx, "proposer.zio", &source);
    eval_str(&ctx, "(def propose (make-llm-proposer {:k 3 :retries 2}))").unwrap();
    let err = propose(&ctx, "[]").unwrap_err();
    assert!(
        err.to_string().contains("capability-denied"),
        "expected capability-denied, got: {err}"
    );
}

#[test]
fn zio_parse_matches_the_rust_reference() {
    let answers = [
        "(+ x 1)\n(* x 2)\n",
        "```lisp\n(+ x 1)\n;; note\n\n   (* (+ x 1) 2)   \n```\n",
        "no newline at end",
        "\n\n\n",
        ";only a comment",
        "",
    ];
    let (ctx, _, _) = proposer_ctx(ScriptedLlmHost::new(["(+ x 1)".into()]));
    for answer in answers {
        let literal = {
            // render the answer as a zio string literal via json-free
            // escaping: backslash and quote and newlines.
            let mut out = String::from("\"");
            for c in answer.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '"' => out.push_str("\\\""),
                    '\n' => out.push_str("\\n"),
                    other => out.push(other),
                }
            }
            out.push('"');
            out
        };
        let zio_lines = eval_str(&ctx, &format!("(llm--parse-answer {literal})")).unwrap();
        let expected: Vec<Value> = parse_answer_lines(answer)
            .into_iter()
            .map(Value::String)
            .collect();
        assert_eq!(zio_lines, Value::Vector(expected.into_iter().collect()), "answer: {answer:?}");
    }
}
