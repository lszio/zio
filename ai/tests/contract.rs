//! Contract tests for zio-ai (ADR-016). Everything runs offline: the only
//! hosts involved are scripted, replay, synthetic, and recording mocks.

use std::sync::Arc;

use zio_ai::mock::{
    MockEmbedHost, MockLlmHost, RecordingEmbedHost, RecordingLlmHost, ScriptedEmbedHost,
    ScriptedLlmHost, SyntheticEmbedHost,
};
use zio_ai::{EmbedHost, HostErrorKind, LlmHost, install};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::value::Value;

fn test_ctx(llm: Option<Arc<dyn LlmHost>>, embed: Option<Arc<dyn EmbedHost>>) -> EvalContext {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = EvalContext::new(env);
    install(&ctx, llm, embed);
    ctx
}

/// Evaluate every top-level form; return the last value (script-runner
/// semantics, no stdlib — the bindings under test are self-contained).
fn eval_str(ctx: &EvalContext, src: &str) -> Result<Value, EvalError> {
    let source_id = ctx.source_map().register("contract-test".into(), src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(src, source_id)
        .map_err(|e| EvalError::custom(format!("parse error: {e}")))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = zio_core::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

fn temp_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("zio-ai-contract-{}-{}", std::process::id(), name))
}

// ── capability-denied ───────────────────────────────────────────

#[test]
fn no_host_calls_fail_with_capability_denied_prefix() {
    let ctx = test_ctx(None, None);
    for src in [r#"(llm-complete "hi")"#, r#"(embed "x")"#] {
        let err = eval_str(&ctx, src).expect_err(src);
        assert!(
            err.to_string().starts_with("capability-denied:"),
            "expected capability-denied prefix, got: {err}"
        );
    }
}

#[test]
fn partial_install_denies_only_the_missing_capability() {
    let scripted: Arc<dyn LlmHost> = Arc::new(ScriptedLlmHost::new(["ok".into()]));
    let ctx = test_ctx(Some(scripted), None);
    let value = eval_str(&ctx, r#"(llm-complete "hi")"#).unwrap();
    assert_eq!(value, Value::String("ok".into()));
    let err = eval_str(&ctx, r#"(embed "x")"#).unwrap_err();
    assert!(err.to_string().starts_with("capability-denied:"));
}

// ── llm-complete + options ──────────────────────────────────────

#[test]
fn llm_complete_passes_prompt_and_options_to_the_host() {
    let scripted = Arc::new(ScriptedLlmHost::new(["done".into()]));
    let handle = scripted.clone();
    let host: Arc<dyn LlmHost> = Arc::new(RecordingLlmHost::new(scripted));
    let ctx = test_ctx(Some(host), None);
    let value = eval_str(
        &ctx,
        r#"(llm-complete "2+2?" :temperature 0.5 :max-tokens 32 :stop "\n")"#,
    )
    .unwrap();
    assert_eq!(value, Value::String("done".into()));
    assert_eq!(handle.last_prompt.borrow().as_str(), "2+2?");
    let opts = handle.last_opts.borrow();
    assert_eq!(opts.temperature, Some(0.5));
    assert_eq!(opts.max_tokens, Some(32));
    assert_eq!(opts.stop, vec!["\n".to_string()]);
}

#[test]
fn llm_complete_rejects_bad_arguments() {
    let scripted: Arc<dyn LlmHost> = Arc::new(ScriptedLlmHost::new(["x".into()]));
    let ctx = test_ctx(Some(scripted), None);
    let err = eval_str(&ctx, "(llm-complete 42)").unwrap_err();
    assert!(err.to_string().contains("prompt must be a string"), "{err}");
    let err = eval_str(&ctx, r#"(llm-complete "p" :bogus 1)"#).unwrap_err();
    assert!(err.to_string().contains("unknown option :bogus"), "{err}");
}

// ── record → replay roundtrip ───────────────────────────────────

#[test]
fn record_then_replay_is_deterministic_for_llm() {
    let scripted = Arc::new(ScriptedLlmHost::new(["4".into()]));
    let recorder = Arc::new(RecordingLlmHost::new(scripted));
    let host: Arc<dyn LlmHost> = recorder.clone();
    let ctx = test_ctx(Some(host), None);
    let first = eval_str(&ctx, r#"(llm-complete "2+2?")"#).unwrap();

    let path = temp_file("llm-recording.zio");
    assert_eq!(recorder.save(&path).unwrap(), 1);
    let mock = MockLlmHost::from_recording_file(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    let ctx = test_ctx(Some(Arc::new(mock)), None);
    let replayed_a = eval_str(&ctx, r#"(llm-complete "2+2?")"#).unwrap();
    let replayed_b = eval_str(&ctx, r#"(llm-complete "2+2?")"#).unwrap();
    assert_eq!(first, Value::String("4".into()));
    assert_eq!(replayed_a, first);
    assert_eq!(replayed_b, first);
}

#[test]
fn record_then_replay_is_deterministic_for_embeddings() {
    let scripted = Arc::new(ScriptedEmbedHost::new([vec![vec![0.5, -1.25], vec![0.0, 2.0]]]));
    let recorder = Arc::new(RecordingEmbedHost::new(scripted));
    let host: Arc<dyn EmbedHost> = recorder.clone();
    let ctx = test_ctx(None, Some(host));
    let value = eval_str(&ctx, r#"(embed ["a" "b"])"#).unwrap();
    let rows = match &value {
        Value::Vector(rows) => rows.clone(),
        other => panic!("expected vector of vectors, got {other:?}"),
    };
    assert_eq!(rows.len(), 2);

    let path = temp_file("embed-recording.zio");
    recorder.save(&path).unwrap();
    let mock = MockEmbedHost::from_recording_file(&path).unwrap();
    std::fs::remove_file(&path).unwrap();

    let ctx = test_ctx(None, Some(Arc::new(mock)));
    let replayed = eval_str(&ctx, r#"(embed ["a" "b"])"#).unwrap();
    assert_eq!(replayed, value);
}

#[test]
fn recordings_survive_escapes_and_utf8() {
    let prompt = "quote \" backslash \\ newline \n tab \t 中文 ✓";
    let response = "line1\nline2";
    let text = zio_ai::mock::test_support::llm_recording_text(&[(
        prompt.to_string(),
        response.to_string(),
    )]);
    let mock = MockLlmHost::from_recording_text(&text).unwrap();
    let answer = mock.complete(prompt, &Default::default()).unwrap();
    assert_eq!(answer, response);
}

// ── replay-miss fail-fast ───────────────────────────────────────

#[test]
fn replay_miss_fails_fast_instead_of_touching_the_network() {
    let mock = MockLlmHost::from_recording_text(r#"["known prompt" "known answer"]"#).unwrap();
    let err = mock.complete("unknown prompt", &Default::default()).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::ReplayMiss);
    assert!(err.to_string().contains("replay-miss"), "{err}");
    assert!(err.to_string().contains("fail-fast"), "{err}");

    let known = mock.complete("known prompt", &Default::default()).unwrap();
    assert_eq!(known, "known answer");
}

#[test]
fn embedding_replay_miss_fails_fast() {
    let mock = MockEmbedHost::from_recording_text(r#"[["known"] (0.1 0.2)]"#).unwrap();
    let err = mock.embed(&["other".into()]).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::ReplayMiss);
    let vectors = mock.embed(&["known".into()]).unwrap();
    assert_eq!(vectors, vec![vec![0.1, 0.2]]);
}

// ── synthetic embedder ──────────────────────────────────────────

#[test]
fn synthetic_embeddings_are_deterministic_and_unit_length() {
    let host = SyntheticEmbedHost::new(16);
    let a = host.embed(&["hello".into()]).unwrap();
    let b = host.embed(&["hello".into()]).unwrap();
    assert_eq!(a, b);
    let norm: f64 = a[0].iter().map(|x| x * x).sum::<f64>().sqrt();
    assert!((norm - 1.0).abs() < 1e-9);
    let c = host.embed(&["world".into()]).unwrap();
    assert_ne!(a[0], c[0]);
}
