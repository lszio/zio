# Zio Architecture — v0.2 生产级语言设计

> 基于当前实现 (v0.1, ~2,700 LOC Rust, 62 tests) 和 README 愿景，
> 制定从 REPL toy 到生产级语言的分层架构方案。

---

## 一、当前状态评估

### 已有的 (可保留的资产)

| 组件 | 状态 | 说明 |
|------|------|------|
| reader.rs | ✅ 可用 | 基于栈的解析，支持 quote reader macro |
| sexp.rs | ✅ 可用 | 干净的数据结构，Sexp 与 Value 分离 |
| eval.rs | ✅ 可用 | 基本 eval/apply 循环，尾递归支持 |
| env.rs | ✅ 可用 | 词法作用域链，闭包正确 |
| special.rs | ⚠️ 需重构 | 膨胀的上帝模块，700行 |
| builtins.rs | ⚠️ 需扩展 | 仅有整数算术，缺少大量基础函数 |
| macros.rs | ⚠️ 需增强 | 基本宏可用，但无 hygiene |
| value.rs | ⚠️ 需扩展 | 缺少类型系统、元数据、GC 支持 |
| error.rs | ⚠️ 需增强 | 缺少源码位置信息 |
| main.rs | ❌ 需重写 | 仅 REPL，无脚本、无模块 |

### 缺失的 (从零开始)

- 模块系统 (load/require/provide)
- 源码位置追踪 (Span)
- 类型系统 (渐进式类型)
- 协议与多分派
- 模式匹配
- 标准库 (数学、字符串、文件、集合)
- 条件系统 (Condition/Restart)
- Actor 并发模型
- 序列化/反序列化
- FFI 外部函数接口
- JIT/Native 编译
- WebAssembly 支持

---

## 二、分层架构设计

```
┌──────────────────────────────────────────────────┐
│                   Application                     │
│  CLI · TUI · WASM Host · Embedded · Agent SDK    │
├──────────────────────────────────────────────────┤
│                  Standard Library                  │
│  Math · String · File · IO · Data · Net · Crypto  │
│  Collection · Concurrency · Serialization · FFI   │
├──────────────────────────────────────────────────┤
│                   Runtime (VIRT)                   │
│  Value · Env · GC · Module · Type · Condition    │
│  Actor · Scheduler · Channel · Task · IO Runtime  │
├──────────────────────────────────────────────────┤
│                   Compiler (ZIR)                   │
│  ZIR IR · Optimizer · Codegen (VM/Native/WASM)   │
│  Macros (Reader/Syntax/Meta) · Pattern Matching   │
├──────────────────────────────────────────────────┤
│                 Frontend (Reader)                  │
│  Reader · Parser · Sexp · Span · Unicode          │
└──────────────────────────────────────────────────┘
```

### 分层原则

1. **单向依赖**：上层依赖下层，禁止反向依赖
2. **接口隔离**：层间通过 trait 通讯，核心不依赖具体实现
3. **模块化**：每一层内部按语义领域拆分为独立 crate
4. **渐进式**：每一层都可以独立测试和交付

---

## 三、Crate 架构 (Rust workspace)

### workspace 布局

```
zio/
├── Cargo.workspace.toml
├── src/                    # 入口 crate (cli + repl)
│   ├── main.rs
│   └── bin/
│       ├── zio.rs          # CLI: zio run, zio repl, zio compile
│       └── ziod.rs         # Daemon: zio language server
│
├── core/                   # 核心运行时 (virgil crate) [核心]
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── value.rs        # Value enum + 类型系统
│       ├── env.rs          # 环境链 + 模块命名空间
│       ├── sexp.rs         # 语法树 (保持不变)
│       ├── span.rs         # 源码位置
│       ├── error.rs        # 错误系统 + 条件系统
│       ├── gc.rs           # 垃圾回收 (可选：引用计数/追踪式)
│       ├── module.rs       # 模块系统
│       ├── type.rs         # 渐进式类型
│       └── condition.rs    # Condition/Restart 系统
│
├── reader/                 # 解析器 (read crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── reader.rs       # S-expression parser
│       ├── lexer.rs        # Tokenizer (从 reader 分离)
│       └── unicode.rs      # Unicode 支持
│
├── compiler/               # 编译器 (zir crate)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── expand.rs       # 宏展开
│       ├── analyze.rs      # 类型推断 / 名称解析
│       ├── ir.rs           # ZIR 中间表示
│       ├── optimize.rs     # 优化 pass
│       └── codegen/
│           ├── mod.rs
│           ├── vm.rs       # 字节码生成 (AST Walker)
│           ├── native.rs   # Native codegen (future)
│           └── wasm.rs     # WASM codegen (future)
│
├── vm/                     # 虚拟机 (vmi crate) [可选的]
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── bytecode.rs     # 字节码定义
│       ├── interpreter.rs  # 字节码解释器
│       └── jit.rs          # JIT 编译 (future)
│
├── std/                    # 标准库 (stdlib crate)
│   ├── Cargo.toml
│   ├── src/
│   │   ├── lib.rs          # 桥接到 Rust 原生实现
│   │   └── *.rs            # 每个标准库模块一个文件
│   └── lib/                # Zio 语言本身编写的标准库
│       ├── math.zio
│       ├── string.zio
│       ├── file.zio
│       └── ...
│
├── ffi/                    # 外部函数接口 (ffi crate) [future]
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── c-ffi.rs        # C ABI 调用
│       └── wasm-host.rs    # WASM 宿主调用
│
└── docs/                   # 设计文档
    ├── architecture.md
    ├── spec.md             # 语言规范
    ├── type-system.md
    └── roadmap.md
```

### 依赖关系图

```
zio (CLI binary)
├── reader    (解析：字符串 → Sexp + Span)
├── compiler  (编译：Sexp → ZIR → 字节码)
│   ├── core  (类型、错误、模块)
│   └── reader
├── vm        (执行：字节码解释器) [可选层]
│   └── core
├── std       (标准库原生实现)
│   └── core
└── core      (运行时：Value, Env, GC, Module,...)
```

注意：`compiler` 和 `vm` 层是可选的插入层。MVP 阶段可以直接走 `reader → core (eval) → std` 的 AST 解释路径。`compiler` 和 `vm` 可以在后续阶段逐步加入，不影响上层 API。

---

## 四、各层详细设计

### 4.1 core — 核心运行时

**价值主张**：所有代码最终依赖的核心，不可变数据结构 + 词法作用域 + 模块系统。

#### Value 系统 (扩展现有)

```rust
pub enum Value {
    // 字面量
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(Symbol),      // 带命名空间的符号 foo/bar
    Keyword(String),

    // 集合 (持久化)
    List(Vector<Value>),
    Vector(Vector<Value>),
    Map(HashMap<Value, Value>),
    Set(HashSet<Value>),  // 新增

    // 可调用类型
    Function(Function),
    NativeFunction(NativeFn),
    Macro(Macro),
    GenericFunction(GenericFn), // 多分派

    // 反射类型
    Type(Type),           // 类型本身是一等值
    Protocol(Protocol),
    Record(Record),       // 结构化数据
    Module(Module),

    // 宿主互操作
    Host(HostValue),      // Rust 对象包装
}
```

#### Symbol 系统

```rust
pub struct Symbol {
    pub name: String,           // 短名
    pub namespace: Option<String>, // 命名空间
}
```

符号支持命名空间解析，在环境查找时处理 `foo/bar` 形式。

#### 源码位置 (新增)

```rust
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
    pub line: usize,
    pub col: usize,
    pub source: SourceId,
}

pub struct SourceId(pub usize);  // 指向 SourceMap 中的文件
pub struct SourceMap { ... }     // 全局源文件注册表
```

#### 模块系统 (新增)

```rust
pub struct Module {
    pub name: Vec<String>,        // 分层模块名 [zio, math]
    pub exports: HashMap<String, Value>,
    pub imports: Vec<(ModuleRef, Vec<String>)>,
    pub doc: Option<String>,
    pub source: SourceId,
}

impl Module {
    pub fn load(path: &str) -> Result<Module>;
    pub fn find(name: &[String]) -> Result<ModuleRef>;
    pub fn import(&self, symbols: &[String]) -> HashMap<String, Value>;
}
```

模块解析策略：当前目录 → `ZIO_PATH` 环境变量 → 内置标准库。

#### 条件系统 (新增)

```rust
pub enum Condition {
    Error { message: String, span: Option<Span>, source: ConditionSource },
    Warning { message: String, ... },
    Restart { name: Symbol, handler: RestartHandler },
}

pub struct RestartHandler(Box<dyn Fn(&[Value]) -> Result<Value, EvalError>>);
```

`(with-restart (restart-name handler-fn) body)` + `(signal condition)` +
`(handle-case (condition-type handler) body)`。

#### GC (初始设计)

**初始阶段**：`Arc` 引用计数 (当前 `im` crate 已使用)。先不做追踪式 GC，等到有性能分析数据再说。

```rust
// 当前方式 (够用)
pub struct Gc<T>(Arc<T>);

// 未来选项 (当需要突破性能瓶颈时)
// pub struct Gc<T>(GcRef<T>); // 基于 mmap 的分代 GC
```

### 4.2 reader — 解析器层

#### 模块拆分：Lexer → Reader

```rust
// lexer.rs — 将字符串转换为 Token 流
pub enum Token {
    LParen, RParen,
    LBracket, RBracket,
    LBrace, RBrace,
    Quote, QuasiQuote, Unquote, Splice,  // `, ~, ~@
    Deref, Meta, Dispatch,              // @, ^, #
    Atom(TokenKind, Span),
}

pub fn lex(input: &str, source: SourceId) -> Result<Vec<Token>>;
```

```rust
// reader.rs — 将 Token 流转换为 Sexp
pub fn read(tokens: &[Token]) -> Result<(Sexp, Span)>;
```

**新增的 reader macro 支持**：
- `#(...)` — dispatch macro (fn literal, set literal, regex)
- `` `x `` — quasiquote
- `~x` — unquote
- `~@x` — unquote splicing
- `#_` — discard (注释掉下一个 form)

### 4.3 compiler/ — 编译器层 (ZIR)

#### 当前 (v0.1) → 生产级路径

```
v0.1: reader → AST eval (当前)
v0.2: reader → [macro expand] → AST eval (更完整的 eval)
v0.3: reader → macro expand → ZIR → VM bytecode
v0.4: reader → macro expand → ZIR → optimize → bytecode
v0.5: reader → macro expand → ZIR → optimize → native/WASM
```

**ZIR 设计 (初始设计)**：

```rust
pub enum ZirInstr {
    // 字面量
    Const(Value),

    // 符号
    Lookup(Symbol),
    Def(Symbol),

    // 控制流
    Call(ZirExpr, Vec<ZirExpr>),    // 函数调用
    If(ZirExpr, ZirExpr, ZirExpr),  // 条件
    Let(Vec<(Symbol, ZirExpr)>, ZirExpr),
    Loop(Vec<(Symbol, ZirExpr)>, ZirExpr),
    Recur(Vec<ZirExpr>),

    // 模块
    Module(Vec<ZirStmt>),
    Import(ModuleRef, Vec<String>),
    Export(Vec<(Symbol, ZirExpr)>),

    // 元编程
    MacroExpand(ZirExpr),
    EvalAtCompile(ZirExpr),
}
```

### 4.4 std/ — 标准库

标准库分为两层：

1. **原生层 (Rust)**：性能敏感的部分直接通过 `NativeFn` 实现
2. **Zio 层 (.zio)**：能用语言本身实现的就用语言本身写

**第一波标准库**：

```
math.zio        — +, -, *, /, abs, sqrt, sin, cos, pow, gcd, lcm
string.zio      — join, split, contains, replace, upper, lower, trim, format
file.zio        — read, write, open, close, exists, delete, list-dir
io.zio          — print, println, read-line, with-open
seq.zio         — map, filter, reduce, take, drop, sort, group-by
coll.zio        — conj, assoc, dissoc, get, keys, vals, merge
conv.zio        — int, float, str, keyword, symbol, to-string
test.zio        — deftest, assert, assert=, assert-error
```

### 4.5 zio (CLI Binary)

**命令行接口**：

```
zio repl                 # REPL (默认，同 zio)
zio run file.zio         # 执行脚本
zio compile file.zio     # 编译为字节码
zio check file.zio       # 类型检查/语法检查
zio doc module-name      # 生成文档
zio init                 # 初始化项目
zio test                 # 运行测试
zio lsp                  # Language Server Protocol
zio help                 # 帮助
```

## 五、交付路线图

### Phase 1 — 架构重组 (当前 → 2周)

**目标**：重构现有代码为分层架构，不增加新功能。

| 任务 | 说明 | 影响 |
|------|------|------|
| 1.1 | 创建 workspace 结构，拆分 core/reader 子 crates | 构建系统 |
| 1.2 | 引入 Span 类型到 Sexp 和错误 | reader, eval |
| 1.3 | 拆分 special.rs → special/ 模块 | 可维护性 |
| 1.4 | 从 reader.rs 分离 lexer.rs | 可测试性 |
| 1.5 | 添加 SourceMap | 错误报告 |
| 1.6 | 引入 Symbol 类型 (带命名空间) | 语言核心 |
| 1.7 | 添加 Numeric trait 支持浮点数算术 | 完整性 |
| 1.8 | 所有测试通过 + 新增 Span 测试 | 质量 |

**交付物**：可运行 `zio repl`，所有旧功能+浮点数+更好的错误信息。

### Phase 2 — 模块系统 + 脚本 (2-4周)

**目标**：`zio run file.zio` 可用，`(load)`/`(require)` 可用。

| 任务 | 说明 | 影响 |
|------|------|------|
| 2.1 | `Module` 结构体 + 解析器 | core |
| 2.2 | `(load path)` 特殊形式 | reader |
| 2.3 | `(module name ...)` 特殊形式 | reader |
| 2.4 | `(require :module-name [:symbols ...])` | reader |
| 2.5 | 模块路径解析 (ZIO_PATH, 当前目录) | core |
| 2.6 | `zio run` CLI 命令 | binary |
| 2.7 | 脚本 shebang 支持 | binary |
| 2.8 | 循环引用检测 + 错误 | core |

**交付物**：多文件项目可执行，模块间符号隔离。

### Phase 3 — 标准库 (4-6周)

**目标**：可用且文档完善的标准库。

| 任务 | 说明 |
|------|------|
| 3.1 | math.zio — 完整数学库 (浮点数+常见函数) |
| 3.2 | string.zio — 字符串操作 |
| 3.3 | seq.zio — 序列操作 (map/filter/reduce/sort) |
| 3.4 | file.zio — 文件 I/O |
| 3.5 | io.zio — 控制台 I/O + 格式化 |
| 3.6 | test.zio — 测试框架 |
| 3.7 | 原生实现中浮点数算术的提升支持 |
| 3.8 | 标准库文档 |

**交付物**：`(use math)` 可用，可写非平凡程序。

### Phase 4 — 进阶语言特性 (6-10周)

**目标**：模式匹配、协议、多分派、条件系统。

| 任务 | 说明 |
|------|------|
| 4.1 | 模式匹配 `(match x ...)` |
| 4.2 | 解构绑定 (let/loop 支持模式) |
| 4.3 | `(defprotocol name ...)` + `(defrecord name ...)` |
| 4.4 | 多分派 `(defmulti name dispatch-fn)` + `(defmethod name type body)` |
| 4.5 | 条件系统: signal / handle-case / with-restart |
| 4.6 | `(deftype name fields)` 用户自定义类型 |

**交付物**：语言表达能力大幅提升，接近 Clojure/Common Lisp 水平。

### Phase 5 — 编译 + VM (10-16周)

**目标**：ZIR 中间表示 + 字节码 VM。

| 任务 | 说明 |
|------|------|
| 5.1 | ZIR 定义 + AST→ZIR 编译 |
| 5.2 | 宏展开集成到编译流程 |
| 5.3 | 字节码定义 + 简单解释器 |
| 5.4 | 常量折叠 + 简单优化 |
| 5.5 | eval 层可选切换到 VM 后端 |
| 5.6 | 性能基准测试 + 对比 |

**交付物**：可选 VM 后端，性能提升 2-5x。

### Phase 6 — 并发 + FFI (16-24周)

**目标**：Actor 模型 + C FFI。

| 任务 | 说明 |
|------|------|
| 6.1 | `(spawn fn)` — actor 创建 |
| 6.2 | `(channel n)` — 通道 |
| 6.3 | `(send ch val)` / `(recv ch)` |
| 6.4 | `(task body)` — structured concurrency |
| 6.5 | `(ffi lib-name fn-name arg-types ret-type)` |
| 6.6 | Rust → Zio 插件系统 |
| 6.7 | 并发基准测试 |

**交付物**：可编写并发程序，可调用 C 库。

### Phase 7 — 生产化 (24周+)

- 语言服务器 (LSP)
- 调试器 (DAP)
- 包管理器
- Native 编译 (LLVM)
- WebAssembly 目标
- 性能优化
- 文档 + 网站
- 社区

---

## 六、架构原则与约束

### 1. 向后兼容
- Phase 1 重构后所有现有 Zio 代码 (当前约 60 行测试代码) 必须能运行
- 新版本读旧 `.zio` 文件需要兼容
- `zio repl` 永远可用

### 2. 渐进式复杂度
- 简单的模块使用 `(load "file.zio")` 即可工作
- 复杂项目可以显式 `(module ...)` + `(require ...)`
- 编译/类型检查是可选的，不是必须的

### 3. 性能基准
- 当前 AST 解释器：~基准 (不优化)
- Phase 5 VM 目标：AST 的 2-5x
- Phase 7 Native 目标：VM 的 10-50x
- 目标：动态语言的合理性能 (类似 Python/Node.js 量级)

### 4. 嵌入友好
- `core` crate 要保持最小依赖 (当前 4 个 crate)
- Zio 值系统可以从 Rust 端直接调用
- 减少 `unsafe` (目标：零 unsafe，除非 GC/FFI)
- 编译时间目标：`cargo test < 2s` 增量

### 5. 错误处理
- 所有错误必须包含 Span
- 条件系统是可选的 (用 try/catch 也能写)
- 错误信息要提供源代码上下文

---

## 七、当前代码重构指南 (Phase 1 详细任务)

### 任务 1.1 — 创建 workspace

```toml
[workspace]
members = ["core", "reader", "cli"]

[workspace.package]
version = "0.2.0"
edition = "2024"
```

### 任务 1.2 — Span 集成

```rust
// 新增 src/core/span.rs
pub struct Span { ... }
pub struct SourceMap { ... }

// Sexp 增加 Span
pub enum Sexp {
    Nil { span: Span },   // 每个变体携带位置
    Boolean(bool, Span),
    // ...
}
```

可以使用 `SexpKind` + `Spanned<SexpKind>` 分离来减少重复。

### 任务 1.3 — special.rs 拆分

```
src/core/special/
├── mod.rs         # eval_special_form() 分发函数
├── bindings.rs    # def, defn, defmacro, fn
├── control.rs     # if, cond, and, or, do
├── letloop.rs     # let, let*, loop, recur
├── data.rs        # quote
└── module.rs      # module, require, load (Phase 2)
```

### 任务 1.6 — Symbol 类型

```rust
pub struct Symbol {
    name: String,
    ns: Option<String>,
}

// 解析 "foo/bar" → Symbol { name: "bar", ns: Some("foo") }
impl From<&str> for Symbol { ... }
```

### 任务 1.7 — 浮点数算术

```rust
// 在算术运算中自动提升
fn numeric_op(args, op_int, op_float) -> Value {
    let any_float = args.iter().any(|v| matches!(v, Value::Float(_)));
    if any_float { op_float(args) } else { op_int(args) }
}
```

---

## 八、多少算够？MVP 里程碑定义

### v0.2 — "可编程" (Phase 1 + 2)

- `zio run hello.zio` 工作
- `(load)` / `(module)` / `(require)` 可用
- 多文件项目
- 含源码位置的错误
- 浮点数算术
- 所有旧功能保留

### v0.3 — "可用" (Phase 1 + 2 + 3)

- 标准库覆盖常见需求
- `zio test` 可用
- 可写非平凡程序 (Web 服务器、文本处理)

### v1.0 — "生产级" (Phase 1-4)

- 模式匹配
- 协议 + 多分派
- 条件系统
- 渐进式类型
- 文档生成
- 语言服务器
- 包管理器