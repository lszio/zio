# AST evaluator baseline

`ast-baseline.json` is a reproducible measurement snapshot for the AST
evaluator. Record it from the repository root with:

```bash
tools/record-ast-baseline.sh --output benchmarks/ast-baseline.json
```

The recorder builds `zio-cli` in release mode, runs each fixed workload by
invoking `target/release/zio-cli` directly, and records one elapsed time in
seconds for each workload. It also records the source revision and machine
metadata used for the run.

This artifact is an AST baseline, not a claim about evaluator speed. A valid
comparison must use the same workloads, the same `engine` field, a release
build, and matching machine metadata. If any of those inputs differ, record a
new baseline and describe the difference instead of treating the elapsed values
as directly comparable.
