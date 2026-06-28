# Zio

> A Modern Lisp for the Agent Era.

Zio 是一门面向未来的通用 Lisp 语言，用 Rust 实现。当前处于 **v0.2 架构重组完成阶段** —— 所有全局状态已显式化，核心可嵌入、可测试。ZOS（Zio Object System）规范已发布，进入实现阶段。

| **指标** | **值** |
|----------|--------|
| 测试 | 103 passing, 0 warnings |
| 代码 | ~3,500 LOC Rust (core) + 规范 |
| 线程局部全局变量 | **0**（已全部移除） |
| Crates | `zio-core`、`zio-reader`、`zio` (CLI) |
| 特殊形式 | 12 种 |
| 内置函数 | 30 个 |
| 设计原则 | 最小、正交、可扩展、运行时优先、机制而非策略 |

## 哲学

Zio 是一门以同像性（homoiconicity）为基石的 Lisp 语言。代码即数据，数据即代码。
宏系统让用户拥有与语言实现者相同的扩展能力。

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + 统一运行时对象模型（ZOS：AMOP + MOP）
    + 扩展库生态（Datalog · Agent · 自学习）
```

核心原则：**核心最小，其余是库**。Datalog、Agent、自学习模型框架都是通过宏 + MOP 构建的 `.zio` 扩展库，不进入核心。

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

## 文档

| 文档 | 说明 |
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
| **Phase 5** — ZOS Phase 3 + 生态 | 2-3 周 | 官方扩展库（persistent、entity、protocol） |
| **Phase 6** — 高级生态 | 3-4 周 | zio-datalog, zio-agent, zio-ai, Baseline JIT |
| **Phase 7** — 生产化 | 持续 | LSP, Debugger, WASM, Profiler, 自举编辑器 |

详见 [docs/roadmap.md](docs/roadmap.md)。

## License

MIT
