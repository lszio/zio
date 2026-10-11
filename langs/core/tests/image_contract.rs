//! M00 image graph contracts: identity, shared cells and reference
//! boundaries. These tests pin the encodable runtime graph — the format
//! premise for M01 save/restore — not a recursive JSON value copy.

use std::sync::Arc;

use zio_core::bootstrap::{self, ModuleRoots};
use zio_core::bytecode;
use zio_core::context::EvalContext;
use zio_core::image::{self, ImageLimits};
use zio_core::value::Value;

fn context() -> EvalContext {
    bootstrap::language_context(ModuleRoots::empty()).expect("bootstrap")
}

fn encode(roots: &[Value]) -> zio_core::image::wire::Image {
    image::encode_graph(roots, &ImageLimits::default()).expect("encode")
}

// ── Savability rules ─────────────────────────────────────────────

#[test]
fn pure_values_encode_without_reference_nodes() {
    let image = encode(&[
        Value::Integer(1),
        Value::String("a".into()),
        Value::Boolean(true),
    ]);
    assert_eq!(image.header.format, "zio-image");
    assert_eq!(image.roots.len(), 3);
    // Scalars are self-contained: no env/function/resource records.
    assert!(
        image
            .nodes
            .iter()
            .all(|n| matches!(n.kind.as_str(), "integer" | "string" | "boolean"))
    );
    assert!(image.resources.is_empty());
}

#[test]
fn native_bindings_record_registration_never_counter_ids() {
    let native = zio_core::value::NativeFn::new("test-binding", |_args, _| Ok(Value::Nil));
    let image = encode(&[Value::NativeFunction(native)]);
    let node = image
        .nodes
        .iter()
        .find(|n| n.kind == "native")
        .expect("native node");
    let name = node.payload["name"].as_str().expect("name");
    assert_eq!(name, "test-binding");
    assert_eq!(
        image.resources,
        vec![zio_core::image::wire::ResourceRequirement {
            kind: "native-binding".into(),
            name: "test-binding".into(),
            path: "roots[0]".into(),
        }]
    );
    // The process-lifetime counter id must not leak into the format.
    let canonical = image.canonical().expect("canonical");
    assert!(
        !canonical.contains("\"id\":1"),
        "counter id leaked: {canonical}"
    );
}

#[test]
fn active_futures_and_channels_are_rejected_by_name() {
    let future = Value::Future(Arc::default());
    let error = image::encode_graph(&[future], &ImageLimits::default()).unwrap_err();
    assert!(error.to_string().contains("future"), "{error}");
    let (_, rx) = std::sync::mpsc::channel::<Value>();
    let channel = zio_core::value::ChannelPair {
        tx: parking_lot::Mutex::new(zio_core::value::ChannelTx::Async(
            std::sync::mpsc::channel().0,
        )),
        rx: parking_lot::Mutex::new(rx),
    };
    let error = image::encode_graph(
        &[Value::Channel(Arc::new(channel))],
        &ImageLimits::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("channel"), "{error}");
}

// ── Identity and sharing ─────────────────────────────────────────

#[test]
fn one_environment_referenced_twice_is_one_node() {
    // (fn (make-adder 10)) evaluated twice shares the adder environment.
    let ctx = context();
    let adder = bootstrap::eval_source(&ctx, "adder.zio", "((fn [k] (fn [x] (+ x k))) 10)")
        .expect("adder closure");
    let Value::Function(inner) = adder else {
        panic!("expected closure");
    };
    let outer = inner.env.clone();
    // Two references to the same closure plus one distinct closure with
    // the same environment.
    let image = encode(&[
        Value::Function(inner.clone()),
        Value::Function(inner),
        Value::Function(Arc::new(zio_core::value::Function {
            params: zio_core::im::Vector::new(),
            rest_param: None,
            body: zio_core::sexp::Sexp::Nil,
            env: outer.clone(),
            compiled: None,
        })),
    ]);
    let env_nodes: Vec<_> = image.nodes.iter().filter(|n| n.kind == "env").collect();
    let function_nodes: Vec<_> = image
        .nodes
        .iter()
        .filter(|n| n.kind == "function")
        .collect();
    assert_eq!(env_nodes.len(), 1, "shared env must intern to one node");
    assert_eq!(function_nodes.len(), 2, "the cloned closure interns once");
    let env_id = env_nodes[0].id;
    for function in &function_nodes {
        assert_eq!(function.payload["env"], ref_id(env_id));
    }
}

#[test]
fn distinct_environments_with_equal_contents_are_not_merged() {
    let ctx = context();
    let first = bootstrap::eval_source(&ctx, "a.zio", "((fn [k] (fn [x] (+ x k))) 10)").unwrap();
    let second = bootstrap::eval_source(&ctx, "b.zio", "((fn [k] (fn [x] (+ x k))) 10)").unwrap();
    let image = encode(&[first, second]);
    assert_eq!(
        image.nodes.iter().filter(|n| n.kind == "env").count(),
        2,
        "equal envs are distinct identity nodes"
    );
}

#[test]
fn shared_mutable_buffer_is_one_cell() {
    let buffer = Value::Buffer(Arc::new(parking_lot::Mutex::new(b"abc".to_vec())));
    let image = encode(&[buffer.clone(), buffer]);
    let buffers: Vec<_> = image.nodes.iter().filter(|n| n.kind == "buffer").collect();
    assert_eq!(buffers.len(), 1, "buffer identity is the shared cell");
    assert_eq!(buffers[0].payload["bytes"], serde_json::json!([97, 98, 99]));
}

#[test]
fn cyclic_environment_terminates_with_mutual_refs() {
    // A let-bound f mutated to a fn that reads f: env → fn → env.
    let ctx = context();
    let cyclic = bootstrap::eval_source(
        &ctx,
        "cycle.zio",
        "((fn [] (let [f nil] (do (set! f (fn [] f)) f))))",
    )
    .expect("cyclic closure");
    let image = encode(&[cyclic]);
    let envs: Vec<_> = image.nodes.iter().filter(|n| n.kind == "env").collect();
    let fns: Vec<_> = image
        .nodes
        .iter()
        .filter(|n| n.kind == "function")
        .collect();
    assert_eq!(envs.len(), 1);
    assert_eq!(fns.len(), 1);
    assert_eq!(envs[0].payload["bindings"]["f"], ref_id(fns[0].id));
    assert_eq!(fns[0].payload["env"], ref_id(envs[0].id));
}

#[test]
fn zos_identity_shares_but_anonymous_instances_clone() {
    let class = Arc::new(zio_core::zos::object::Class {
        name: "Box".into(),
        superclasses: Vec::new(),
        slots: Vec::new(),
        cpl: vec!["Box".into()],
    });
    let mut shared = zio_core::value::ZosInstance::new(class.clone());
    shared.header.identity = Some(7);
    let mut twin = zio_core::value::ZosInstance::new(class.clone());
    twin.header.identity = Some(7);
    let anonymous = zio_core::value::ZosInstance::new(class);
    let image = encode(&[
        Value::Object(Box::new(shared)),
        Value::Object(Box::new(twin)),
        Value::Object(Box::new(anonymous.clone())),
        Value::Object(Box::new(anonymous)),
    ]);
    let objects: Vec<_> = image
        .nodes
        .iter()
        .filter(|n| n.kind == "instance")
        .collect();
    assert_eq!(objects.len(), 3, "identity 7 interns; anonymous clones");
}

#[test]
fn compiled_functions_reference_their_module_once() {
    let ctx = context();
    let module = bytecode::compile_source(&ctx, "mod.zio", "(defn f [x] (* x x)) (f 3)").unwrap();
    let result = bytecode::execute_module(&ctx, &module).expect("run");
    let _ = result;
    // Take the compiled function value out of the module's global install.
    let function = ctx.env.get("f").expect("compiled f");
    let Value::Function(function) = function else {
        panic!("expected compiled function");
    };
    assert!(function.compiled.is_some());
    // The same compiled closure twice: one function node, one module node.
    let image = encode(&[Value::Function(function.clone()), Value::Function(function)]);
    let modules: Vec<_> = image.nodes.iter().filter(|n| n.kind == "module").collect();
    let fns: Vec<_> = image
        .nodes
        .iter()
        .filter(|n| n.kind == "function")
        .collect();
    assert_eq!(
        modules.len(),
        1,
        "module artifact is referenced, not copied"
    );
    assert_eq!(fns.len(), 1, "the closure interns to one node");
    assert_eq!(fns[0].payload["module"], ref_id(modules[0].id));
    assert_eq!(fns[0].payload["function"], "f");
}

// ── Determinism and limits ───────────────────────────────────────

#[test]
fn encoding_is_deterministic_across_runs() {
    let ctx = context();
    let value = bootstrap::eval_source(
        &ctx,
        "det.zio",
        "{:b [1 \"x\" {:z true}] :a (list 2 1) :c ((fn [k] (fn [x] (+ x k))) 4)}",
    )
    .unwrap();
    let first = encode(std::slice::from_ref(&value)).canonical().unwrap();
    let second = encode(std::slice::from_ref(&value)).canonical().unwrap();
    assert_eq!(first, second);
}

#[test]
fn limits_reject_unbounded_graphs_with_named_errors() {
    // Pure nesting is bounded by an explicit depth budget: recursion must
    // stop before the test stack is at risk (deep Values already overflow
    // plain drop, the known quoted-list runtime limit).
    let deep: Value = (0..300).fold(Value::Nil, |acc, _| Value::List(vec![acc].into()));
    let budget = ImageLimits {
        max_depth: 8,
        ..ImageLimits::default()
    };
    let error = image::encode_graph(&[deep], &budget).unwrap_err();
    assert!(error.to_string().contains("nesting limit"), "{error}");

    // Node budget trips without deep recursion: many sibling roots.
    let roots: Vec<Value> = (0..64).map(|i| Value::Integer(i)).collect();
    let tight = ImageLimits {
        max_nodes: 16,
        ..ImageLimits::default()
    };
    let error = image::encode_graph(&roots, &tight).unwrap_err();
    assert!(error.to_string().contains("node limit"), "{error}");
}

// ── Wire validation ──────────────────────────────────────────────

#[test]
fn wire_rejects_unknown_kind_orphan_ref_duplicate_id_and_mismatch() {
    let valid = encode(&[Value::Integer(1)]).canonical().unwrap();

    let unknown = valid.replace("\"kind\":\"integer\"", "\"kind\":\"blob\"");
    let error = image::wire::Image::parse(&unknown).unwrap_err();
    assert!(error.to_string().contains("unknown node kind"), "{error}");

    let mut image = encode(&[Value::List(vec![Value::Integer(1)].into())]);
    // Point the list's element at a missing node.
    let list_id = image.nodes.iter().find(|n| n.kind == "list").unwrap().id;
    image.nodes[list_id].payload["items"] = serde_json::json!([{ "$ref": 999 }]);
    let error = image.validate(&ImageLimits::default()).unwrap_err();
    assert!(error.to_string().contains("reference"), "{error}");

    let mut bad = encode(&[Value::Integer(1)]);
    bad.nodes.insert(
        0,
        zio_core::image::wire::Node {
            id: 0,
            kind: "integer".into(),
            payload: serde_json::json!({ "value": 9 }),
        },
    );
    let error = bad.validate(&ImageLimits::default()).unwrap_err();
    assert!(error.to_string().contains("duplicate node id"), "{error}");

    // A scalar kind carrying a reference payload is a named mismatch.
    let mut mismatched = encode(&[Value::Integer(1)]);
    mismatched.nodes[0].kind = "string".into();
    let error = mismatched.validate(&ImageLimits::default()).unwrap_err();
    assert!(error.to_string().contains("payload"), "{error}");
}

#[test]
fn wire_rejects_wrong_format_and_version() {
    let valid = encode(&[Value::Nil]).canonical().unwrap();
    let wrong = valid.replace("\"format\":\"zio-image\"", "\"format\":\"zio-blob\"");
    assert!(image::wire::Image::parse(&wrong).is_err());
    let old = valid.replace("\"format_version\":1", "\"format_version\":99");
    assert!(image::wire::Image::parse(&old).is_err());
}

fn ref_id(id: usize) -> serde_json::Value {
    serde_json::json!({ "$ref": id })
}

// ── M01: still-image save/restore ────────────────────────────────

use zio_core::image::{restore, save};

fn map_entry(value: &Value, key: &str) -> Value {
    let Value::Map(entries) = value else {
        panic!("expected a map of roots");
    };
    entries
        .get(&Value::Keyword(key.into()))
        .cloned()
        .unwrap_or_else(|| panic!("missing root :{key}"))
}

fn bind(ctx: &EvalContext, name: &str, value: Value) {
    ctx.env.set(name.into(), value);
}

#[test]
fn save_restore_round_trips_shared_mutable_binding() {
    let ctx1 = context();
    let roots = bootstrap::eval_source(
        &ctx1,
        "shared.zio",
        "(let [n 0] {:get (fn [] n) :bump (fn [] (set! n (+ n 1)) n)})",
    )
    .expect("closures");
    let image = save::save_image(&ctx1, &[roots], &ImageLimits::default()).expect("save");
    let ctx2 = context();
    let restored = restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    let Value::Map(entries) = &restored[0] else {
        panic!("expected the root map");
    };
    let get = entries.get(&Value::Keyword("get".into())).unwrap().clone();
    let bump = entries.get(&Value::Keyword("bump".into())).unwrap().clone();
    bind(&ctx2, "get", get);
    bind(&ctx2, "bump", bump);
    assert_eq!(
        bootstrap::eval_source(&ctx2, "bump.zio", "(bump)").unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        bootstrap::eval_source(&ctx2, "get.zio", "(get)").unwrap(),
        Value::Integer(1),
        "setter and getter must share the restored binding cell"
    );
}

#[test]
fn self_referential_environment_survives_the_round_trip() {
    let ctx1 = context();
    let spin = bootstrap::eval_source(
        &ctx1,
        "spin.zio",
        "((fn [] (let [f nil] (do (set! f (fn [] f)) f))))",
    )
    .expect("cyclic closure");
    let image = save::save_image(&ctx1, &[spin], &ImageLimits::default()).expect("save");
    let ctx2 = context();
    let restored = restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    let Value::Function(function) = &restored[0] else {
        panic!("expected the closure");
    };
    // The env's `f` binding is the very same function object.
    assert_eq!(function.env.get("f"), Some(restored[0].clone()));
    bind(&ctx2, "spin", restored[0].clone());
    assert_eq!(
        bootstrap::eval_source(&ctx2, "call-spin.zio", "(spin)").unwrap(),
        restored[0],
        "calling the restored closure returns the same object"
    );
}

#[test]
fn restored_macro_expands_in_the_new_context() {
    let ctx1 = context();
    bootstrap::eval_source(&ctx1, "macro.zio", "(defmacro twice [e] `(+ ~e ~e))").expect("macro");
    let macro_value = ctx1.env.get("twice").expect("twice");
    let image = save::save_image(&ctx1, &[macro_value], &ImageLimits::default()).expect("save");
    let ctx2 = context();
    let restored = restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    bind(&ctx2, "twice", restored[0].clone());
    let expanded = bootstrap::eval_source(&ctx2, "expand.zio", "(macroexpand-1 '(twice 3))")
        .expect("macroexpand");
    assert_eq!(
        expanded,
        Value::List(
            vec![
                Value::Symbol("+".into()),
                Value::Integer(3),
                Value::Integer(3),
            ]
            .into()
        )
    );
}

#[test]
fn module_exports_survive_the_round_trip() {
    let ctx1 = context();
    bootstrap::eval_source(
        &ctx1,
        "mod.zio",
        "(module session :export [answer] (def answer 42))",
    )
    .expect("module");
    let image = save::save_image(&ctx1, &[], &ImageLimits::default()).expect("save");
    assert_eq!(image.modules.len(), 1, "module table travels");
    assert_eq!(image.modules[0].exports, vec!["answer".to_string()]);
    let ctx2 = context();
    restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    let exported = ctx2
        .modules
        .borrow()
        .find(&["session".to_string()])
        .and_then(|module| module.exported("answer"));
    assert_eq!(
        exported,
        Some(Value::Integer(42)),
        "module exports resolve from the restored environment"
    );
}

#[test]
fn class_dispatch_works_after_restore() {
    let ctx1 = context();
    let roots = bootstrap::eval_source(
        &ctx1,
        "class.zio",
        "(do
            (defclass Widget () ((label :initarg :label)))
            (defgeneric describe-widget [w])
            (defmethod describe-widget ((w Widget)) (slot-value w :label))
            (def box (make-instance Widget :label \"kept\"))
            {:box box :describe describe-widget})",
    )
    .expect("classes");
    let image = save::save_image(&ctx1, &[roots], &ImageLimits::default()).expect("save");
    let ctx2 = context();
    let restored = restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    bind(&ctx2, "box", map_entry(&restored[0], "box"));
    bind(
        &ctx2,
        "describe-widget",
        map_entry(&restored[0], "describe"),
    );
    let dispatched =
        bootstrap::eval_source(&ctx2, "dispatch.zio", "(describe-widget box)").expect("dispatch");
    assert_eq!(dispatched, Value::String("kept".into()));
}

#[test]
fn restored_buffer_is_one_shared_cell() {
    let ctx1 = context();
    let buffer = Value::Buffer(Arc::new(parking_lot::Mutex::new(b"abc".to_vec())));
    let image =
        save::save_image(&ctx1, &[buffer.clone(), buffer], &ImageLimits::default()).expect("save");
    let ctx2 = context();
    let restored = restore::restore_image(&ctx2, &image, &ImageLimits::default()).expect("restore");
    assert_eq!(restored[0], restored[1], "one cell, not two copies");
    let Value::Buffer(cell) = &restored[0] else {
        panic!("expected the buffer");
    };
    cell.lock().push(b'd');
    let Value::Buffer(same) = &restored[1] else {
        panic!("expected the buffer");
    };
    assert_eq!(&*same.lock(), b"abcd", "mutation is visible through both");
}

#[test]
fn missing_native_binding_and_version_mismatch_are_named_errors() {
    let ctx1 = context();
    let native = zio_core::value::NativeFn::new("only-in-one", |_args, _| Ok(Value::Nil));
    let image = save::save_image(
        &ctx1,
        &[Value::NativeFunction(native)],
        &ImageLimits::default(),
    )
    .expect("save");
    let ctx2 = context();
    let error = restore::restore_image(&ctx2, &image, &ImageLimits::default()).unwrap_err();
    assert!(
        error.to_string().contains("missing native binding")
            && error.to_string().contains("only-in-one"),
        "{error}"
    );

    let mut stale = image;
    stale.header.runtime = "0.0.0".into();
    let error = restore::restore_image(&ctx2, &stale, &ImageLimits::default()).unwrap_err();
    assert!(
        error.to_string().contains("runtime version mismatch"),
        "{error}"
    );
}

#[test]
fn image_session_example_round_trips_end_to_end() {
    let ctx1 = context();
    let example = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/toolchain/image-session.zio"
    );
    let source = std::fs::read_to_string(example).expect("example source");
    let roots = bootstrap::eval_source(&ctx1, "image-session.zio", &source).expect("example");
    let macro_value = ctx1.env.get("twice").expect("twice macro");
    let image = save::save_image(
        &ctx1,
        &[roots.clone(), macro_value],
        &ImageLimits::default(),
    )
    .expect("save");
    let canonical = image.canonical().expect("canonical");
    let reparsed = image::wire::Image::parse(&canonical).expect("reparse");

    let ctx2 = context();
    let restored =
        restore::restore_image(&ctx2, &reparsed, &ImageLimits::default()).expect("restore");
    bind(&ctx2, "get", map_entry(&restored[0], "get"));
    bind(&ctx2, "bump", map_entry(&restored[0], "bump"));
    bind(&ctx2, "spin", map_entry(&restored[0], "spin"));
    bind(&ctx2, "twice", restored[1].clone());
    bind(&ctx2, "box", map_entry(&restored[0], "box"));
    bind(&ctx2, "describe", map_entry(&restored[0], "describe"));
    bind(&ctx2, "one", map_entry(&restored[0], "one"));
    bind(&ctx2, "two", map_entry(&restored[0], "two"));

    assert_eq!(
        bootstrap::eval_source(&ctx2, "s1.zio", "(do (bump) (bump) (get))").unwrap(),
        Value::Integer(2)
    );
    assert_eq!(
        bootstrap::eval_source(&ctx2, "s2.zio", "(spin)").unwrap(),
        map_entry(&restored[0], "spin")
    );
    assert_eq!(
        bootstrap::eval_source(&ctx2, "s3.zio", "(macroexpand-1 '(twice 3))").unwrap(),
        Value::List(
            vec![
                Value::Symbol("+".into()),
                Value::Integer(3),
                Value::Integer(3),
            ]
            .into()
        )
    );
    assert_eq!(
        bootstrap::eval_source(&ctx2, "s4.zio", "(describe box)").unwrap(),
        Value::String("kept".into())
    );
    let Value::Buffer(cell) = map_entry(&restored[0], "one") else {
        panic!("expected the buffer");
    };
    cell.lock().push(b'!');
    let Value::Buffer(same) = map_entry(&restored[0], "two") else {
        panic!("expected the shared buffer");
    };
    assert_eq!(&*same.lock(), b"abc!");
}

#[test]
fn save_is_deterministic_across_runs() {
    let ctx1 = context();
    let roots = bootstrap::eval_source(
        &ctx1,
        "det.zio",
        "{:b ((fn [k] (fn [x] (+ x k))) 4) :a (list 2 1)}",
    )
    .expect("roots");
    let first = save::save_image(&ctx1, std::slice::from_ref(&roots), &ImageLimits::default())
        .expect("save")
        .canonical()
        .expect("canonical");
    let second = save::save_image(&ctx1, &[roots], &ImageLimits::default())
        .expect("save")
        .canonical()
        .expect("canonical");
    assert_eq!(first, second);
}
