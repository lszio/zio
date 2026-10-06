# Zio: A Modern Lisp for the Agent Era

> 系列 blog，边学边做。从零搭建一个可工作的 Lisp 方言，在 Rust 中实现。

---

## 总览

这个系列不是教程，是**设计笔记**。每个帖子围绕一个具体的架构决策展开——为什么这样选、代价是什么、代码长什么样。

各篇保留早期解释器的设计过程与示意代码，不是当前 API、性能结果或阶段完成清单。现有文件是 4 篇文章加本索引；尚未落地的产品与编译器目标标为 Planned。

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
│       ├── builtins/      # 07: 内置函数（按域分模块）
│       ├── env.rs          # 08: 词法环境
│       ├── zos/            # 11: ZOS 对象系统
│       │   ├── class.rs
│       │   ├── gf.rs
│       │   └── method.rs
│       └── reader/         # 09: 词法分析与解析
├── cli/            ← zio-cli 可执行入口
├── ai/             ← 当前 AI crate
├── learning/       ← 当前学习服务 crate
└── app/            ← 当前应用 crate
```

## 当前基线与实施导航

- [语言架构](../docs/zio-architecture.md)、[Zio/Grove 批准基线](../docs/self-learning-architecture.md)、[ADR-019](../docs/adrs.md#adr-019-grove-独立应用与同像性逻辑演化)。
- [收敛实施计划](../docs/superpowers/plans/2026-10-05-zio-grove-convergence.md)：ZOS 留在语言核心；展开/分析/编译自举（Planned）与编辑器/APP 开发分开验收，Rust 收敛为最小运行时、宿主与性能原语。
- Numa（连续数值数组/矩阵/向量与后端接口）、Rill（参数/子命令/帮助、命令组合、终端 I/O 与退出状态，不承担语言求值）、Loom（模型/工具/会话/预算/取消/provider/ACP）是按需安装、独立版本发布的官方库，不是语言内置特性；Grove 是消费语言与三库的独立 APP，持学习目标、实验、反馈、检查点、独立评价、人工批准、逻辑演化和 Web，核心无反向依赖。新增能力均 Planned，公共能力仅按真实复用抽取。
- 当前 `zio-ai`/`ai/` 是 Loom 的起点，`zio-cli`/`cli/` 是未来消费 Rill 的可执行宿主；历史代码与包命令不改名。正式短名不是已确认注册包 ID，未来规范目录和逻辑命名空间见[术语表](../docs/glossary.md)。
- Grove 真实模型生成代码→解析/能力检查/隔离执行→证据修订→Web 源码/路径/diff→反馈候选→独立评估→人工发布仍是 Planned。固定 agent 回应、训练队列记录、静态站点与模拟 Q-learning 都不是闭环证据；自动实验受批准预算/编辑范围限制，并保护可信基线与发布守卫。
- ACP 是双向 Agent Client Protocol，Loom 的 Planned 顺序为 client/teacher adapter 后 server，由 Grove 注入执行回调并通过未来 `grove acp serve` 装配；普通语言 CLI 不要求 Loom。同像性支持表示和编辑，不保证模型透明、程序正确或 LLM 自述可信。


## 已发布

| # | 标题 | 对应代码 | 核心概念 |
|---|------|----------|----------|
| 01 | [你好，eval](01-hello-eval.md) | `eval.rs`, `sexp.rs` | eval 循环, 同像性, S 表达式 → Value |
| 02 | [两个世界：Sexp 与 Value](02-two-worlds.md) | `sexp.rs`, `value.rs` | ADR-001: 语法树 vs 运行时值, 宏的数据往返 |
| 03 | [显式状态：EvalEngine Trait](03-explicit-state.md) | `context.rs`, `eval.rs` | ADR-002: 从 thread_local! 到 trait object |
| 11 | [ZOS：统一运行时对象模型（历史设计稿）](04-zos-overview.md) | `zos/` | Object/Class/GF/Method/MOP 方向；旧阶段与数字不代表当前验收 |

## 计划中

| # | 标题 | 对应代码 | 核心概念 |
|---|------|----------|----------|
| 04 | 持久化数据结构 | `env.rs`, `value.rs` | ADR-003: im crate, 结构共享, 不可变性 |
| 05 | 特殊形式 | `special/` | 12 种特殊形式, TCO, loop/recur |
| 06 | 宏：代码写代码 | `macros.rs` | defmacro, 同像性, syntax-rules |
| 07 | 内置函数 | `builtins/` | NativeFn, engine 参数, IoHost |
| 08 | 词法环境与闭包 | `env.rs` | 词法作用域链, 闭包捕获 |
| 09 | 解析器 | `lexer.rs`, `reader.rs` | tokenize → parse, Span 位置, Reader macro |
| 10 | 模块系统与 REPL | `main.rs`, `module.rs` | 模块加载, 命名空间, 包管理器 |
| 12 | 条件系统 | `zos/condition.rs` | try/catch, restart, 错误恢复 |
| 13 | CLOS 多方法 | `zos/gf.rs` | 多分派, Method Combination, CPL |
| 14 | MOP：自省与元编程 | `zos/mop.rs` | 元类, 反射, 自定义分派 |
| 15 | 从 REPL 到语言 | — | 编译器自举（Planned）与编辑器/LSP/APP（独立目标） |

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
