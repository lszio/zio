//! W15 recipe contract: a declaration nobody checks is a comment.
//!
//! `libs/learning/learn/recipes.zio` publishes each method as data — which
//! signal kinds it consumes, which layers it may train, what it may
//! spend, and (for the policy and soft-distillation recipes) the extra
//! contract fields. This test proves the host actually enforces that
//! declaration, and that the Zio declaration and the Rust `Recipe` record
//! describe the same thing.
//!
//! What is real here: every refusal below is a `grove::Error` with a
//! stable `ErrorKind`, produced by the real store against a real SQLite
//! database, and the cross-language check evaluates the actual zio
//! source through the actual zio reader.
//!
//!     cargo test -p grove --test recipe_contract

use std::path::PathBuf;
use std::sync::Arc;

use grove::contracts::*;
use grove::store::Store;
use zio_core::context::EvalRuntime;

// ── fixtures ───────────────────────────────────────────────────────

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w15-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn actor(role: ActorRole) -> Actor {
    Actor::new("trainer", role)
}

fn sample_store(name: &str) -> (PathBuf, Store) {
    let root = temp_root(name);
    let store = Store::open(&root).unwrap();
    (root, store)
}

/// A recipe that consumes the given signal kinds, on `task`.
fn recipe(id: &str, task: &str, consumes: Vec<SignalKind>, budget: u32) -> Recipe {
    Recipe {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: task.to_string(),
        consumes,
        trainable_modules: vec!["fusion".to_string()],
        budget_steps: budget,
    }
}

fn signal(kind: SignalKind, field: Option<&str>) -> LearningSignal {
    LearningSignal {
        schema: SCHEMA_VERSION,
        id: format!("sig-{}", kind_name(kind)),
        idempotency_key: format!("key-{}", kind_name(kind)),
        producer: "annotator".to_string(),
        kind,
        task_id: "task-w15".to_string(),
        observation_id: Some("obs-1".to_string()),
        prediction_id: None,
        target_field: field.map(str::to_string),
        content: "class 1".to_string(),
        usage_permitted: true,
        occurred_at_ms: 1_700_000_000_000,
        received_at_ms: 1_700_000_000_000,
        revises: None,
    }
}

fn kind_name(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::TeacherLabel => "teacher-label",
        SignalKind::HumanCorrection => "human-correction",
        SignalKind::HumanPreference => "human-preference",
        SignalKind::Demonstration => "demonstration",
        SignalKind::EnvironmentResult => "environment-result",
        SignalKind::Revision => "revision",
        SignalKind::Retraction => "retraction",
    }
}

fn run_on(recipe_id: &str, task: &str, budget: u32) -> Run {
    Run {
        schema: SCHEMA_VERSION,
        id: format!("run-{recipe_id}"),
        task_id: task.to_string(),
        base_snapshot: digest_bytes(b"base"),
        dataset_revision: "ds-1".to_string(),
        recipe: recipe_id.to_string(),
        state: RunState::Queued,
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    }
}

// ── a recipe is a record, and it round-trips ────────────────────────

#[test]
fn a_recipe_is_persisted_and_loaded_with_its_declaration_intact() {
    let (_root, store) = sample_store("roundtrip");
    let declared = recipe(
        "preference@1",
        "task-w15",
        vec![SignalKind::HumanPreference],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();

    let loaded = store.get_recipe("preference@1").unwrap();
    assert_eq!(loaded.id, declared.id);
    assert_eq!(loaded.task_id, declared.task_id);
    assert_eq!(loaded.budget_steps, 512);
    assert_eq!(loaded.consumes, vec![SignalKind::HumanPreference]);
    assert_eq!(loaded.trainable_modules, vec!["fusion".to_string()]);
    // The canonical bytes are the identity, and they survive the round trip.
    assert_eq!(
        loaded.canonical_bytes().unwrap(),
        declared.canonical_bytes().unwrap()
    );
}

#[test]
fn every_extended_recipe_family_registers_and_is_visible() {
    let (_root, store) = sample_store("families");
    let families: Vec<(&str, Vec<SignalKind>)> = vec![
        (
            "supervised@1",
            vec![SignalKind::TeacherLabel, SignalKind::HumanCorrection],
        ),
        ("hard-distill@1", vec![SignalKind::TeacherLabel]),
        ("soft-distill@1", vec![SignalKind::TeacherLabel]),
        ("preference@1", vec![SignalKind::HumanPreference]),
        ("demonstration@1", vec![SignalKind::Demonstration]),
        ("self-supervised@1", vec![SignalKind::Demonstration]),
        (
            "environment-feedback@1",
            vec![SignalKind::EnvironmentResult],
        ),
        ("policy-gradient@1", vec![SignalKind::EnvironmentResult]),
    ];
    for (id, consumes) in &families {
        store
            .put_recipe(
                &actor(ActorRole::Operator),
                &recipe(id, "task-w15", consumes.clone(), 512),
            )
            .unwrap();
    }
    let listed = store.registered_recipes().unwrap();
    assert_eq!(listed.len(), families.len());
    for (id, consumes) in &families {
        let found = listed
            .iter()
            .find(|r| r.id == *id)
            .unwrap_or_else(|| panic!("recipe {id} did not survive registration"));
        assert_eq!(
            &found.consumes, consumes,
            "recipe {id} lost its consumes list"
        );
    }
}

// ── the `consumes` list gates which signals the store will accept ───

#[test]
fn a_signal_kind_the_recipe_does_not_consume_is_refused() {
    let (_root, store) = sample_store("gate");
    let declared = recipe(
        "soft-distill@1",
        "task-w15",
        vec![SignalKind::TeacherLabel],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();

    // The recipe's own signal is accepted.
    store
        .bind_signal_to_recipe(&declared, &signal(SignalKind::TeacherLabel, None))
        .expect("the declared kind is in force");

    // Preference data under a distillation recipe is an invisible policy
    // change, and is refused rather than dropped later.
    let err = store
        .bind_signal_to_recipe(&declared, &signal(SignalKind::HumanPreference, None))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation, "{err}");
    // The message names what the recipe DOES accept, so the refusal is
    // actionable rather than a bare "no".
    assert!(err.context.contains("teacher-label"), "{err}");
}

#[test]
fn a_retraction_is_in_force_whatever_the_recipe_declares() {
    let (_root, store) = sample_store("retract");
    let declared = recipe(
        "preference@1",
        "task-w15",
        vec![SignalKind::HumanPreference],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();
    // A recipe cannot make it unlawful to withdraw data. Revision and
    // retraction are lifecycle operations, not training material.
    for kind in [SignalKind::Retraction, SignalKind::Revision] {
        store
            .bind_signal_to_recipe(&declared, &signal(kind, None))
            .unwrap_or_else(|e| panic!("{kind:?} must always be in force: {e}"));
    }
}

#[test]
fn a_demonstration_naming_an_untrainable_module_is_refused() {
    let (_root, store) = sample_store("module");
    let mut declared = recipe(
        "demonstration@1",
        "task-w15",
        vec![SignalKind::Demonstration],
        512,
    );
    declared.trainable_modules = vec!["fusion".to_string()];
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();

    store
        .bind_signal_to_recipe(
            &declared,
            &signal(SignalKind::Demonstration, Some("fusion")),
        )
        .expect("a module the recipe may train is fine");

    let err = store
        .bind_signal_to_recipe(&declared, &signal(SignalKind::Demonstration, Some("head")))
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation, "{err}");
    assert!(err.context.contains("head"), "{err}");
}

#[test]
fn the_consumes_gate_and_the_store_permission_are_both_real() {
    // The recipe gate is not a substitute for the store's own permission
    // check: a reader may not file a human correction either.
    let (_root, store) = sample_store("permission");
    let declared = recipe(
        "supervised@1",
        "task-w15",
        vec![SignalKind::HumanCorrection],
        512,
    );
    let correction = signal(SignalKind::HumanCorrection, None);
    store
        .bind_signal_to_recipe(&declared, &correction)
        .expect("the declared kind is in force");
    let err = store
        .submit_signal(&actor(ActorRole::Reader), &correction)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");
    store
        .submit_signal(&actor(ActorRole::Annotator), &correction)
        .expect("an annotator may file a correction");
}

// ── an unknown recipe is refused at the boundary ───────────────────

#[test]
fn an_unknown_recipe_is_refused_before_a_run_ever_starts() {
    let (_root, store) = sample_store("unknown");
    let err = store.require_known_recipe("preference@9").unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable, "{err}");

    // A typo in a run's recipe field cannot become a run that trains
    // under a policy nobody declared.
    let run = run_on("preference@9", "task-w15", 512);
    let err = store.check_run_against_recipe(&run).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable, "{err}");
}

#[test]
fn a_registered_recipe_is_loaded_by_its_id() {
    let (_root, store) = sample_store("known");
    let declared = recipe(
        "policy-gradient@1",
        "task-w15",
        vec![SignalKind::EnvironmentResult],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();
    let found = store.require_known_recipe("policy-gradient@1").unwrap();
    assert_eq!(found.id, "policy-gradient@1");
    assert_eq!(Store::recipe_consumes(&found), vec!["environment-result"]);
}

// ── the budget cap is the recipe's ──────────────────────────────────

#[test]
fn a_run_may_not_exceed_its_recipes_budget() {
    let (_root, store) = sample_store("budget");
    let declared = recipe(
        "soft-distill@1",
        "task-w15",
        vec![SignalKind::TeacherLabel],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();

    // A run inside the cap is accepted.
    let ok = run_on("soft-distill@1", "task-w15", 512);
    store
        .check_run_against_recipe(&ok)
        .expect("512 is exactly the cap");

    // A run that asks for more than the recipe declares is refused —
    // whatever the run itself claims its budget is.
    let greedy = run_on("soft-distill@1", "task-w15", 4096);
    let err = store.check_run_against_recipe(&greedy).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted, "{err}");
    assert!(err.context.contains("512"), "{err}");
}

#[test]
fn a_run_on_another_tasks_recipe_is_refused() {
    let (_root, store) = sample_store("task");
    let declared = recipe(
        "preference@1",
        "task-w15",
        vec![SignalKind::HumanPreference],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();
    let run = run_on("preference@1", "task-other", 512);
    let err = store.check_run_against_recipe(&run).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation, "{err}");
}

#[test]
fn the_spent_ledger_still_refuses_to_go_past_the_budget() {
    // The run-level ledger is independent of the recipe gate, and both
    // have to hold: the recipe caps the ask, the ledger caps the spend.
    let (_root, store) = sample_store("ledger");
    let declared = recipe(
        "soft-distill@1",
        "task-w15",
        vec![SignalKind::TeacherLabel],
        512,
    );
    store
        .put_recipe(&actor(ActorRole::Operator), &declared)
        .unwrap();
    let mut run = run_on("soft-distill@1", "task-w15", 512);
    store.put_run(&actor(ActorRole::Operator), &run).unwrap();

    assert_eq!(store.consume_steps(&run.id, 300).unwrap(), 300);
    run.steps_consumed = 300;
    store.set_run_state(&run).unwrap();

    let err = store.consume_steps(&run.id, 300).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted, "{err}");
}

// ── the zio declaration and the rust record agree ───────────────────

/// Load `libs/learning/learn/recipes.zio` into a real zio context.
fn recipes_ctx() -> zio_core::context::EvalContext {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf();
    let env = Arc::new(zio_core::env::Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = zio_core::context::EvalContext::new(env);
    let source_id = ctx
        .source_map()
        .register("core.zio".into(), zio_core::stdlib_source().to_string());
    let forms =
        zio_core::reader::reader::read_program_with_source(zio_core::stdlib_source(), source_id)
            .expect("stdlib parses");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, &ctx).expect("stdlib loads");
    }
    let lib = root.join("libs/learning/learn/recipes.zio");
    let source = std::fs::read_to_string(&lib).expect("recipes.zio exists");
    let sid = ctx
        .source_map()
        .register("learn/recipes.zio".into(), source.clone());
    let forms = zio_core::reader::reader::read_program_with_source(&source, sid)
        .expect("recipes.zio parses");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, &ctx)
            .unwrap_or_else(|e| panic!("loading recipes.zio: {e}"));
    }
    ctx
}

/// Evaluate `src` and return its value rendered as JSON.
///
/// The zio reader pulls `,` into symbols, so a Value's Display is read
/// syntax, not JSON. The fix is the same one the model description uses:
/// bind the VALUE and stringify it, rather than round-tripping a string.
fn eval_json(ctx: &zio_core::context::EvalContext, src: &str) -> serde_json::Value {
    // Evaluate first and bind the RESULT by name. Quoting a symbol here
    // would quote the symbol, not its value.
    let unique = format!(
        "probe{}",
        PROBE_ID.with(|c| {
            let next = c.get();
            c.set(next + 1);
            next
        })
    );
    let bind = format!("(def {unique} {src})");
    let sid = ctx.source_map().register("bind".into(), bind.clone());
    let forms = zio_core::reader::reader::read_program_with_source(&bind, sid).expect("binds");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, ctx).expect("binds");
    }
    let json_src = format!("(json-stringify {unique})");
    let sid = ctx
        .source_map()
        .register("json".into(), json_src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(&json_src, sid).expect("json");
    let mut rendered = zio_core::value::Value::Nil;
    for sexp in forms {
        rendered = zio_core::eval::eval_in_context(&sexp, ctx).expect("json-stringify");
    }
    match rendered {
        zio_core::value::Value::String(s) => {
            serde_json::from_str(&s).expect("zio renders strict JSON")
        }
        other => panic!("expected a JSON string, got {other:?}"),
    }
}

thread_local! {
    /// Each bind needs a fresh name: the context is shared across the
    /// cases in one test, and redefining a probe would clobber it.
    static PROBE_ID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Evaluate `src` and return its value's Display, for scalar assertions.
fn eval_str(ctx: &zio_core::context::EvalContext, src: &str) -> String {
    let source_id = ctx
        .source_map()
        .register("recipe-test".into(), src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(src, source_id)
        .expect("test source parses");
    let mut last = zio_core::value::Value::Nil;
    for sexp in forms {
        last = zio_core::eval::eval_in_context(&sexp, ctx).expect("evaluates");
    }
    format!("{last}")
}

#[test]
fn the_zio_declaration_matches_the_stored_recipe_record() {
    // The declaration the product ships and the record the host stores
    // must describe the same recipe. If they drift, the host enforces a
    // policy nobody published.
    let ctx = recipes_ctx();
    let declared = eval_json(&ctx, "recipes--preference");
    let consumes: Vec<String> =
        serde_json::from_value(declared["consumes"].clone()).expect("consumes is a list");
    let budget: u32 = declared["budget-steps"]
        .as_u64()
        .expect("budget-steps is a number") as u32;

    let (_root, store) = sample_store("cross");
    let kinds: Vec<SignalKind> = consumes
        .iter()
        .map(|name| match name.as_str() {
            "teacher-label" => SignalKind::TeacherLabel,
            "human-correction" => SignalKind::HumanCorrection,
            "human-preference" => SignalKind::HumanPreference,
            "demonstration" => SignalKind::Demonstration,
            "environment-result" => SignalKind::EnvironmentResult,
            other => panic!("unknown signal kind in the declaration: {other}"),
        })
        .collect();
    let stored = recipe("preference@1", "task-w15", kinds, budget);
    store
        .put_recipe(&actor(ActorRole::Operator), &stored)
        .unwrap();

    // A run built from the published budget is accepted, and one that
    // asks for a step more is not: the zio cap is the host's cap.
    store
        .check_run_against_recipe(&run_on("preference@1", "task-w15", budget))
        .expect("the declared budget is accepted");
    let over = run_on("preference@1", "task-w15", budget + 1);
    assert_eq!(
        store.check_run_against_recipe(&over).unwrap_err().kind,
        ErrorKind::BudgetExhausted
    );
}

#[test]
fn the_zio_registry_holds_every_extended_recipe() {
    let ctx = recipes_ctx();
    // `recipes--load-registry` is a map keyed by recipe id, which is what
    // the driver dispatches on.
    let registry = eval_json(&ctx, "(recipes--load-registry)");
    for expected in [
        "supervised@1",
        "hard-distill@1",
        "soft-distill@1",
        "preference@1",
        "demonstration@1",
        "self-supervised@1",
        "environment-feedback@1",
        "policy-gradient@1",
    ] {
        assert!(
            registry.get(expected).is_some(),
            "recipe {expected} is not in the registry: {:?}",
            registry
                .as_object()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        );
    }
    // The extended recipes carry the contract fields the host needs; a
    // soft-distillation recipe with no label space, or a policy recipe
    // with no action set, is not a usable declaration.
    let soft = eval_json(&ctx, "recipes--soft-distill");
    assert!(
        soft["teacher-vocab"].is_array(),
        "soft-distill must declare its label space"
    );
    let policy = eval_json(&ctx, "recipes--policy-gradient");
    assert!(
        policy["actions"].is_array(),
        "policy-gradient must declare its actions"
    );
    assert!(
        policy["reward"].is_string(),
        "policy-gradient must declare its reward"
    );
    assert!(
        policy["termination"].is_string(),
        "policy-gradient must declare its termination"
    );
    let demo = eval_json(&ctx, "recipes--demonstration");
    assert!(
        demo["legal-actions"].is_array(),
        "demonstration must declare its legal actions"
    );
    let selfsup = eval_json(&ctx, "recipes--self-supervised");
    assert!(
        selfsup["task-gate"].is_string(),
        "self-supervision must name the task gate"
    );
}

#[test]
fn the_zio_refusals_are_refusals() {
    // The Zio side refuses a signal kind a recipe does not consume, a
    // teacher on another label space, an abstention used as a loss, an
    // action outside the legal space, and an unattributable delayed
    // result. These are the same rules the host and the worker enforce.
    let ctx = recipes_ctx();
    let cases: Vec<(&str, &str)> = vec![
        (
            r#"(recipes--check-signal recipes--preference :human-preference)"#,
            "true",
        ),
        (
            r#"(recipes--check-signal recipes--preference :environment-result)"#,
            "false",
        ),
        (
            r#"(recipes--vocab-matches? recipes--soft-distill ["clear" "fault"] ["clear" "fault"])"#,
            "true",
        ),
        (
            r#"(recipes--vocab-matches? recipes--soft-distill ["fault" "clear"] ["clear" "fault"])"#,
            "false",
        ),
        (
            r#"(recipes--legal-action? recipes--demonstration "clear")"#,
            "true",
        ),
        (
            r#"(recipes--legal-action? recipes--demonstration "teleport")"#,
            "false",
        ),
        (
            r#"(recipes--preference-usable? recipes--preference {:abstain false} {:abstain false})"#,
            "true",
        ),
        (
            r#"(recipes--preference-usable? recipes--preference {:abstain true} {:abstain false})"#,
            "false",
        ),
        (
            r#"(recipes--delayed-binds-trajectory? recipes--environment-feedback {:trajectory-id "traj-1"})"#,
            "true",
        ),
        (
            r#"(recipes--delayed-binds-trajectory? recipes--environment-feedback {:trajectory-id ""})"#,
            "false",
        ),
        (
            r#"(recipes--action-set-declared? recipes--policy-gradient)"#,
            "true",
        ),
        (r#"(recipes--action-set-declared? {:id "x"})"#, "false"),
        (
            r#"(recipes--budget-for recipes--soft-distill 99999)"#,
            "512",
        ),
        (r#"(recipes--budget-for recipes--policy-gradient 10)"#, "10"),
    ];
    for (src, expected) in cases {
        let got = eval_str(&ctx, src);
        assert_eq!(
            got.trim(),
            expected,
            "{src} should evaluate to {expected}, got {got}"
        );
    }
}

#[test]
fn the_pre_existing_recipe_functions_still_work() {
    // `recipes--supervised`, `check-signal`, `budget-for` and
    // `trainable?` are the W05 surface; W15 is additive and must not have
    // moved them.
    let ctx = recipes_ctx();
    for (src, expected) in [
        (
            r#"(recipes--check-signal recipes--supervised :teacher-label)"#,
            "true",
        ),
        (
            r#"(recipes--check-signal recipes--supervised :human-correction)"#,
            "true",
        ),
        (
            r#"(recipes--check-signal recipes--supervised :environment-result)"#,
            "false",
        ),
        (r#"(recipes--budget-for recipes--supervised 99999)"#, "512"),
        (r#"(recipes--budget-for recipes--supervised 10)"#, "10"),
        (r#"(recipes--trainable? recipes--supervised "h0")"#, "true"),
        (r#"(recipes--trainable? {:trainable ["h0"]} "h1")"#, "false"),
    ] {
        let got = eval_str(&ctx, src);
        assert_eq!(got.trim(), expected, "{src} changed: {got}");
    }
}
