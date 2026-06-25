# Zio 架构手册 — v0.3

> 作者：来自截至 v0.2 重构（EvalContext + EvalEngine trait，103 tests passing，0 warnings）的代码库洞察
> 本文档从第一性原理出发，定义 Zio 的目标、架构、路线图和应用示例。

---

## 目录

1. [Zio 是什么？— 第一性原理定义](#1-zio-是什么第一性原理定义)
2. [当前状态审计](#2-当前状态审计)
3. [分层架构](#3-分层架构)
4. [核心类型系统](#4-核心类型系统)
5. [求值与编译管线](#5-求值与编译管线)
6. [路线图（分 Phase）](#6-路线图分-phase)
7. [应用一：Datomic 风格的 Datalog 数据库](#7-应用一datomic-风格的-datalog-数据库)
8. [应用二：基于同像性的自学习模型框架](#8-应用二基于同像性的自学习模型框架)

---

## 1. Zio 是什么？— 第一性原理定义

### 核心命题

```text
Lisp = S-expressions + eval/apply
     = 同像性 (homoiconicity)
     = 代码即数据 → 宏系统
     = 交互式开发 → REPL 原生
```

Zio 从这条线出发，但**不止于此**。Zio 的扩充分解：

```
Zio = Lisp 核心
    + Rust 宿主系统编程能力 (FFI, 内存, 并发)
    + LLM/agent-era 原生支持 (向量, prompt, tool-use)
    + 渐进类型 + 可选 JIT
    + 跨平台 (任何 Rust 目标)
```

### 目标用户

| 用户画像 | 使用的场景 | Zio 提供的价值 |
|----------|-----------|---------------|
| **系统程序员** | 写 CLI 工具、配置文件解析、脚本 | Rust FFI 零开销 + Lisp 高表达力 |
| **数据科学家** | 数据处理流水线、模型编排 | 同像性让 pipeline 可自动分析和改写 |
| **LLM 开发者** | agent 编排、tool-use、prompt 工程 | eval loop 即 agent loop，宏即 DSL 生成器 |
| **Common Lisp 用户** | 需要现代 CL 且要 Rust 生态 | 近似 CL 能力 + cargo 生态 |
| **教学** | 程序设计语言课程 | 代码最简，概念正交，实现可用 |

### 设计定理

```
定理 1: 所有 mutable 状态必须显式
推论:  无 thread-local 全局变量 → EvalContext
代码:  当前已经达成 (103 tests, 0 globals)

定理 2: Rust 是合同边界，Lisp 是组合层
推论:  性能关键路径用 NativeFn 实现
代码:  map/filter/reduce 已通过 engine 参数支持

定理 3: 宏是用户扩展 eval 的方式
推论:  没有特殊形式不可用宏替代
代码:  当前 defmacro 可用

定理 4: 模块系统是代码组织的唯一方式
推论:  flat namespace → 分层命名空间
代码:  Symbol { ns, name } + (module :name ...)
```

---

## 2. 当前状态审计

### 代码库指标 (v0.2 重构后)

| 指标 | 值 |
|------|-----|
| **总 LOC** | ~3,500 Rust (core 2,400 + reader 300 + cli 200) |
| **测试** | 103 passing, 0 failing |
| **编译警告** | 0 |
| **Crates** | `zio-core`, `zio-reader`, `zio` (CLI) |
| **thread_local! 全局变量** | 0 (已全部移除) |
| **跨 crate 依赖** | `zio-reader → zio-core`, `zio → zio-core + zio-reader` |
| **核心依赖 (非 dev)** | `im` (持久化数据结构), `thiserror` |

### 组件成熟度矩阵

```
                   ┌──────┬──────┬──────┬──────┐
                   │ 现有  │ 覆盖  │ 正确性 │ 性能  │
    ┌──────────────┼──────┼──────┼──────┼──────┤
    │ Reader       │ ✅   │ 基础  │ ✅   │ N/A  │
    │ Sexp 类型    │ ✅   │ 完整  │ ✅   │ N/A  │
    │ Value 类型   │ ✅   │ 缺少  │ ✅   │ 待优化 │
    │              │      │ char  │      │       │
    │              │      │ ratio │      │       │
    │              │      │ pinfo │      │       │
    ├──────────────┼──────┼──────┼──────┼──────┤
    │ Env(词法)    │ ✅   │ 完整  │ ✅   │ ✅   │
    │ EvalEngine   │ ✅   │ 完整  │ ✅   │ ✅   │
    │ Tail-call    │ ⚠️   │ loop  │ ✅   │ N/A  │
    │              │      │ only  │      │       │
    │ Specials     │ ✅   │ 12种  │ ✅   │ ✅   │
    │ Macros       │ ✅   │ 基础   │ ✅   │ N/A  │
    │ Builtins     │ ⚠️   │ 30个  │ ✅   │ ⚠️   │
    │ Span         │ ⚠️   │ 未集成 │ N/A  │ N/A  │
    │ Module sys   │ 🚧   │ 基础   │ ✅   │ N/A  │
    │ FFI          │ ❌   │ —    │ —    │ —    │
    │ 标准库 .zio  │ ❌   │ —    │ —    │ —    │
    └──────────────┴──────┴──────┴──────┴──────┘
```

### 当前特殊形式清单（12 种）

| 形式 | 文件 | 说明 |
|------|------|------|
| `quote` | data.rs | 阻止求值 |
| `def` | bindings.rs | 定义全局变量 |
| `defun` | bindings.rs | 定义函数 |
| `defmacro` | bindings.rs | 定义宏 |
| `fn` | bindings.rs | 匿名函数 |
| `if` | control.rs | 条件 |
| `do` | control.rs | 顺序求值 |
| `and`, `or` | control.rs | 布尔运算符 |
| `cond` | control.rs | 多分支条件 |
| `let`, `let*` | letloop.rs | 词法绑定 |
| `loop`, `recur` | letloop.rs | 尾递归循环 |
| `module` | module\_forms.rs | 声明模块 |
| `require` | module\_forms.rs | 引入模块 |

### 当前内置函数清单（30 个）

**算术**: `+`, `-`, `*`, `/`

**比较**: `=`, `<`, `>`, `<=`, `>=`

**类型谓词**: `nil?`, `boolean?`, `number?`, `string?`, `symbol?`, `keyword?`, `list?`, `vector?`, `map?`, `fn?`

**列表**: `cons`, `car`, `cdr`, `list`

**高阶**: `map`, `filter`, `reduce`

**I/O**: `println`, `prn`, `read-line`

**其他**: `macroexpand` (todo!())

---

## 3. 分层架构

```
                    ┌───────────────────────────────────┐
                    │          Application Layer         │
                    │  CLI · REPL · WASM · Embed · Agent │
                    │  LSP · DevTools · Debugger          │
                    ├───────────────────────────────────┤
                    │        Standard Library (.zio)      │
                    │  collections  math  io  net  json   │
                    │  test  serialize  crypto  llm       │
                    ├───────────────────────────────────┤
                    │          Extension API              │
                    │  #[zio_export] macro   FFI/C-ABI    │
                    │  plugin loader (dynamic lib)        │
                    ├────────────┬───────────┬───────────┤
                    │            │           │            │
                    │  Compiler  │  Runtime  │  Reader    │
                    │  (ZIR)     │  (eval)   │  (parse)   │
                    │  JIT       │  TCO      │  macrochar │
                    │  bytecode  │  GC       │  #\ ...    │
                    │            │           │            │
                    └────────────┴───────────┴───────────┘
                    ┌───────────────────────────────────┐
                    │        Rust 宿主层 (Todo)           │
                    │  EvalContext · EvalEngine · Ext    │
                    │  NativeFn · ModuleRegistry · FFI   │
                    └───────────────────────────────────┘
```

### 分层职责清晰分解

| 层 | 做什么 | 在哪个 crate | 用什么语言 |
|----|--------|-------------|-----------|
| **Host** | 管理 EvalContext、生命周期、全局状态 | `zio-core` | Rust |
| **Reader** | tokenize + Sexp parse + reader macro | `zio-reader` | Rust |
| **Runtime** | eval/apply、special forms、macroexpand | `zio-core` | Rust |
| **Compiler** | ZIR IR、优化、JIT codegen | `zio-compiler` (未来) | Rust |
| **Stdlib** | 标准库函数、数据类型语法糖 | `zio-stdlib` | `.zio` + Rust |
| **Extension** | `#[zio_export]`、`define-builtin!` 宏 | `zio-macros` (未来) | Rust proc-macro |
| **Application** | CLI/REPL/LSP/embed | `zio` / `zio-lsp` / 用户代码 | Rust + Zio |

### 关键架构决策日志 (ADRs)

#### ADR-001: Sexp 与 Value 分离

**决定**: `Sexp` 是语法树（代码），`Value` 是运行时值（执行结果）。两者通过 `From<Sexp> for Value` / `value_to_sexp` 转换。

**理由**: 同像性需要在两个域之间往返。Macro 接受 `Sexp`，返回 `Sexp`，然后 eval 将其变为 `Value`。

**代价**: 两个类似的枚举，偶尔需要 `into_value()` / `value_to_sexp()` 转换。

#### ADR-002: `EvalContext` + `EvalEngine` trait 收容所有 mutable 状态

**决定**: 所有可变状态（env, module registry, loader）属于 `EvalContext`，通过 `&dyn EvalEngine` trait 注入到特殊形式和 builtin 函数。

**理由**: 可测试性（mock engine）、可嵌入性（多个独立 EvalContext）、安全性（无 thread_local 全局）。

**代价**: 每个 NativeFn 签名多一个 `engine: &dyn EvalEngine` 参数。

#### ADR-003: 持久化数据结构 (im crate)

**决定**: 使用 `im::Vector` / `im::HashMap` 作为 List/Map 的默认实现。

**理由**: 结构共享使函数式更新便宜，不可变性防止 eval 中意外副作用。

**代价**: 比 `Vec` / `HashMap` 慢 2-5x（对小集合不明显）。

#### ADR-004: `Span` 最终将内联到 `Sexp`

**决定**: 当前 `SpannedSexp` 是包装类型，Phase 1 将 `span: Option<Span>` 直接嵌入每个 `Sexp` 变体。

**理由**: 没有源码位置的错误消息对用户不可用。Reader 的 tokenizer 在 parse 时已有 Token 位置信息。

---

## 4. 核心类型系统

### Sexp（语法树）

```rust
Sexp::Nil
Sexp::Boolean(bool)
Sexp::Integer(i64)         // → BigInteger (Phase 4)
Sexp::Float(f64)           // → f64 足够短期
Sexp::Ratio(i64, i64)      // Phase 5
Sexp::Complex(f64, f64)    // Phase 5
Sexp::Char(char)           // Phase 2
Sexp::String(String)
Sexp::Symbol(String)       // → Symbol { ns, name } 已完成
Sexp::Keyword(String)
Sexp::List(Vector<Sexp>)   // 持久化 Vector
Sexp::Vector(Vector<Sexp>) // 持久化 Vector
Sexp::Map(HashMap<Sexp, Sexp>)
Sexp::Set(HashSet<Sexp>)   // Phase 3, #{...}
```

### Value（运行时值）

```rust
Value::Nil
Value::Boolean(bool)
Value::Integer(i64)
Value::Float(f64)
Value::String(String)
Value::Symbol(String)
Value::Keyword(String)
Value::List(Vector<Value>)
Value::Vector(Vector<Value>)
Value::Map(HashMap<Value, Value>)
Value::Function(Arc<Function>)        // user-defined (closure)
Value::NativeFunction(NativeFn)       // Rust-implemented
Value::Macro(Macro)                   // defmacro result
Value::Atom(RefCell<Value>)           // Phase 4, CL-style atom
Value::Promise(Arc<Mutex<Option<Result<Value, EvalError>>>>) // Phase 4, future
Value::Buffer(Vec<u8>)                // Phase 3, byte array
Value::Pointer(*mut u8)               // Phase 4, FFI raw pointer
Value::Type(TypeDescriptor)           // Phase 5, type annotation
```

### Env（词法环境）

```rust
Env {
    data: RefCell<HashMap<String, Value>>,
    outer: Option<Arc<Env>>,
}
// bind(params, args) → child environment with params bound to args
// bind_variadic(params, rest_param, args) → with & rest support
```

支持：
- 词法作用域（闭包正确捕获环境）
- Variadic 函数 (`[a b & rest]`)
- 递归遍历 outer chain

### EvalEngine Trait（一切可变状态的接口）

```rust
pub trait EvalEngine {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(&self, name: &[String], source: &str, parent_env: &Arc<Env>) -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
}
```

---

## 5. 求值与编译管线

### 当前管线 (v0.3)

```
Source text
    │
    ▼
┌─────────────────────┐
│  zio-reader::       │
│  lexer::tokenize()  │  → Vec<String>  ("(", "+", "1", "2", ")")
│  reader::read()     │  → Sexp::List([Symbol("+"), Integer(1), Integer(2)])
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  zio-core::         │
│  eval::eval()       │  → 进入 eval_inner
│    │                │
│    ├─ Self-eval     │  Nil, Bool, Int, Float, String, Keyword → Value 直接
│    ├─ Symbol lookup │  env.get(name) → Value
│    ├─ Special form  │  eval_special_form → dispatch
│    ├─ Macro expand  │  try_expand_by_name → re-eval
│    ├─ Eval args     │  eval_inner on each arg
│    └─ apply()       │  match func type → bind env → eval body (or native)
│                     │
│  special::          │
│  └─ bindings.rs     │  def, , fn, defmacro
│  └─ control.rs      │  if, do, and, or, cond
│  └─ letloop.rs      │  let, let*, loop, recur
│  └─ module_forms.rs │  module, require
│  └─ data.rs         │  quote
│                     │
│  macros::           │
│  └─ apply_macro()   │  绑定参数 → eval 宏体 → value_to_sexp → re-eval
│  └─ try_expand()    │  符号查找 → 宏展开
│                     │
│  builtins::         │
│  └─ setup_env()     │  + - * / = < > <= >= cons car cdr list map filter ...
└─────────────────────┘
    │
    ▼
Value::Integer(3)      ← 结果
```

### 未来管线 (v0.7+, 加入 JIT)

```
Source text → Reader → Sexp
                           │
                    ┌──────┴──────┐
                    ▼              ▼
            ┌────────────┐  ┌──────────────┐
            │ eval (AST)  │  │ ZIR Compiler  │
            │ 当前管线     │  │ future          │
            │             │  │ Sexp → ZIR IR   │
            │             │  │ → opt → codegen │
            └──────┬──────┘  └──────┬──────────┘
                   │                │
                   ▼                ▼
             Value::Integer(3)   machine code
```

ZIR 的引入是 Phase 5+ 的事。在此之前，解释器性能足够验证语言语义。

---

## 6. 路线图（分 Phase）

### Phase 1: 核心稳定化（当前，1-2 周）

**目标**: Zio 成为可靠的 Lisp 内核 —— 可嵌入式、可测试、错误消息可用。

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **1.1** | `Span` 嵌入 `Sexp` | `sexp.rs`, `reader.rs` | 🔴 | `SpannedSexp` 合并到 `Sexp::span`；reader 在 parse 时填充；error.rs 输出带行号的错误 |
| **1.2** | Reader 扩展 | `reader/src/reader.rs`, `lexer.rs` | 🔴 | 支持 `#()` `#{}` `#\c`；`,@` splicing；多行 string；字符转义更多 |
| **1.3** | `Value` 添加 `Char` 类型 | `value.rs` | 🟡 | `Value::Char(char)`；reader parse `#\a` → Sexp::Integer(codepoint) → Value::Char？统一处理 |
| **1.4** | `macroexpand` 实现 | `builtins.rs`, `macros.rs` | 🟡 | 当前是 `todo!()` |
| **1.5** | 通用 TCO | `eval.rs` | 🟡 | 尾调用位置不再新建栈帧；使递归函数表达循环不爆栈 |
| **1.6** | 更多错误变体 | `error.rs` | 🟡 | 类型错误、索引越界、参数类型不匹配等 |
| **1.7** | 扩展测试覆盖 | 所有文件 | 🟡 | 当前 103 tests，目标 200+ |

### Phase 2: 系统编程基础（2-4 周）

**目标**: 能写实际的系统程序 —— 文件操作、网络、进程。

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **2.1** | FFI 原型 | `ffi.rs` (新) | 🔴 | `(ffi/c "libm" (fn sin "sin" f64 -> f64))`；用 libffi 或直接 Rust FFI |
| **2.2** | `#[zio_export]` proc-macro | `zio-macros` (新 crate) | 🔴 | 用户 Rust crate 写 `#[zio_export] fn my_fn(...)` 自动注册为 NativeFn |
| **2.3** | Buffer 类型 | `value.rs` | 🟡 | `Value::Buffer(Vec<u8>)`；`(buffer n)`, `(bset buf i byte)`, `(bget buf i)` |
| **2.4** | File I/O | `builtins.rs` 或 stdlib | 🟡 | `(slurp path)`, `(spit path data)`, `(with-open ...)` |
| **2.5** | 结构体 | `special/struct.rs` (新) | 🟡 | `(defstruct Point [x y])` → 生成构造器 + 访问器 + 模式匹配 |
| **2.6** | Error/Exception 完善 | `error.rs` | 🟡 | `(try body (catch Type e ...))`；stack trace |

### Phase 3: 宏与元编程（2-3 周）

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **3.1** | 卫生宏 | `macros.rs` (重构) | 🔴 | `syntax-rules` 风格的 pattern matching macro；自动重命名避免捕获 |
| **3.2** | Reader macro | `reader.rs` | 🟡 | `(set-reader-macros! #\[ ...)` → 自定义 reader syntax |
| **3.3** | Compiler macro | `macros.rs` | 🟡 | `(define-compiler-macro my-fn [pattern] expansion)` → 可选内联 |
| **3.4** | 代码 walker | `walker.rs` (新) | 🟡 | `(walk form f)` 用于编写复杂的程序变换宏 |

### Phase 4: 标准库 + 并发（2-3 周）

**目标**: 可以写实际的多线程、网络应用。

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **4.1** | 核心标准库 | `stdlib/` (新目录) | 🔴 | `zio.core.zio`：`comp` `partial` `juxt` `complement` `constantly` `identity` `inc` `dec` `take` `drop` `partition` `group-by` `sort-by` `interpose` |
| **4.2** | 懒序列 | `eval.rs` + stdlib | 🔴 | `(lazy-seq body)` 延迟求值；`(range)` `(iterate f x)` `(take n coll)` |
| **4.3** | Future/Promise | `builtins.rs` + `value.rs` | 🔴 | `(future expr)` 线程池；`@p` 阻塞 deref |
| **4.4** | Channel CSP | `builtins.rs` (新) | 🟡 | `(chan n)` `(put! ch v)` `(take! ch)` |
| **4.5** | Async/await 宏 | `special/` (新) | 🟡 | 基于 tokio/smol；`(go body)` → async block |
| **4.6** | JSON | `builtins.rs` 或 stdlib | 🟡 | `(json/parse str)` `(json/generate val)` |
| **4.7** | Test 框架 | `stdlib/test.zio` | 🟡 | `(deftest ...)` `(is ...)` `(run-tests)` |
| **4.8** | 包管理器 | `cli/` (新) | 🔴 | `zio install pkg-name`；从 registry 拉 `.zio` 文件 |

### Phase 5: CLOS + 多方法（2-3 周）

**目标**: 达到 CL 的对象系统水平。

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **5.1** | Generic function | `special/gf.rs` (新) | 🔴 | `(defgeneric draw (shape))` — 基于类的分派 |
| **5.2** | Defmethod | `special/gf.rs` | 🔴 | `(defmethod draw ((rect Rectangle)) ...)` |
| **5.3** | :before/:after/:around | `special/gf.rs` | 🟡 | 方法组合（标准 CLOS 模式） |
| **5.4** | Condition system | `special/condition.rs` (新) | 🟡 | `(define-condition ...)` `(handler-bind ...)` `(restart-case ...)` |

### Phase 6: LLM/Native（3-4 周）

| # | 任务 | 文件/区域 | 优先级 | 说明 |
|---|------|-----------|--------|------|
| **6.1** | 向量原语 | `builtins.rs` + Rust deps | 🔴 | `(embed text)` → `[0.1, 0.2, ...]`；`(cosine-sim v1 v2)` |
| **6.2** | LLM eval | `builtins.rs` | 🔴 | `(llm/completion :model "gpt-4" :prompt ... :tools ...)` |
| **6.3** | Prompt DSL | stdlib | 🟡 | `(defprompt summarize [text] "...{{text}}...")` |
| **6.4** | Agent 框架 | stdlib | 🟡 | `(agent "name" prompt [tool1 tool2 ...])` |
| **6.5** | Baseline JIT | `zio-compiler` (新 crate) | 🔴 | Cranelift JIT：选 hot 函数 → 编译为机器码 |

### Phase 7: 生产化（持续）

| # | 任务 | 说明 |
|---|------|------|
| **LSP Server** | `zio-lsp` crate；诊断、补全、跳转定义、hover |
| **Debugger** | DWARF + DAP 协议；断点、step、变量查看 |
| **WASM** | 编译到 WASM；浏览器内运行 |
| **Profiler** | 热路径识别 + 优化建议 |
| **文档生成** | `(doc fn-name)` 自动提取注释 + 签名 |

---

## 7. 应用一：Datomic 风格的 Datalog 数据库

### 动机

Datomic 是一个**以数据为事件流**的数据库，核心创新：
- **Datalog** 作为查询语言（声明式逻辑编程）
- **时间作为一等概念**（数据不可变，只追加）
- **同像性**：Datalog 查询是数据结构，可以被 Lisp 宏任意组合

Zio 的 Lisp 核心 + 持久化数据结构 (`im::Vector/HashMap`) 天然适合实现 Datomic 的子集。

### 架构

```
┌──────────────────────────────────────────────┐
│              Zio/Datomic                       │
│                                                │
│  ┌───────────────────────────────────────┐    │
│  │  API Layer (纯 Zio，同像性+宏)          │    │
│  │                                       │    │
│  │  (q '[:find ?name                     │    │
│  │       :in $ ?age                      │    │
│  │       :where                          │    │
│  │       [?e :person/name ?name]         │    │
│  │       [?e :person/age ?age]           │    │
│  │       [(> ?age 30)])                  │    │
│  │    db 40)                             │    │
│  │                                       │    │
│  │  → #{["Alice"] ["Bob"]}               │    │
│  └───────────┬───────────────────────────┘    │
│              │ 宏展开                            │
│              ▼                                  │
│  ┌───────────────────────────────────────┐    │
│  │  Query Plan (Sexp → Pattern Match)    │    │
│  │  ┌──────────┐ ┌────────┐ ┌─────────┐  │    │
│  │  │find vars │ │source  │ │patterns │  │    │
│  │  │[?name]   │ │$ (db)  │ │[?e :a ?n]│  │    │
│  │  └──────────┘ └────────┘ └─────────┘  │    │
│  └───────────┬───────────────────────────┘    │
│              ▼                                  │
│  ┌───────────────────────────────────────┐    │
│  │  Query Engine (Rust NativeFn)          │    │
│  │  ┌──────┐ ┌──────┐ ┌───────────────┐  │    │
│  │  │index │ │join  │ │unification    │  │    │
│  │  │scan  │ │planner│ │(substitution) │  │    │
│  │  └──────┘ └──────┘ └───────────────┘  │    │
│  └───────────┬───────────────────────────┘    │
│              ▼                                  │
│  ┌───────────────────────────────────────┐    │
│  │  Storage Layer (Rust)                  │    │
│  │  - EAVT/VAET/AEVT indices (im Map)     │    │
│  │  - Datom: [e a v tx op]               │    │
│  │  - Segment tree for time-travel        │    │
│  │  - Optional: rocksdb persistence       │    │
│  └───────────────────────────────────────┘    │
└──────────────────────────────────────────────┘
```

### 核心数据结构（Rust）

```rust
// Datom: 最小的数据单元
#[derive(Clone)]
pub struct Datom {
    pub entity_id: i64,
    pub attr: Value,         // :person/name
    pub value: Value,        // "Alice"
    pub tx_id: i64,          // 事务 id
    pub op: bool,            // true = add, false = retract
}

// 索引（持久化 im::HashMap, BTree 变体）
pub struct Database {
    pub eavt: HashMap<(i64, Value, Value, i64), Datom>,  // 实体优先
    pub vae: HashMap<(Value, Value, i64), Vec<Datom>>,   // 值优先
    pub ae: HashMap<(i64, Value), Vec<Datom>>,            // 属性质
}

// 编译后的查询计划
pub enum QueryOp {
    Scan {
        entity: PatternSlot,        // ?e, ?name, or concrete
        attr: PatternSlot,
        value: PatternSlot,
    },
    Join {
        left: Box<QueryOp>,
        right: Box<QueryOp>,
        on: (String, String),       // shared variable
    },
    Project {
        vars: Vec<String>,
        source: Box<QueryOp>,
    },
}
```

### 关键设计：查询即数据（同像性在发挥作用）

```lisp
;; 查询本身就是 Zio 列表（Sexp），可以被宏变换

(defmacro q [query-spec db]
  `(query-engine/run '~query-spec ~db))

;; 高阶查询：从查询生成查询
(defmacro q-by-age [age]
  `[:find ?name
    :in $
    :where
    [?e :person/name ?name]
    [?e :person/age ~age]])    ;; ~ 展开 age

;; 使用
(q (q-by-age 40) my-db)

;; 自动分页宏
(defmacro q-paged [query page-size]
  `(fn [db offset]
     (take ~page-size
       (drop offset
         (q ~query db)))))

;; 时间旅行 — — 查询过去的状态
(q [:find ?salary
    :where [?e :employee/salary ?salary]]
   (as-of db tx-42))          ;; tx-42 时刻的数据库快照
```

### 查询引擎的关键路径

```rust
impl QueryEngine {
    pub fn run(database: &Database, plan: &QueryOp) -> Result<Set<Vec<Value>>> {
        match plan {
            QueryOp::Scan { entity, attr, value } => {
                // 选择合适的索引进行扫描
                match (entity, attr, value) {
                    // 已知实体 + 属性 → AE 索引 (O(log n))
                    (PatternSlot::Concrete(e), PatternSlot::Concrete(a), _) =>
                        return database.ae.get(&(*e, a.clone()))
                            .map(|datoms| datoms.iter()
                                .map(|d| resolve_value(d, value))
                                .collect())
                            .unwrap_or_default();

                    // 已知值 → VAE 索引
                    (_, _, PatternSlot::Concrete(v)) =>
                        return database.vae.get(&(*v))
                            .map(|datoms| ...)
                            .unwrap_or_default();

                    // 完全未知 → EAVT 全扫 (慢)
                    _ => full_scan(database, entity, attr, value),
                }
            }

            QueryOp::Join { left, right, on } => {
                // 哈希连接：小关系构建哈希表，大关系探测
                let left_set = self.run(database, left)?;
                let right_set = self.run(database, right)?;
                hash_join(left_set, right_set, on)
            }

            QueryOp::Project { vars, source } => {
                let results = self.run(database, source)?;
                self.project(results, vars)
            }
        }
    }
}
```

### 实现路线（Phase 分步）

| Step | 内容 | 代码量 | 依赖 |
|------|------|--------|------|
| **P0** | `Datom` struct + `Database` 索引 (内存, `im::HashMap`) | 200 LOC | 无 |
| **P1** | Pattern variable 解析 + 单模式扫描 | 150 LOC | P0 |
| **P2** | Hash join + 多模式连接 | 200 LOC | P1 |
| **P3** | 事务系统 (add/retract + tx-id) | 150 LOC | P0 |
| **P4** | 时间旅行 (`as-of` / `since`) | 100 LOC | P3 |
| **P5** | 宏层 API (`q` defmacro, query 组合) | 100 LOC Zio | P2 |
| **P6** | 持久化 (RocksDB 或 sled) | 200 LOC | P3 |
| **P7** | 全文索引 + 规则 (`:rules`) | 250 LOC | P5 |

总计约 **1,350 LOC Rust + 100 LOC Zio**。P0-P2 可以在 1-2 天内完成原型。

### 和 Datomic 的差异

| Datomic 特点 | Zio/Datomic 处理 |
|-------------|-----------------|
| Peer-server 架构 | 嵌入式库（类似 Datomic Local） |
| 索引引擎用 DynamoDB | 内存 `im::HashMap`，可选 RocksDB |
| 事务通过 REST API | NativeFn 直接提交 |
| Datalog 完整实现 | 子集 + 宏扩展化 |
| 全文搜索 | P7 支持 |
| 属性级安全性 | Phase 2 问题 |

---

## 8. 应用二：基于同像性的自学习模型框架

### 动机

Lisp 的同像性（代码即数据）对机器学习有一个独特的价值：**模型可以 "读取" 自己的训练/推理代码，并改写它**。

传统 ML 流程：
```
数据 → 特征工程 → 模型定义 → 训练 → 部署
                                  ↺       ← 人工调参
```

Zio 同像性带来的可能：
```
(Zio 程序)
  │
  ▼
Reader → Sexp → eval → Value → 损失
  │                                │
  │  (同像性制作)                     │
  │  模型可以"看"自己的代码            │
  │  并自动变换它                     │
  ▼                                │
  (宏展开 / 程序变换) ←──────────────┘
  │
  ▼
  新的 Zio 程序 → 重新 eval  → 更低的损失
```

### 架构

```
┌────────────────────────────────────────────────────┐
│             Zio/AutoML — 自学习模型框架               │
│                                                     │
│  ┌────────────────────────────────┐                 │
│  │    用户层 API (纯 Zio 宏)        │                 │
│  │                                │                 │
│  │ (model neural-network          │                 │
│  │   (layer dense 128 relu)       │                 │
│  │   (layer dropout 0.2)          │                 │
│  │   (layer dense 10 softmax))    │                 │
│  │                                │                 │
│  │ (train model dataset           │                 │
│  │   :optimizer adam              │                 │
│  │   :lr 0.001                    │                 │
│  │   :epochs 100)                 │                 │
│  └───────────┬────────────────────┘                 │
│              │ 宏展开 (model → Rust 运算)              │
│              ▼                                      │
│  ┌────────────────────────────────┐                 │
│  │  模型描述 = 数据 (Sexp)          │                 │
│  │                                │                 │
│  │  (model-definition              │                 │
│  │   name: neural-network         │                 │
│  │   layers: [(dense 128 relu)    │                 │
│  │            (dropout 0.2)       │                 │
│  │            (dense 10 softmax)] │                 │
│  │   params: <tensor data>)      │                 │
│  └───────────┬────────────────────┘                 │
│              │                                      │
│              ├──→ Rust 层执行前向/反向                   │
│              │                                      │
│              └──→ 同像性自修改循环                       │
│                                                     │
│  ┌────────────────────────────────┐                 │
│  │  自修改引擎 (Sexp 变换器)        │                 │
│  │                                │                 │
│  │  1. 运行当前程序                 │                 │
│  │  2. 计算评估指标                 │                 │
│  │  3. 对程序的 Sexp 做模式替换      │                 │
│  │  4. 用宏展开新程序               │                 │
│  │  5. 评估新程序                   │                 │
│  │  6. 接受改进，丢弃退化           │                 │
│  └────────────────────────────────┘                 │
└────────────────────────────────────────────────────┘
```

### 关键洞察：模型=数据=程序=三合一

在 Zio 中，模型定义、训练脚本和数据结构**是同一个东西**。

```lisp
;; 这是一个合法的 Zio 程序，也是模型定义，也是可训练的
(def model
  (sequential
    (dense 784 128 :relu)
    (dropout 0.2)
    (dense 128 10 :softmax)))

;; 模型"定义" = Sexp = 可以被宏遍历和修改的数据
(defmacro auto-optimize [model-spec accuracy-threshold]
  `(let [current-model '~model-spec       ;; 引用，保持为数据
         acc (evaluate current-model)]
     (if (< acc ~accuracy-threshold)
       (let [variants (generate-variants current-model  ;; 生成变体
                                          :strategies
                                          [:add-layer
                                           :remove-layer
                                           :change-activation
                                           :adjust-width])]
         ;; 选择最佳变体
         (->> variants
              (map (fn [v] {:model v :acc (evaluate v)}))
              (sort-by :acc >)
              first
              :model))
       current-model)))

;; 使用 — 和普通 Zio 代码无差别
(def best-model (auto-optimize model 0.95))
(evaluate best-model)
```

### Rust 核心组件

```rust
// Tensor 操作 (NativeFn)
pub fn tensor_add(args, engine) -> Result<Value, EvalError>;
pub fn tensor_mul(args, engine);
pub fn tensor_matmul(args, engine);
pub fn tensor_relu(args, engine);
pub fn tensor_softmax(args, engine);
pub fn tensor_backward(args, engine);  // 自动微分

// 模型描述结构
pub struct ModelDef {
    pub name: String,
    pub layers: Vec<LayerDef>,
    pub params: HashMap<String, Tensor>,  // 可训练参数
}

// 训练循环
pub fn train(
    model: &ModelDef,
    data: &Dataset,
    config: &TrainConfig,
) -> Result<ModelDef, EvalError>;

// 程序变换（在同像性层面操作）
pub fn generate_variants(
    model_sexp: &Sexp,
    strategies: &[MutationStrategy],
) -> Vec<Sexp>;

pub enum MutationStrategy {
    AddLayer(LayerType, Position),
    RemoveLayer(usize),
    ChangeActivation { layer: usize, new_act: Activation },
    AdjustWidth { layer: usize, new_width: usize },
}
```

### 程序变换引擎（这只有同像性才能做到）

```lisp
;; 在训练的循环中，模型读取自己的损失曲线并变换代码

( self-improving-train [model-spec data]
  (loop [best-model model-spec
         best-loss infinity
         generation 0]

    (let [current-model (train best-model data {:epochs 10})
          ;; 模型"检查"自己的程序结构
          current-code (model->code current-model)   ;; Sexp
          complexity (estimate-complexity current-code)
          loss (evaluate-loss current-model data)]

      (println "Gen" generation "Loss" loss "Complexity" complexity)

      (if (< loss best-loss)
        (do
          ;; 记录改进
          (println "Found improvement!")

          ;; 尝试自动化简- — 宏级别的 dead code elimination
          (let [simplified (macroexpand-all
                             (simplify-model current-code))
                simplified-model (code->model simplified)
                simplified-loss (evaluate-loss simplified-model data)]

            (if (< simplified-loss (* loss 1.01))  ;; 允许 1% 退化
              (recur simplified-model simplified-loss (inc generation))
              (recur current-model loss (inc generation)))))
        (do
          ;; 尝试架构变异
          (let [variants (generate-variants
                           current-code
                           [:add-layer :change-activation
                            :adjust-width])
                best-variant (->> variants
                                  (map (fn [v]
                                         {:code v
                                          :loss (evaluate-loss
                                                  (code->model v) data)}))
                                  (sort-by :loss <)
                                  first)]
            (if (< (:loss best-variant) best-loss)
              (recur (code->model (:code best-variant))
                     (:loss best-variant)
                     (inc generation))
              (recur best-model best-loss (inc generation)))))))))
```

### 关键设计：Sexp Walker = 架构搜索器

```lisp
;; 在"正常"语言中，架构搜索需求额外的图 IR
;; 在 Lisp 中，模型架构就是 Sexp，遍历 Sexp 就是遍历架构

( generate-variants [model-spec strategies]
  (let [layers (get-layers model-spec)]   ;; Sexp 上的 list 操作
    (mapcat (fn [strategy]
              (case strategy
                :add-layer     (map (fn [pos]
                                     (insert-layer model-spec
                                                   (random-layer)
                                                   pos))
                                   (range (inc (length layers))))
                :remove-layer  (map (fn [i]
                                     (remove-layer model-spec i))
                                   (range (length layers)))
                :change-activation
                              (map (fn [i]
                                     (change-activation
                                       model-spec i
                                       (random-activation)))
                                   (range (length layers)))
                :adjust-width  (map (fn [i]
                                      (adjust-width
                                        model-spec i
                                        (* (nth layers i :width)
                                           (rand-range 0.5 2.0))))
                                    (range (length layers)))))
            strategies)))
```

### 实现路线

| Step | 内容 | 代码量 | 依赖 |
|------|------|--------|------|
| **P0** | Tensor 类型 + 基础运算 (add, mul, matmul) | 300 LOC | Zio core + 可选 `ndarray` crate |
| **P1** | 自动微分引擎 (AD, 基于 Wengert list) | 400 LOC | P0 |
| **P2** | `ModelDef` + 前向传播完成 | 200 LOC | P1 |
| **P3** | 训练循环 (SGD, Adam) | 300 LOC | P2 |
| **P4** | `macroexpand-all` 用于程序简化 | 100 LOC Zio | P0 |
| **P5** | `generate-variants` 变换引擎 | 200 LOC | Zio 宏系统 |
| **P6** | 自改进循环（架构搜索） | 150 LOC Zio | P4, P5 |
| **P7** | 模型导出 (ONNX / safetensors) | 200 LOC | P3 |

总计约 **1,700 LOC Rust + 450 LOC Zio**。P0-P1 是关键路径，也是纯数值工作。

### 和传统框架的对比

| 能力 | TensorFlow/PyTorch | Zio/AutoML |
|------|-------------------|-----------|
| 自动微分 | ✅ 计算图 | ✅ 类似 |
| GPU | ✅ CUDA | 通过 `ndarray` + `candle` |
| 架构搜索 | ❌ 独立库 (NAS) | ✅ **同一系统内** |
| 程序变换 | ❌ 需要外部代码分析 | ✅ **同像性内置** |
| 运行时修改 | ❌ 热加载有限 | ✅ `(def model ...)` 后直接 eval |
| DSAL 自定义 | ❌ 复杂 Python DSL | ✅ **宏** = DSL 生成器 |

---

## 附录 A：工程边界条件

### 性能目标

| 场景 | 当前 (v0.3, AST eval) | 目标 (v0.7, JIT) |
|------|----------------------|-----------------|
| 简单整数循环 (10^7 iter) | ~300 ms | ~50 ms |
| 函数调用开销 (per call) | ~50 ns | ~5 ns (native) |
| 启动时间 | <10 ms | <5 ms |
| 内存 (idle) | ~2 MB | ~1 MB |

### 跨平台测试矩阵

| 目标 | Rust target | 状态 |
|------|-------------|------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | ✅ 开发主力 |
| macOS ARM | `aarch64-apple-darwin` | ✅ CI |
| macOS x86_64 | `x86_64-apple-darwin` | ⏳ 需 CI |
| Windows | `x86_64-pc-windows-msvc` | ⏳ 待测试 |
| WASM | `wasm32-unknown-unknown` | Phase 7 |
| ARM Linux (RPi) | `aarch64-unknown-linux-gnu` | Phase 7 |

### 依赖策略

| 类别 | 允许 | 禁止 |
|------|------|------|
| 核心 crate | `im`, `thiserror` | 任何 async runtime, 任何网络库 |
| CLI crate | `clap`, `rustyline`, `reqwest` (blocking) | 大量 dev-deps |
| FFI crate | `libffi`, `dlopen2` | 分叉的系统绑定 |
| 测试 | `proptest`, `criterion` | — |

---

## 附录 B：术语表

| 术语 | 含义 |
|------|------|
| **同像性 (Homoiconicity)** | 代码和数据使用相同的表示（Sexp）。宏在"数据域"中操作，再求值为"代码域"。 |
| **Sexp** | Symbolic Expression。Zio 的 AST。一切代码和数据都编码为 Sexp。 |
| **Tail-call optimization (TCO)** | 尾调用位置不创建新栈帧，等效于 `goto`。在 Zio 中当前只对 `loop/recur` 生效。 |
| **Special Form** | 不被标准 eval 规则（先求值参数，再应用函数）处理的语法形式。如 `if`, `def`, `fn`。 |
| **EvalEngine trait** | Zio 运行时对"如何管理状态"的抽象。每个 EvalEngine 实现都是完全隔离的 eval 上下文。 |
| **EvalContext** | 默认的 EvalEngine 实现。持有环境、模块注册表、加载器。 |
| **Persistent data structure** | 不可变 + 结构共享的数据结构。修改返回新"版本"，旧版本仍有效。用于 List/Vector/Map。 |

---

## 附录 C：贡献者入门

```bash
# 构建
cargo build
cargo test          # 103 tests + doc tests
cargo clippy        # 0 warnings

# 项目结构
zio/
├── core/           # 运行时核心
│   └── src/
│       ├── eval.rs     # eval/apply 循环
│       ├── sexp.rs     # 语法树
│       ├── value.rs    # 运行时值
│       ├── env.rs      # 词法环境
│       ├── context.rs  # EvalEngine trait + EvalContext
│       ├── builtins.rs # 内置函数注册
│       ├── macros.rs   # 宏展开引擎
│       ├── special/    # 特殊形式分发
│       └── module.rs   # 模块系统
├── reader/         # tokenizer + parser
│   └── src/
│       ├── lexer.rs    # 词法分析
│       └── reader.rs   # 语法分析 (Sexp 构造)
├── cli/            # 命令行 REPL
│   └── src/
│       └── main.rs
└── docs/           # 文档
```