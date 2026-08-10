# Zio 系统架构

> Version 0.4 — 分层架构、组件状态、性能目标
>
> 本文件同时描述当前架构与未来目标。实现状态以
> [特性矩阵](feature-matrix.md)为唯一依据，生成的仓库数量见
> [项目状态](status.md)。实现细节见 [eval-pipeline.md](eval-pipeline.md)，
> ZOS 规范见 [zos-spec.md](zos-spec.md)，设计决策见 [adrs.md](adrs.md)。

---

## 1 概述

### 1.1 系统定义

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + 统一运行时对象模型（ZOS：AMOP + MOP）
    + 规划中的扩展库生态（Datalog · Agent · 自学习）
```

详细哲学定义在 [zio-philosophy.md](zio-philosophy.md)。

### 1.2 目标用户

| 用户画像 | 场景 | Zio 价值 |
|----------|------|----------|
| 系统程序员 | CLI 工具、配置文件、脚本 | Rust FFI 零开销 + Lisp 表达力 |
| LLM 开发者 | Agent 编排、tool-use | eval loop = agent loop |
| Common Lisp 用户 | 现代 CL + Rust 生态 | CLOS + MOP + cargo |
| 教学 | 编程语言课程 | 代码最简、概念正交 |

### 1.3 设计定理

| # | 定理 | 状态 |
|---|------|------|
| 1 | **所有 mutable 状态必须显式** | ✅ 0 thread_local 全局变量 |
| 2 | **Rust 是合同边界，Lisp 是组合层** | ✅ NativeFn + EvalEngine |
| 3 | **宏是用户扩展 eval 的方式** | ✅ defmacro 可用 |
| 4 | **核心最小，其余是库** | 设计原则；扩展库均为 [Planned](feature-matrix.md) |
| 5 | **协议比实现重要** | ZOS 子集为 [Experimental](feature-matrix.md)，完整 MOP 为规划 |

---

## 2 当前状态

### 2.1 可验证状态

当前 workspace 由 `zio-core` 和 `zio-cli` 两个 crate 组成。测试、特殊形式、
native binding 和 runnable example 数量由
[`tools/project-status.sh`](../tools/project-status.sh) 生成，不在本页复制；
快照见[项目状态](status.md)。

### 2.2 组件成熟度

| 组件 | 状态 | 说明 |
|------|------|------|
| Reader 与语法展开 | Stable | reader unit tests |
| AST evaluator、闭包、核心宏与 stdlib | Stable | core tests 与 runnable examples |
| ZOS class 与 generic dispatch 子集 | Experimental | 受 `zos-concept.zio` 合同覆盖，API 仍可能变化 |
| Persistent library、Datalog evaluator | Planned | 现有文件仅是 placeholder 或 query-data demo |
| ZIR、bytecode VM、JIT 与应用能力 | Planned | 只有批准设计，不是当前运行时 |

完整证据和应用能力状态见[特性矩阵](feature-matrix.md)。

---

## 3 分层架构

### 3.1 层视图

```
┌────────────────────────────────────────────────────┐
│                   Application Layer                  │
│   CLI · REPL · Editor · LSP · WASM · Embed · Agent  │
├────────────────────────────────────────────────────┤
│                 Extension Library Layer              │
│   zio-datalog · zio-agent · zio-ai · zio-persist    │
│   zio-entity · zio-protocol · zio-actor · zio-graph │
├────────────────────────────────────────────────────┤
│               Standard Library Layer (.zio)          │
│   collections · math · io · json · test · llm        │
├────────────────────────────────────────────────────┤
│       Runtime Layer + Planned Compiler Layer (ZOS)   │
│   ┌──────────┐ ┌──────────┐ ┌───────────────────┐  │
│   │ ZOS exp. │ │ AST Eval │ │ Compiler (Planned) │  │
│   │  Class   │ │  TCO     │ │  JIT               │  │
│   │  GF      │ │  Macro   │ │  Bytecode          │  │
│   │  Method  │ │  Expand  │ │  Codegen           │  │
│   │  MOP     │ │          │ │                    │  │
│   │  Package │ │          │ │                    │  │
│   │  Cond    │ │          │ │                    │  │
│   └──────────┘ └──────────┘ └───────────────────┘  │
├────────────────────────────────────────────────────┤
│                Frontend Layer (Reader)               │
│   Reader · Parser · Sexp · Span · Reader Macro      │
├────────────────────────────────────────────────────┤
│                Rust 宿主层 (Host)                    │
│   EvalContext · EvalRuntime · ModuleRegistry         │
│   ZosRuntime · NativeFn · IoHost · FFI              │
│   #[zio_export] proc-macro · 插件加载器              │
└────────────────────────────────────────────────────┘
```

### 3.2 层职责

| 层 | 做什么 | Crate | 语言 |
|----|--------|-------|------|
| **Host** | EvalContext、生命周期、全局状态 | `zio-core` | Rust |
| **Reader** | tokenize + Sexp parse + reader macro | `zio-core` | Rust |
| **Runtime** | eval/apply、TCO、special forms、macroexpand | `zio-core` | Rust |
| **ZOS** | Experimental Class/GF/Method subset; broader MOP is planned | `zio-core` | Rust |
| **Stdlib** | 当前 core macros/stdlib；更广标准库为规划 | `core` | Rust + Zio |
| **Extension Libs** | Planned: Datalog · Agent · persistent collections | `lib/zio/*.zio` placeholders | **Zio** |

### 3.3 Crate 依赖图

**当前（两个 workspace crate）**：

```
zio-cli (cli/src/main.rs)
└── zio-core (reader + AST evaluator + experimental ZOS subset)

lib/zio/*.zio (planned extension-library placeholders; not workspace crates)
```

**规划（ZIR/VM/JIT，尚未实现）**：

```
zio-cli (CLI + REPL)
├── zio-core (运行时 + ZOS)
│   ├── eval     (AST 解释器)
│   └── compiler (ZIR + bytecode VM + JIT，可选、Planned)
├── stdlib/           (.zio 标准库)
└── lib/zio/          (扩展库，纯 Zio 实现)
    ├── persistent.zio
    ├── datalog.zio
    └── agent/*.zio
```

上述 compiler 与扩展库均为 [Planned](feature-matrix.md)。

---

## 4 规划性能目标

以下数字是设计目标，不是当前基准结果。当前可复现的 AST 测量见
[AST evaluator baseline](../benchmarks/README.md)；ZIR、VM 和 JIT 状态为
[Planned](feature-matrix.md)。

本页不复制某台机器上的当前耗时。已批准设计中的 VM/JIT 性能数字仅是未来
验收目标；比较必须使用 AST baseline 文档规定的相同 workload、engine、
release build 与 machine metadata。

## 5 跨平台目标

| 目标 | Rust target | 状态 |
|------|-------------|------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | ✅ 开发主力 |
| macOS ARM | `aarch64-apple-darwin` | ✅ CI |
| macOS x86_64 | `x86_64-apple-darwin` | ⏳ 需 CI |
| Windows | `x86_64-pc-windows-msvc` | ⏳ 待测试 |
| WASM | `wasm32-unknown-unknown` | Phase 7 |
| ARM Linux | `aarch64-unknown-linux-gnu` | Phase 7 |

---

## 6 相关文档

| 文档 | 内容 |
|------|------|
| [zio-philosophy.md](zio-philosophy.md) | 语言哲学、设计定理、设计原则 |
| [feature-matrix.md](feature-matrix.md) | 权威能力状态与证据 |
| [status.md](status.md) | 自动生成的仓库事实 |
| [eval-pipeline.md](eval-pipeline.md) | Eval 循环、TCO、宏系统、编译器管线 |
| [zos-spec.md](zos-spec.md) | ZOS 完整规范（Class/GF/Method/MOP/Condition） |
| [roadmap.md](roadmap.md) | Phase 路线图、交付标准、依赖分析、应用蓝图 |
| [adrs.md](adrs.md) | 架构决策记录（ADR-001 ~ ADR-011） |
| [glossary.md](glossary.md) | 术语参考 |
