# Zio 系统架构

> Version 0.4 — 分层架构、组件状态、性能目标
>
> 本文件是架构总览。实现细节见 [eval-pipeline.md](eval-pipeline.md)，ZOS 规范见 [zos-spec.md](zos-spec.md)，设计决策见 [adrs.md](adrs.md)。

---

## 1 概述

### 1.1 系统定义

```text
Zio = Lisp 核心（同像性 + eval/apply + 宏）
    + Rust 宿主（FFI + 嵌入 + 零开销）
    + 统一运行时对象模型（ZOS：AMOP + MOP）
    + 扩展库生态（Datalog · Agent · 自学习）
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
| 4 | **核心最小，其余是库** | ✅ ZOS 规范 |
| 5 | **协议比实现重要** | ✅ EvalEngine + MOP |

---

## 2 当前状态

### 2.1 代码库指标

| 指标 | 值 |
|------|-----|
| 总 LOC | ~3,500 Rust（core 2,400 + reader 300 + cli 200 + 测试） |
| 测试 | 103 passing, 0 failing |
| 编译警告 | 0 |
| Crates | `zio-core`、`zio-reader`、`zio`（CLI） |
| thread_local 全局变量 | 0 |
| 核心依赖 | `im`（持久化数据结构）、`thiserror`、`itertools` |

### 2.2 组件成熟度

| 组件 | 状态 | 说明 |
|------|------|------|
| Reader | ✅ 可用 | 基于栈的解析，支持 quote reader macro |
| Sexp | ✅ 可用 | 干净的数据结构，Sexp 与 Value 分离 |
| Value | ✅ 可用 | 缺少 Char/Ratio/BigInteger 变体 |
| Env | ✅ 可用 | 词法作用域链，闭包正确 |
| EvalEngine | ✅ 可用 | 0 thread_local，trait-based，可 mock |
| Tail-call | ⚠️ 仅 loop/recur | 扩展到所有尾位置（Phase 1） |
| Specials | ✅ 12 种 | 基本完备 |
| Macros | ✅ 可用 | 基础 defmacro，无 hygiene |
| Builtins | ⚠️ 30 个 | 缺少字符串、文件、集合操作 |
| Span | ⚠️ 未集成 Sexp | Phase 1 P0 |
| Module sys | 🚧 基础 | 无缓存、无循环依赖检测 |
| ZOS Runtime | ❌ 待实现 | Phase 2 |
| FFI | ❌ — | Phase 4 |
| 标准库 .zio | ❌ — | Phase 4 |

### 2.3 特殊形式（12 种）

| 形式 | 文件 | 说明 |
|------|------|------|
| `quote` | data.rs | 阻止求值 |
| `def` | bindings.rs | 定义全局变量 |
| `defun` | bindings.rs | 定义函数 |
| `defmacro` | bindings.rs | 定义宏 |
| `fn` | bindings.rs | 匿名函数 |
| `if` | control.rs | 条件 |
| `do` | control.rs | 顺序求值 |
| `and` / `or` | control.rs | 布尔运算符 |
| `cond` | control.rs | 多分支条件 |
| `let` / `let*` | letloop.rs | 词法绑定 |
| `loop` / `recur` | letloop.rs | 尾递归循环 |
| `module` | module_forms.rs | 模块声明 |
| `require` | module_forms.rs | 模块引入 |

### 2.4 内置函数（30 个）

**算术**: `+` `-` `*` `/`
**比较**: `=` `<` `>` `<=` `>=`
**类型谓词**: `nil?` `boolean?` `number?` `string?` `symbol?` `keyword?` `list?` `vector?` `map?` `fn?`
**列表**: `cons` `car` `cdr` `list`
**高阶**: `map` `filter` `reduce`
**I/O**: `println` `prn` `read-line`
**其他**: `macroexpand`（todo!()）

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
│              Runtime + Compiler Layer (ZOS)          │
│   ┌──────────┐ ┌──────────┐ ┌───────────────────┐  │
│   │  ZOS     │ │  Eval    │ │  Compiler (ZIR)    │  │
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
| **Host** | EvalContext、生命周期、全局状态、I/O 抽象 | `zio-core` | Rust |
| **Reader** | tokenize + Sexp parse + reader macro | `zio-reader` | Rust |
| **Runtime** | eval/apply、TCO、special forms、macroexpand | `zio-core` | Rust |
| **ZOS** | Class/GF/Method/MOP/Package/Condition | `zio-core` | Rust |
| **Stdlib** | 标准库函数、集合、IO 封装 | `tools/stdlib/zio/` | `.zio` |
| **Tools** | CLI · REPL · LSP · 编辑器 · 调试器 · `#[zio_export]` proc-macro | `zio` | Rust + Zio |
| **Extension Libs** | Datalog · Agent · 持久化集合 | `zio-datalog` / `zio-agent` / `zio-persistent` | Rust + Zio |

### 3.3 Crate 依赖图

**当前**：

```
zio (tools binary)
├── zio-reader    (reader/src/: 解析字符串 → Sexp + Span)
├── zio-core      (core/src/: 运行时 Value/Env/Eval + ZOS)
│   └── zio-reader (依赖 core 的类型定义)
└── lib/          (扩展库: datalog, agent, persistent)
    └── zio-core
```

**未来（Phase 5+，Compiler/JIT 加入 tools）**：

```
zio (tools binary: CLI + REPL + LSP + 编辑器 + JIT)
├── zio-reader
├── zio-core (运行时 + ZOS)
│   ├── eval     (AST 解释器)
│   └── compiler (ZIR + JIT，可选特性)
├── tools/stdlib/     (.zio 标准库)
└── lib/              (扩展库)
    ├── zio-datalog
    ├── zio-agent
    └── zio-persistent
```

---

## 4 性能目标

| 场景 | 当前（v0.2, AST eval） | 目标（v0.7, JIT） |
|------|----------------------|-----------------|
| 简单整数循环 (10^7 iter) | ~300 ms | ~50 ms |
| 函数调用开销 | ~50 ns | ~5 ns（原生） |
| 启动时间 | <10 ms | <5 ms |
| 内存（idle） | ~2 MB | ~1 MB |
| GF 分派（缓存命中） | N/A（未实现） | ~100 ns |
| GF 分派（缓存未命中） | N/A（未实现） | ~1 µs |

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
| [eval-pipeline.md](eval-pipeline.md) | Eval 循环、TCO、宏系统、编译器管线 |
| [zos-spec.md](zos-spec.md) | ZOS 完整规范（Class/GF/Method/MOP/Condition） |
| [roadmap.md](roadmap.md) | Phase 路线图、交付标准、依赖分析、应用蓝图 |
| [adrs.md](adrs.md) | 架构决策记录（ADR-001 ~ ADR-011） |
| [glossary.md](glossary.md) | 术语参考 |
