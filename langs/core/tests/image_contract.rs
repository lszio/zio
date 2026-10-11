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
