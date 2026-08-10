# Zio 架构决策记录（ADRs）

> 轻量级架构决策日志。每个 ADR 记录一个关键决策的背景、决策、理由、代价、备选方案。
>
> ADR 的“已采纳”只表示设计决策成立，不表示其所有下游能力都已实现。
> 当前实现状态以[特性矩阵](feature-matrix.md)为准；其中 ZIR/VM/JIT、
> persistent collection library、Datalog 和应用能力均为 Planned。

---

## ADR-001: Sexp 与 Value 分离

**状态**: ✅ 已实现（v0.1 → v0.2）

### 背景

Lisp 的同像性（homoiconicity）要求代码本身就是数据结构。宏接受代码（Sexp），变换代码（Sexp），返回代码（Sexp）。同时，eval 将代码求值为运行时的值。两个域需要的类型不同。

### 决策

```rust
// 代码域（语法树）
pub enum Sexp {
    Nil, Boolean(bool), Integer(i64), Float(f64),
    String(String), Symbol(String), Keyword(String),
    List(Vector<Sexp>), Vector(Vector<Sexp>), Map(HashMap<Sexp, Sexp>),
}

// 运行时域（值）
pub enum Value {
    Nil, Boolean(bool), Integer(i64), Float(f64),
    String(String), Symbol(String), Keyword(String),
    List(Vector<Value>), Vector(Vector<Value>), Map(HashMap<Value, Value>),
    Function(Arc<Function>), NativeFunction(NativeFn), Macro(Macro),
    // ZOS Phase 1:
    Object(Box<dyn ZosObject>),
}
```

Sexp 和 Value 之间通过 `From<Sexp> for Value` / `value_to_sexp` 转换。

### 理由

- 宏需要操作未求值的代码（符号需要保持为符号，而不是去环境查找）
- `quote` 需要将 Sexp 域直接映射到 Value 域
- Rust 的类型系统保证在两个域之间不会混淆

### 代价

- 两个类似的枚举
- 宏展开后需要 `value_to_sexp` 转换（O(n) 克隆）
- 偶尔需要手动 `into_value()` / `value_to_sexp()`

### 备选方案

- **单一类型**：一个枚举包含所有可能性。代价：`quote` 需要特殊的「暂停求值」标记，宏实现变得更复杂
- **使用 Rust trait 统一接口**：尝试用 trait 抽象。代价：向下转型的开销，失去 match 穷举检查

---

## ADR-002: EvalContext + EvalEngine trait

**状态**: ✅ 已实现（v0.2 重构）

### 背景

Zio 早期有 4 个 `thread_local!` 全局变量（MODULES、LOADING_STACK、REQUIRE_LOADER、隐式 GLOBAL_ENV）。这导致测试不可隔离、无法嵌入多个解释器实例、无法 mock。

### 决策

所有可变状态聚合到 `EvalContext` struct：

```rust
pub struct EvalContext {
    pub env: Arc<Env>,
    pub modules: RefCell<ModuleRegistry>,
    pub loader: RefCell<Option<Box<ModuleLoader>>>,
}
```

通过 `&dyn EvalEngine` trait 注入到特殊形式和 builtin 函数（随后拆分为 `EvalRuntime` + `ZosRuntime` + `ModuleRegistry`）。

### 理由

- 可测试性：mock engine 用于特殊形式测试
- 可嵌入性：多个独立 EvalContext 实例
- 安全性：0 thread_local 全局变量
- 打破循环依赖：trait 放在独立模块，不依赖 special/macros

### 代价

- 每个 NativeFn 签名多一个 `engine: &dyn EvalEngine` 参数
- `RefCell` 运行时借用检查（单线程下可忽略）
- `!Send` 约束——多线程时需要改为 Mutex

### 备选方案

- **Actor 模型**：将 EvalContext 封装在 actor 中，通过消息传递访问。代价：不适合同步 eval 调用
- **Generator/Coroutine**：用生成器管理状态。代价：Rust 生态不成熟

---

## ADR-003: Core 值的结构共享（im crate）

**状态**: ✅ 已实现

此 ADR 只覆盖 core 中 `im` 类型的结构共享，不表示
`lib/zio/persistent.zio` collection API 已实现；该 library 在
[特性矩阵](feature-matrix.md)中仍为 Planned。

### 背景

Lisp 的默认数据结构应该是不可变的。函数式更新需要结构共享来避免 O(n) 克隆。

### 决策

使用 `im::Vector`（RRB 树）、`im::HashMap`（HAMT）作为 List、Vector、Map 的默认实现。

### 理由

- 结构共享使 cons/car/cdr 操作 O(log n)，对大多数集合可接受
- 不可变性防止 eval 中的意外副作用
- `im` crate 稳定、纯 Rust、无 unsafe

### 代价

- 比 `Vec` / `HashMap` 慢 2-5x（对小集合不明显）
- `im::HashMap` 的迭代顺序不确定
- `im::Vector` 索引访问 O(log n)，不是 O(1)

### 备选方案

- **Rust std Vec/HashMap**：可变、O(1) 索引。代价：失去不可变性保证
- **sled / btree**：更持久的存储。代价：不适合内存操作
- **自制持久化数据结构**：不必要——im crate 已充分成熟

---

## ADR-004: Span 嵌入 Sexp

**状态**: 📋 待实现（Phase 1）

### 背景

Zio 当前只在顶层表达式中有源位置信息。错误消息形式：`"error: symbol not found: foo"` 没有文件/行/列。Reader 在 tokenize 时已有 Token 的位置信息，但未传播到 Sexp 树。

### 决策

将 `span: Option<Span>` 嵌入每个 Sexp 变体：

```rust
pub struct Span {
    pub start: BytePos,
    pub end: BytePos,
    pub line: usize,
    pub col: usize,
    pub source: SourceId,
}

pub enum Sexp {
    Nil { span: Option<Span> },
    Boolean(bool) { span: Option<Span> },
    // ... 每个变体携带 span
}
```

或者（代码更少的方式）将 Sexp 包装：

```rust
pub struct Spanned<T> {
    pub span: Option<Span>,
    pub inner: T,
}

pub type Sexp = Spanned<SexpKind>;

pub enum SexpKind {
    Nil,
    Boolean(bool),
    // ...
}
```

Reader 在 parse 时填充 Span。Error 显示时格式化。

### 理由

- 没有源位置的错误消息对用户不可用
- Reader 的 tokenizer 已经有位置信息——丢弃它是浪费信息
- 错误消息从 `"error: symbol not found: foo"` 变为带行号的可用消息

### 代价

- 每个 Sexp 节点 +16-32 bytes（取决于 Span 内联还是 Option 包装）
- Reader 需要传递位置信息（已有）
- 所有 eval 错误需要携带 Span（已有错误类型）

### 备选方案

- **SpannedSexp 包装类型**（当前方式）：特殊形式的错误处理需要额外的 `if let` 解包
- **单独的 SourceMap**：错误时从表查找。代价：查表开销

---

## ADR-005: EvalEngine 拆分为子 trait

**状态**: 📋 待实现（Phase 1，ZOS Phase 1 之前）

### 背景

当前 `EvalEngine` trait 有 8 个方法，混合了求值（`eval_expr`、`env`）、模块操作（`register_module`、`find_module` 等）、ZOS 操作（未来需要）。对于嵌入场景（不想做模块加载），需要 mock 不需要的方法。

### 决策

拆分为 3 个独立的 trait：

```rust
/// 最小编译/求值能力。不含模块和 ZOS。
pub trait EvalRuntime {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
}

/// ZOS 运行时能力。
pub trait ZosRuntime: EvalRuntime {
    fn class_of(&self, val: &Value) -> ClassRef;
    fn find_and_apply_gf(&self, gf: &GenericFunction, args: &[Value])
        -> Result<TailResult, EvalError>;
    fn signal_condition(&self, c: Condition)
        -> Result<Option<TailResult>, EvalError>;
    fn invoke_restart(&self, name: &Symbol, args: &[Value])
        -> Result<TailResult, EvalError>;
}

/// 模块注册表。
pub trait ModuleRegistry {
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(&self, ...) -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
}
```

### 理由

- 嵌入场景只需要 `EvalRuntime`，不需要模块和 ZOS
- ZOS 场景需要 `EvalRuntime + ZosRuntime`，不一定需要模块
- CLI 场景实现全部三个 trait
- Future 可以添加 `IoHost` trait 而不污染求值接口

### 代价

- 多 trait 边界：某些函数需要 `where T: EvalRuntime + ZosRuntime`
- 重构旧代码：所有 `&dyn EvalEngine` 使用点需要更新

---

## ADR-006: Value::Object 作为 ZOS 入口

**状态**: 📋 待实现（ZOS Phase 1）

### 背景

当前 `Value` 是 Rust 的封闭枚举。ZOS 需要支持 Heap Object（Instance、Class、GF、Method、Package），这些类型在编译期无法全部预知（MOP 扩展允许创建新的 Object 类型）。

### 决策

新增 `Value::Object(Box<dyn ZosObject>)` 变体：

```rust
pub trait ZosObject: std::fmt::Debug + Send + Sync {
    fn header(&self) -> &ObjectHeader;
    fn as_any(&self) -> &dyn std::any::Any;
}

pub struct ObjectHeader {
    pub class: ClassRef,
    pub flags: ObjectFlags,
    pub identity: Option<u64>,
}

pub enum Value {
    // 现有 Immediate 类型保持不变
    Nil, Boolean(bool), Integer(i64), Float(f64),
    String(String), Symbol(String), Keyword(String),
    List(Vector<Value>), Vector(Vector<Value>), Map(HashMap<Value, Value>),
    // 现有可调用类型（逐步迁移到 ZOS Object）
    Function(Arc<Function>), NativeFunction(NativeFn), Macro(Macro),
    // ZOS 入口
    Object(Box<dyn ZosObject>),
}
```

### 理由

- Immediate 类型保持内联（性能不损失）
- Heap Object 通过 trait object 获得开放扩展能力
- `as_any()` 允许向下转型到具体类型

### 代价

- `Object` 变体的堆分配和虚函数调用开销
- `apply()` 需要 `downcast_ref` 来区分 GF 和其他可调用对象
- Debug/Display 需要 vtable

### 备选方案

- **Value = 统一 GcHandle**：所有值通过 GC 指针访问。代价：Integer 也要装箱
- **Value = enum + 大量预定义变体**：每个新 Object 类型加一个变体。代价：MOP 无法扩展

---

## ADR-007: Condition System 分阶段实现

**状态**: 📋 待实现（ZOS Phase 1 简化版，Phase 2+ 完整版）

### 背景

Common Lisp 的 Condition System 依赖非局部控制流（`invoke-restart` 可以穿越多个栈帧）。在 Rust 的线性控制流（`Result` 传播）中完全实现这个模型需要复杂的基础设施（panic/unwind 或显式 continuation）。

### 决策

分两阶段实现：

**Phase 1**: 简化版，基于 Result 传播 + 局部 restart

```rust
pub enum TailResult {
    Value(Value),
    Recur(Arc<Env>),
    Restart(Symbol, Vec<Value>),  // 栈传播的 restart
}
```

支持 `(try body (catch Type handler))` 和 `(with-restart (name fn) body)`。Restart 不跨越函数调用栈——仅在同一个函数内或通过 `Result` 传播。

**Phase 2+**: 完整版，基于 handler_stack + 跨栈 invoke_restart

```rust
handler_stack: RefCell<Vec<HandlerEntry>>,

struct HandlerEntry {
    condition_type: TypeSpec,
    handler: Box<dyn Fn(&Condition, &dyn ZosRuntime) -> HandlerResult>,
    restarts: Vec<Restart>,
}
```

### 理由

- Phase 1 满足 80% 的需求（try/catch + 本地 restart），不需要复杂基础设施
- Phase 2 只在验证了确实需要完整 CL 风格条件系统后才实现
- 可以实际使用 Phase 1 来积累经验，指导 Phase 2 设计

### 代价

- Phase 1 的 restart 不穿透函数调用栈——某些 CL 习惯用法的移植不直接
- Phase 1 → Phase 2 可能有 API break（如果 Phase 1 设计不当）

---

## ADR-008: 4-参数 Dispatch Cache

**状态**: 📋 待实现（ZOS Phase 1）

### 背景

Generic Function 的多分派在 AST 解释器中已经是性能敏感路径。每次 GF 调用都需要参数类型收集、方法扫描、CPL 排序。对于常见情况（1-4 个参数），缓存可以大幅提升性能。

### 决策

使用前 4 个参数的类型 ID 元组作为哈希键：

```rust
pub struct DispatchCache {
    cache: HashMap<(u64, u64, u64, u64), MethodList>,
}
```

- 键：`(type_id_of_arg1, type_id_of_arg2, type_id_of_arg3, type_id_of_arg4)`
- 未命中时：完整扫描 → 按 CPL 排序 → 缓存
- 失效：`defmethod`、`defclass` 重定义时清除整个缓存

### 理由

- 4 参数覆盖了绝大多数 GF 调用（`draw rect` 1 参数, `collide car wall` 2 参数, `render mesh camera light` 3 参数）
- 哈希键是 4 个 u64——在栈上分配，堆无分配
- 缓存命中时 O(1)，不命中时 O(m·n) 但一次

### 代价

- 6+ 参数的分派缓存不命中（罕见情况）
- 缓存清除开销：`defmethod` 时全清除

### 备选方案

- **无缓存**：每次 GF 调用都完整扫描。代价：性能差 10-100x
- **单参数缓存**：只缓存第一个参数。代价：多分派场景不工作
- **Trie 缓存**：按参数类型逐层索引。代价：实现复杂度高

---

## ADR-009: 核心最小，其余是库

**状态**: ✅ 已采纳（v0.3 规范阶段）

### 背景

Zio 的路线图中规划了 Datalog 数据库、自学习模型框架、Agent 框架、
Clojure 风格集合等特性。如果全部进入 core，核心将膨胀到不可维护。

### 决策

明确划定核心与库的边界：

**Zio 核心包含**：
- Sexp/Value/Eval eval 循环
- ZOS（Class/GF/Method/MOP/Package/Condition）
- 最小编程原语（`+` `-` `cons` `car` `cdr` `map` `filter` `reduce` 等）

**Zio 核心不包含**（全部作为扩展库）：
- Datalog 查询（→ `zio-datalog`）
- Agent 编排（→ `zio-agent`）
- 自学习模型（→ `zio-ai`）
- Clojure 风格集合（→ `zio-persistent`）
- Entity/Protocol（→ `zio-entity` / `zio-protocol`）
- Actor 并发（→ `zio-actor`）
- LLM API（→ `zio-llm`）

以上库名表达架构边界，不是交付声明；相关能力当前均为
[Planned](feature-matrix.md)。

### 理由

- 核心稳定性取决于它不做多少事
- 库可以独立迭代、版本、替换
- 用户按需加载——嵌入场景不需要 Agent 框架
- 社区可以通过库贡献新功能而不需要修改核心

### 代价

- 用户需要额外安装和加载库（包管理器成为必需的基础设施）
- 库之间的版本兼容性需要保证
- 某些库可能需要核心的 API 扩展（通过 MOP）

---

## ADR-010: 卫生宏分阶段实现

**状态**: 📋 待实现（Phase 2）

### 背景

Zio 当前的 `defmacro` 是不卫生的——宏可以意外捕获调用者环境中的变量。卫生宏需要在宏展开时进行变量重命名，避免捕获冲突。

### 决策

分两阶段实现：

**Phase 2（当前）**：`syntax-rules` 模式匹配宏

```lisp
(defmacro swap! [a b]
  (syntax-rules ()
    ((swap! a b)
     (let [tmp a]
       (set! a b)
       (set! b tmp)))))
```

- 基于模式匹配（类似 Scheme R5RS）
- 自动重命名展开后的变量
- 功能有限——无法在宏展开时执行任意计算

**Phase 3+**：Explicit Renaming / Syntactic Closures

```lisp
(defmacro my-macro [form]
  (call-with-implicit-renaming
    (fn [form rename compare?]
      ...)))
```

显式重命名，功能完整，可以实现最复杂的宏模式。

### 理由

- `syntax-rules` 的正确性容易验证（模式匹配 + 自动重命名）
- 覆盖 80% 的卫生宏需求（简单的 DSL、控制结构）
- Explicit Renaming 只在需要高级宏时才使用
- 从简单到复杂的渐进路线跟 Racket/Scheme 社区的经验吻合

### 代价

- Phase 2 的 `syntax-rules` 不支持宏内计算
- 从 `syntax-rules` 到 Explicit Renaming 的迁移可能需要宏库更新

---

## ADR-011: IoHost 抽象

**状态**: 📋 待实现（Phase 1）

### 背景

Zio 当前的 builtin I/O（`println`、`read-line`）直接调用 Rust 的 `std::io`。这使测试无法捕获输出，WASM 嵌入需要重写 I/O，沙盒模式无法实现。

### 决策

新增 `IoHost` trait：

```rust
pub trait IoHost {
    fn read_file(&self, path: &str) -> Result<String, EvalError>;
    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError>;
    fn stdout(&self, msg: &str);
    fn stderr(&self, msg: &str);
    fn env_var(&self, name: &str) -> Option<String>;
    fn current_dir(&self) -> Result<String, EvalError>;
}
```

实现：
- `StdIoHost`（默认，打印到真实 stdout/stderr）
- `TestIoHost`（测试用，捕获到 Vec<String>）
- `WasmIoHost`（WASM 嵌入用，桥接到 JS 控制台）

### 理由

- 测试：捕获 stdout 用于断言
- WASM：替换 I/O 实现而不重写 builtin
- 沙盒：限制 `IoHost` 调用来实现安全模式

### 代价

- 每个 I/O 操作多一次虚函数调用
- Builtin 的 `&dyn EvalEngine` 需要扩展为包含 `IoHost`（通过 supertrait 或单独注入）
