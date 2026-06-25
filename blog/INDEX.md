# Zio: A Modern Lisp for the Agent Era

> 系列 blog，边学边做。从零搭建一个可工作的 Lisp 方言，在 Rust 中实现。

---

## 总览

这个系列不是教程，是 **设计笔记**。每个帖子围绕一个具体的架构决策展开——为什么这样选、代价是什么、代码长什么样。

每篇对应 `zio/` 仓库中一个具体的代码模块。你可以打开对应文件跟着读。

```
zio/
├── core/         ← Lisp 运行时核心
│   └── src/
│       ├── sexp.rs       # 01: 代码即数据
│       ├── value.rs      # 02: 运行时值
│       ├── eval.rs       # 03: eval/apply
│       ├── context.rs    # 04: 显式状态
│       ├── special/      # 05: 特殊形式
│       ├── macros.rs     # 06: 宏系统
│       ├── builtins.rs   # 07: 内置函数
│       └── env.rs        # 08: 词法环境
├── reader/       ← 解析器
│   └── src/
│       ├── lexer.rs      # 09: 词法分析
│       └── reader.rs     # 09: S 表达式解析
└── cli/          ← 应用层
    └── src/
        └── main.rs       # 10: REPL
```

## 阅读顺序

| # | 标题 | 对应代码 | 核心概念 |
|---|------|----------|---------|
| 01 | [你好，eval](01-hello-eval.md) | `eval.rs`, `sexp.rs` | eval 循环, 同像性, S 表达式 → Value |
| 02 | [两个世界：Sexp 与 Value](02-two-worlds.md) | `sexp.rs`, `value.rs` | ADR-001: 语法树 vs 运行时值, 宏的数据往返 |
| 03 | [显式状态：EvalEngine Trait](03-explicit-state.md) | `context.rs`, `eval.rs` | ADR-002: 从 thread_local! 到 trait object |
| 04 | [持久化数据结构为什么是默认选择](04-persistent-data.md) | `env.rs`, `value.rs` | ADR-003: im crate, 结构共享, 不可变性的收益 |
| 05 | [特殊形式：eval 的内核](05-special-forms.md) | `special/` | 12 种特殊形式, 为什么它们不能被 builtin 替代 |
| 06 | [宏：代码写代码](06-macros.md) | `macros.rs` | defmacro, 同像性在行动, macroexpand |
| 07 | [内置函数：Rust 合同边界](07-builtins.md) | `builtins.rs` | NativeFn, map/filter/reduce 的 engine 参数 |
| 08 | [词法环境与闭包](08-env-closure.md) | `env.rs` | Env 链, 闭包捕获, let/loop |
| 09 | [解析器：从字节到 S 表达式](09-reader.md) | `lexer.rs`, `reader.rs` | tokenize → parse, 注释处理, EOF 边界 |
| 10 | [REPL 到语言：整合的艺术](10-repl-to-language.md) | `main.rs`, `module.rs` | stdlib 加载, 模块系统, 错误处理 |

---

## 主题串

除了按编号阅读，也可以用主题串浏览：

**架构决策线**：01 → 02 → 03 → 04

**语言实现线**：01 → 05 → 06 → 07 → 08

**工具链线**：09 → 10

---

## 配套代码

每篇末尾有 `## 在代码中` 小节，告诉你打开哪个文件、看哪个函数、跑哪个测试。

```bash
# 跑全部测试
cd zio && cargo test

# 跑某个测试
cargo test -p zio-reader test_read_core_stdlib_do_wrapped

# 启动 REPL
cargo run
```

---

## 状态

| 帖子 | 状态 |
|------|------|
| 01: 你好，eval | ✅ 已发布 |
| 02: 两个世界 | ✅ 已发布 |
| 03: 显式状态 | ✅ 已发布 |
| 04-10 | 📝 计划中 |

> 这是一个活系列——随着 Zio 从 v0.2 走向 v1.0，帖子会不断增加。