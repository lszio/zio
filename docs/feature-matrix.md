# Zio Feature Matrix

This is the authoritative implementation-status table for public Zio
documentation. Repository counts are generated in [Project Status](status.md),
and the reproducible AST evaluator measurement is documented in the
[AST Evaluator Baseline](../benchmarks/README.md).

Status meanings:

- **Stable**: covered by the referenced unit or executable-example contract.
- **Experimental**: runnable, but its API and behavior may change.
- **Planned**: represented only by a placeholder, example data, specification,
  or roadmap; it is not an implemented capability.

| Area | Status | Evidence |
| --- | --- | --- |
| Reader and syntax expansion | Stable | reader unit tests |
| AST evaluator and closures | Stable | core evaluator tests |
| Core macros and stdlib | Stable | [`examples/macros.zio` contract](../examples/macros.zio) |
| ZOS classes and generic dispatch subset | Experimental | [`examples/zos-concept.zio` contract](../examples/zos-concept.zio) |
| Persistent collection library | Planned | [`lib/zio/persistent.zio`](../lib/zio/persistent.zio) |
| Datalog evaluator | Planned | [`examples/datalog-concept.zio`](../examples/datalog-concept.zio) |
| ZIR, bytecode VM, and JIT | Planned | [approved VM design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Process capability and pacman updater | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Numeric kernel and scientific API | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Pipeline DSL | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Homoiconic learner | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Landing site | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |

The stable reader row does not include set literals: `#{...}` remains planned
and must not be used in runnable examples.
