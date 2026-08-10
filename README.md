# Zio

> A Modern Lisp for the Agent Era.

Zio 是一门面向未来的通用 Lisp 语言，用 Rust 实现。当前 workspace
包含两个 crate：`zio-core` 和 `zio-cli`。实时生成的测试、语法和内置
绑定数量见 [项目状态](docs/status.md)；能力成熟度以
[特性矩阵](docs/feature-matrix.md)为准。

仓库数量不在 README 中手工维护；运行 `tools/project-status.sh` 可重新生成
[项目状态](docs/status.md)。

## 哲学

Zio 是一门以同像性（homoiconicity）为基石的 Lisp 语言。代码即数据，数据即代码。
宏系统让用户拥有与语言实现者相同的扩展能力。

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + 统一运行时对象模型（ZOS：AMOP + MOP）
    + 规划中的扩展库生态（Datalog · Agent · 自学习）
```

核心原则：**核心最小，其余是库**。Datalog、Agent、自学习模型框架的
扩展库边界已经设计，但这些能力仍是
[Planned](docs/feature-matrix.md)，不属于当前已实现核心。

详细哲学：[docs/zio-philosophy.md](docs/zio-philosophy.md)

## 快速开始

```bash
cd zio
cargo run
```

```
Zio REPL
Press Ctrl+D or type (exit) to quit
zio> (+ 1 2 3)
6
zio> (defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
#<function (n)>
zio> (fib 10)
55
zio> (defmacro unless [test body] (list 'if test nil body))
#<macro unless (test body)>
zio> (unless false 42)
42
zio> (load "program.zio")
zio> (require :my.module)
```

## 示例

[`examples/manifest.tsv`](examples/manifest.tsv) 中列出的所有文件都可由
CLI 运行，并受可执行示例合同测试保护。`datalog-concept.zio` 只演示查询
数据是可读取的 Zio 值，并不执行 Datalog 查询；Datalog evaluator 仍是
[Planned](docs/feature-matrix.md)。当前不支持 `#{...}` set literal，限制及
状态也记录在[特性矩阵](docs/feature-matrix.md)。

## 文档

| 文档 | 说明 |
|------|------|
| [docs/status.md](docs/status.md) | 自动生成的 workspace、测试和语言表面数量 |
| [docs/feature-matrix.md](docs/feature-matrix.md) | **权威能力状态：Stable / Experimental / Planned** |
| [docs/zio-philosophy.md](docs/zio-philosophy.md) | 语言哲学、设计定理、设计原则、特性来源 |
| [docs/zos-spec.md](docs/zos-spec.md) | **ZOS 完整规范**（Object/Class/GF/Method/MOP/Condition） |
| [docs/zio-architecture.md](docs/zio-architecture.md) | 系统架构总览、分层、组件状态 |
| [docs/eval-pipeline.md](docs/eval-pipeline.md) | Eval 循环、TCO、宏、编译器管线 |
| [docs/roadmap.md](docs/roadmap.md) | 分 Phase 路线图、交付标准、依赖分析、应用蓝图 |
| [docs/adrs.md](docs/adrs.md) | 架构决策记录（ADR-001 ~ ADR-011） |
| [docs/glossary.md](docs/glossary.md) | 术语参考 |
| [blog/INDEX.md](blog/INDEX.md) | 系列博文 |

## 设计定理

| # | 定理 | 状态 |
|---|------|------|
| 1 | **所有 mutable 状态必须显式** — 无 thread-local 全局变量 | ✅ 已达成 |
| 2 | **Rust 是合同边界，Lisp 是组合层** — 性能关键路径用 NativeFn | ✅ 已达成 |
| 3 | **宏是用户扩展 eval 的方式** — 没有特殊形式不可用宏替代 | ✅ 已达成 |
| 4 | **核心最小，其余是库** — 领域能力不进入 Core | ✅ ZOS 规范中 |
| 5 | **协议比实现重要** — EvalEngine/MOP 是扩展契约 | ✅ 规范中 |

## 路线图一览

| Phase | 时间 | 交付物 |
|-------|------|--------|
| **Phase 1** — 核心稳定化 | 1-2 周 | Span 集成, Reader 扩展, 全 TCO, macroexpand, 200+ tests |
| **Phase 2** — ZOS Phase 1 | 2-3 周 | Class/GF/Method/Package/Condition, `defclass` `defgeneric` `defmethod` |
| **Phase 3** — 卫生宏 + ZOS Phase 2 | 2-3 周 | `syntax-rules` 模式匹配宏, MOP, 多分派, 完整反射 |
| **Phase 4** — 标准库 + 系统编程 | 2-4 周 | FFI, File I/O, 懒序列, Future/Channel, 包管理器, 标准库 |
| **Phase 5** — ZOS Phase 3 + 生态（Planned） | 2-3 周 | 官方扩展库（persistent、entity、protocol） |
| **Phase 6** — 高级生态（Planned） | 3-4 周 | zio-datalog, zio-agent, zio-ai, Baseline JIT |
| **Phase 7** — 生产化 | 持续 | LSP, Debugger, WASM, Profiler, 自举编辑器 |

以上是规划目标，不代表已经交付。详见 [docs/roadmap.md](docs/roadmap.md)，
当前实现状态以[特性矩阵](docs/feature-matrix.md)为准。

## License

MIT
