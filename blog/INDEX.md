# Zio: A Modern Lisp for the Agent Era

> 系列 blog，边学边做。从零搭建一个可工作的 Lisp 方言，在 Rust 中实现。

---

## 总览

这个系列不是教程，是**设计笔记**。每个帖子围绕一个具体的架构决策展开——为什么这样选、代价是什么、代码长什么样。

每篇对应 `zio/` 仓库中一个具体的代码模块。你可以打开对应文件跟着读。

```
zio/
├── core/           ← Lisp 运行时核心
│   └── src/
│       ├── sexp.rs         # 01: 代码即数据
│       ├── value.rs        # 02: 运行时值
│       ├── eval.rs         # 03: eval/apply
│       ├── context.rs      # 04: 显式状态
│       ├── special/        # 05: 特殊形式
│       ├── macros.rs       # 06: 宏系统
│       ├── builtins.rs     # 07: 内置函数
│       ├── env.rs          # 08: 词法环境
│       ├── zos/            # 11: ZOS 对象系统
│       │   ├── class.rs
│       │   ├── gf.rs
│       │   └── method.rs
├── reader/         ← 解析器
│   └── src/
│       ├── lexer.rs        # 09: 词法分析
│       └── reader.rs       # 09: S 表达式解析
└── tools/           ← 应用层
    └── src/
        └── main.rs         # 10: REPL + 模块加载
```

## 已发布

| # | 标题 | 对应代码 | 核心概念 |
|---|------|----------|----------|
| 01 | [你好，eval](01-hello-eval.md) | `eval.rs`, `sexp.rs` | eval 循环, 同像性, S 表达式 → Value |
| 02 | [两个世界：Sexp 与 Value](02-two-worlds.md) | `sexp.rs`, `value.rs` | ADR-001: 语法树 vs 运行时值, 宏的数据往返 |
| 03 | [显式状态：EvalEngine Trait](03-explicit-state.md) | `context.rs`, `eval.rs` | ADR-002: 从 thread_local! 到 trait object |

## 计划中

| # | 标题 | 对应代码 | 核心概念 |
|---|------|----------|----------|
| 04 | 持久化数据结构 | `env.rs`, `value.rs` | ADR-003: im crate, 结构共享, 不可变性 |
| 05 | 特殊形式 | `special/` | 12 种特殊形式, TCO, loop/recur |
| 06 | 宏：代码写代码 | `macros.rs` | defmacro, 同像性, syntax-rules |
| 07 | 内置函数 | `builtins.rs` | NativeFn, engine 参数, IoHost |
| 08 | 词法环境与闭包 | `env.rs` | 词法作用域链, 闭包捕获 |
| 09 | 解析器 | `lexer.rs`, `reader.rs` | tokenize → parse, Span 位置, Reader macro |
| 10 | 模块系统与 REPL | `main.rs`, `module.rs` | 模块加载, 命名空间, 包管理器 |
| 11 | ZOS：运行时对象模型 | `zos/` | Object/Class/GF/Method/MOP/Condition |
| 12 | 条件系统 | `zos/condition.rs` | try/catch, restart, 错误恢复 |
| 13 | CLOS 多方法 | `zos/gf.rs` | 多分派, Method Combination, CPL |
| 14 | MOP：自省与元编程 | `zos/mop.rs` | 元类, 反射, 自定义分派 |
| 15 | 从 REPL 到语言 | — | 自举编辑器, LSP, 生产化 |

## 主题串

除了按编号阅读，也可以用主题串浏览：

**架构决策线**：01 → 02 → 03 → 04

**语言实现线**：01 → 05 → 06 → 07 → 08

**ZOS 对象系统线**：11 → 12 → 13 → 14

**工具链线**：09 → 10 → 15

## 配套代码

```bash
# 跑全部测试
cd zio && cargo test

# 启动 REPL
cargo run

# 查看完整架构文档
cat docs/zio-philosophy.md
cat docs/zos-spec.md
cat docs/zio-architecture.md
cat docs/eval-pipeline.md
cat docs/adrs.md
```

---

> 这是一个活系列——随着 Zio 从 v0.2 走向 v1.0，帖子会不断增加。
