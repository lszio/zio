//! Grove's Zio selection policy: quality/cost trade-offs never buy back a failed gate.

use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::value::Value;

#[test]
fn selection_keeps_quality_cost_alternatives_without_gate_failed_dominators() {
    let ctx = language_context(ModuleRoots::empty()).unwrap();
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../selection.zio"),
    )
    .expect("Grove owns its selection strategy in Zio");
    eval_source(&ctx, "apps/grove/selection.zio", &source).unwrap();
    let result = eval_source(
        &ctx,
        "selection-contract",
        r#"
        (grove-non-dominated
          [{:index 0 :quality 0.95 :cost 10.0 :meets-gates true}
           {:index 1 :quality 0.91 :cost 1.0 :meets-gates true}
           {:index 2 :quality 0.70 :cost 0.5 :meets-gates false}
           {:index 3 :quality 0.99 :cost 0.1 :meets-gates false}
           {:index 4 :quality 0.80 :cost 20.0 :meets-gates true}])
    "#,
    )
    .unwrap();
    let ids = match result {
        Value::List(ids) | Value::Vector(ids) => ids,
        other => panic!("selection returned {other}, not row indices"),
    };
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        vec![Value::Integer(0), Value::Integer(1)]
    );
    for input in [
        "[]",
        "[{:index 0 :quality 1.0 :cost 0.0 :meets-gates false}]",
    ] {
        let result = eval_source(
            &ctx,
            "selection-empty",
            &format!("(grove-non-dominated {input})"),
        )
        .unwrap();
        match result {
            Value::List(ids) | Value::Vector(ids) => assert!(ids.is_empty()),
            other => panic!("selection returned {other}, not row indices"),
        }
    }
}

#[test]
fn native_rows_use_accuracy_and_cannot_rank_a_gate_failed_snapshot() {
    use grove::contracts::ArtifactRef;
    use grove::evaluation::Comparison;

    let row = |id, mean, meets_gates| Comparison {
        snapshot: ArtifactRef { digest: [id; 32] },
        repeats: 1,
        mean,
        meets_gates,
        gate_failures: Vec::new(),
    };
    let rows = [
        row(0, vec![("accuracy".into(), 1.0)], false),
        row(
            1,
            vec![("precision".into(), 100.0), ("accuracy".into(), 0.91)],
            true,
        ),
        row(
            3,
            vec![("precision".into(), 0.0), ("accuracy".into(), 0.95)],
            true,
        ),
        row(4, vec![("precision".into(), 1000.0)], true),
    ];
    let selected = grove_app::select_candidates(&rows).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|r| r.snapshot.digest[0])
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(grove_app::select_candidates(&[]).unwrap().is_empty());
    assert!(grove_app::select_candidates(&rows[..1]).unwrap().is_empty());
}
