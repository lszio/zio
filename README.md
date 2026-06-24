# Zio

> A Modern Lisp for the Agent Era.

Zio 是一门面向未来的通用 Lisp 语言，用 Rust 实现。当前处于 **v0.2 架构重组完成阶段**——所有全局状态已显式化，核心可嵌入、可测试。

| **指标** | **值** |
|----------|-------|
| 测试 | 103 passing, 0 warnings |
| 代码 | ~3,500 LOC Rust |
| 线程局部全局变量 | **0** (已全部移除) |
| Crates | `zio-core`, `zio-reader`, `zio` (CLI) |
| 特殊形式 | 12 种 |
| 内置函数 | 30 个 |

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
|------|------|
| [docs/architecture-handbook.md](docs/architecture-handbook.md) | 完整架构设计、设计定理、ADR、类型系统、两张应用蓝图 |
| [docs/ROADMAP.md](docs/ROADMAP.md) | 分 Phase 路线图、交付标准、工程风险、时间线 |
| [docs/architecture-v0.2.md](docs/architecture-v0.2.md) | 上一版本的架构文档（部分已过时） |

## 设计定理

1. **所有 mutable 状态必须显式** — 无 thread-local 全局变量
2. **Rust 是合同边界，Lisp 是组合层** — 性能关键路径用 NativeFn
3. **宏是用户扩展 eval 的方式** — 同像性使一切可编程
4. **模块系统是代码组织的唯一方式** — 包 + 命名空间

## 路线图一览

| Phase | 时间 | 交付物 |
|-------|------|--------|
| **Phase 1** — 核心稳定化 | 1-2 周 | Span 集成, Reader 扩展, TCO, macroexpand, 200+ tests |
| **Phase 2** — 系统编程 | 2-4 周 | FFI, `#[zio_export]`, Buffer, File I/O, struct, try/catch |
| **Phase 3** — 宏与元编程 | 2-3 周 | 卫生宏, Reader macro, Compiler macro |
| **Phase 4** — 标准库+并发 | 2-3 周 | 懒序列, Future/Channel, async, 包管理器 |
| **Phase 5** — CLOS/多方法 | 2-3 周 | Generic function, defmethod, condition system |
| **Phase 6** — LLM/性能 | 3-4 周 | 向量原语, LLM API, Agent 框架, Baseline JIT |
| **Phase 7** — 生产化 | 持续 | LSP, Debugger, WASM, Profiler |

项目同时推进两个工程原型：
- **Datomic 风格 Datalog 数据库** — 内存在, 时间旅行, 纯宏 API
- **自学习模型框架** — 基于同像性的 AutoML + 程序搜索

详见 [docs/ROADMAP.md](docs/ROADMAP.md)。

## License

MIT