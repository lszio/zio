use super::*;
use zio_core::im::vector;

fn run(source: &str, engine: ExecutionEngine, limits: ExecutionLimits, modules: FrozenModules) -> Value {
    evaluate_with_engine(source, "candidate.zio", None, None, limits, modules, engine).unwrap()
}

fn field<'a>(envelope: &'a Value, name: &str) -> &'a Value {
    crate::values::get(envelope, name).unwrap()
}

#[test]
fn isolation_boundaries_hold_for_both_engines() {
    for engine in [ExecutionEngine::Compiled, ExecutionEngine::Interpreter] {
        let limits = ExecutionLimits::default();
        let good = run("(+ 20 22)", engine, limits.clone(), FrozenModules::new());
        assert_eq!(field(&good, "status"), &Value::String("completed".into()));
        assert_eq!(field(&good, "result"), &Value::Integer(42));
        assert!(matches!(field(&good, "steps"), Value::Integer(n) if *n > 0));
        for source in [
            "(host/read-text \"/etc/passwd\")",
            "(slurp \"/etc/passwd\")",
            "(load \"/etc/passwd\")",
            "(recv! (chan))",
            "(defmacro hidden [] '(slurp \"/etc/passwd\")) (hidden)",
            "(module leak (export host/read-text)) (require :leak) (leak/host/read-text \"/etc/passwd\")",
        ] {
            let denied = run(source, engine, limits.clone(), FrozenModules::new());
            assert_eq!(field(&denied, "status"), &Value::String("failed".into()), "{source}");
            assert_eq!(field(&denied, "result"), &Value::Nil);
        }
        let mut short = limits.clone();
        short.max_steps = 2000;
        for source in [
            "(defn forever [] (forever)) (forever)",
            "(loop [] (recur))",
            "(defmacro forever [] (forever)) (forever)",
            "(map (fn [x] (loop [] (recur))) [1])",
        ] {
            let stopped = run(source, engine, short.clone(), FrozenModules::new());
            assert_eq!(field(&stopped, "status"), &Value::String("failed".into()), "{source}");
            assert!(matches!(field(&stopped, "error"), Value::String(error) if error.contains("limit") || error.contains("depth") || error.contains("timeout")), "{stopped:?}");
        }
        let mut output_limit = limits.clone();
        output_limit.max_output_bytes = 5;
        let output = run("(println \"ééé\") 9", engine, output_limit, FrozenModules::new());
        assert_eq!(field(&output, "result"), &Value::Integer(9));
        assert_eq!(field(&output, "output"), &Value::String("éé\n".into()));
        assert_eq!(field(&output, "output-truncated"), &Value::Boolean(true));

        let modules = FrozenModules::from([("helper".into(), b"(defn answer [] 42) (export answer)".to_vec())]);
        let dependency = run("(require :helper) (answer)", engine, limits.clone(), modules);
        assert_eq!(field(&dependency, "result"), &Value::Integer(42));
        let cycles = FrozenModules::from([
            ("a".into(), b"(require :b)".to_vec()),
            ("b".into(), b"(require :a)".to_vec()),
        ]);
        let cycle = run("(require :a)", engine, limits.clone(), cycles);
        assert!(matches!(field(&cycle, "error"), Value::String(error) if error.contains("circular")));
        let module_loop = FrozenModules::from([("looping".into(), b"(loop [] (recur))".to_vec())]);
        let stopped = run("(require :looping)", engine, short, module_loop);
        assert_eq!(field(&stopped, "status"), &Value::String("failed".into()));

        let unsupported = run("(fn [] 1)", engine, limits, FrozenModules::new());
        assert_eq!(field(&unsupported, "status"), &Value::String("failed".into()));
        assert!(matches!(field(&unsupported, "error"), Value::String(error) if error.contains("JSON")));
    }
}

#[test]
fn strict_input_and_limit_refusals_never_run_candidate() {
    let input = Value::NativeFunction(NativeFn::new("secret", |_, _| Ok(Value::String("secret".into()))));
    let result = evaluate("(println \"ran\")", "input.zio", None, Some(input), ExecutionLimits::default(), FrozenModules::new()).unwrap();
    assert_eq!(field(&result, "status"), &Value::String("refused".into()));
    assert_eq!(field(&result, "output"), &Value::String(String::new()));
    assert_eq!(field(&result, "steps"), &Value::Integer(0));
    let mut limits = ExecutionLimits::default();
    limits.max_steps = 0;
    let result = run("42", ExecutionEngine::Compiled, limits, FrozenModules::new());
    assert_eq!(field(&result, "status"), &Value::String("refused".into()));
}

#[test]
fn entry_input_is_data_and_candidate_status_is_only_a_result() {
    let result = evaluate("(defn main [input] {:status \"completed\" :answer (+ (get input \"n\") 1)})", "entry.zio", Some("main"), Some(crate::values::from_json(serde_json::json!({"n": 41})).unwrap()), ExecutionLimits::default(), FrozenModules::new()).unwrap();
    assert_eq!(field(&result, "status"), &Value::String("completed".into()));
    assert_eq!(crate::values::to_json(field(&result, "result")).unwrap(), serde_json::json!({"status":"completed","answer":42}));
    assert!(matches!(field(&result, "traces"), Value::Vector(events) if events.iter().any(|event| crate::values::get(event, "kind") == Some(&Value::String("call".into())))));
}

#[test]
fn installer_does_not_capture_parent_authority() {
    let ctx = zio_core::bootstrap::language_context(zio_core::bootstrap::ModuleRoots::empty()).unwrap();
    ctx.env.set("secret".into(), Value::NativeFunction(NativeFn::new("secret", |_, _| Ok(Value::String("secret".into())))));
    install(&ctx, &HostPolicy::default());
    let request = crate::values::map([("source", Value::String("(secret)".into()))]);
    let isolated = ctx.env.get("host/evaluate-isolated").unwrap();
    let result = crate::values::invoke(isolated, vector![request], &ctx).unwrap();
    assert_eq!(field(&result, "status"), &Value::String("failed".into()));
    assert!(matches!(field(&result, "error"), Value::String(error) if error.contains("secret")));
}
