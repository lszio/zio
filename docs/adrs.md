# Zio 架构决策记录（ADRs）

> 轻量级架构决策日志。每个 ADR 记录一个关键决策的背景、决策、理由、代价、备选方案。
>
> ADR 的“已采纳”只表示设计决策成立，不表示其所有下游能力都已实现。
> 当前实现状态以[特性矩阵](feature-matrix.md)为准。

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
    pub modules: RefCell<ModuleTable>,
    pub loader: RefCell<Option<Box<ModuleLoader>>>,
}
```

通过 `&dyn EvalEngine` trait 注入到特殊形式和 builtin 函数；当前
`EvalEngine` 组合已拆出的 `EvalRuntime` 与 `ModuleRegistry`。

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

此 ADR 只覆盖 core 中 `im` 类型的结构共享。`lib/zio/persistent.zio`
的 library API 状态（Experimental，部分函数待核心 map 迭代支持）以
[特性矩阵](feature-matrix.md)为准。

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

**状态**: ✅ 已实现 — Sexp 各变体携带 `Option<Span>`，Reader 填充位置，
`SourceMap` 在 CLI 中注册多文件源；错误消息含行列号。

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

**状态**: ✅ 已实现（当前 AST evaluator）

### 背景

早期 `EvalEngine` 把求值（`eval_expr`、`env`）和模块操作
（`register_module`、`find_module` 等）放在同一个 trait 中。对于不加载模块的
嵌入边界，这会暴露不需要的能力。

### 决策

当前实现拆分为两个能力 trait，并用 marker supertrait 组合完整 evaluator：

```rust
/// 最小编译/求值能力。不含模块和 ZOS。
pub trait EvalRuntime {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
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

pub trait EvalEngine: EvalRuntime + ModuleRegistry {}
```

`EvalContext` 持有 `Arc<Env>`、`RefCell<ModuleTable>` 和可选 loader，分别实现
`EvalRuntime`、`ModuleRegistry` 与组合后的 `EvalEngine`。独立的
`ZosRuntime` 或 VM 能力 trait 尚不是当前 `context.rs` API；需要时另行决策。

### 理由

- 嵌入场景只需要 `EvalRuntime`，不需要模块和 ZOS
- 模块能力有独立的 `ModuleRegistry` 边界
- CLI/完整 AST evaluator 通过 `EvalEngine` 同时要求两种能力
- Future 可以添加 `IoHost`、ZOS 或 VM 能力 trait，而不污染核心求值接口

### 代价

- 需要求值与模块的调用点仍使用 `&dyn EvalEngine`
- 只实现 `EvalRuntime` 的嵌入器不能直接传给要求完整 `EvalEngine` 的 API

---

## ADR-006: Value::Object 作为 ZOS 入口

**状态**: ✅ 已实现 — `Value::Object(Box<dyn ZosObject>)` 已落地，
承载 ZOS Experimental 子集（Instance / GF 对象）；可调用性经
[ADR-013](#adr-013-zos-可调用协议zosapply)的 `zos::apply` 协议进入 eval。

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

**状态**: ✅ 已实现（与原设计的差异：缓存键是任意参数数量的类名哈希
`Vec<u64>`，而非固定 4 元组；`add_method` 时整体失效。类重定义的缓存
失效当前未处理——`defclass` 不支持重定义场景。）

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

**状态**: ✅ 已实现（含文件 I/O 收口）— IoHost 覆盖 stdout/stdin 与
`current_dir` / `read_file` / `write_file` / `file_exists`；
`slurp` / `spit` / `load` / `file-exists?` 全部经 IoHost 路由。
`StdIoHost` 使用真实文件系统；`BufferIoHost` 提供内存虚拟文件系统
（测试与沙盒）。`EvalContext` 持有唯一的 `SourceMap`，`load` 保留
span，加载文件内报错带真实行列号。WASM 侧 `WasmIoHost` 仍为后续项。

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

---

## ADR-012: 并发原语为同步占位，真并发延后决策

**状态**: ✅ 已采纳（诚实化决定）

### 背景

core 的 `Value` 含 `Future` 与 `Channel` 变体，`future-call` / `chan` /
`send!` / `recv!` 等 binding 已注册。但 `EvalContext` 因 `Env` 使用
`RefCell` 而是 `!Send`（ADR-002 已记录此约束），仓库中没有任何
`thread::spawn`：`future-call` 在当前线程同步求值后将结果包进 Future，
`deref` 的阻塞路径在实践中不可达。这构成 API 语义谎言——用户以为获得了
并发，实际为零；且与 README 的 agent 愿景（eval loop = agent loop，
需要真实异步 I/O）矛盾。

### 决策

1. 如实标注：并发原语的当前语义是**同步占位**（同步求值 + 值包装），
   在 [特性矩阵](feature-matrix.md) 与代码文档中显式声明。
2. **不再向 core 添加新的并发原语**；`Value::Future` / `Value::Channel`
   保留为数据形状（宿主嵌入方可用其驱动另一端）。
3. 真实并发的引入是独立决策，必须先回答 Env 的线程模型
   （Mutex 化 / 无共享 Actor / 绿色线程），形成新 ADR 后才动工；
   倾向将并发编排放在 `zio-actor` 库层而非 core。

### 理由

- 诚实优先：一个假装并发的 API 比"未实现"更糟。
- 同步占位已满足当前测试与演示需求；真并发是 VM/agent 阶段的前置决策，
  不是当下阻塞项。
- 在 AST 解释器上投资线程安全 Env 会在 VM 替换 Env 时被丢弃。

### 代价

- 短期内 `future-call` 无性能价值，仅保持 API 形状。
- agent 库的真实编排（LLM 调用、工具执行）被显式推迟。

---

## ADR-013: ZOS 可调用协议（zos::apply）

**状态**: ✅ 已实现

### 背景

`eval.rs` 的 `apply()` 直接 downcast 到 `GFObject` 并在 eval 层构建
method combination（`:before`/`:primary`/`:after`/`:around` 链）。这使
Runtime 层硬编码知晓 ZOS 的具体类型与分派语义，违反定理 5（协议比实现
重要）：MOP 的任何演化（分派缓存、combination 变体）都要修改 eval.rs。

### 决策

- `GFObject` 移入 `zos/gf.rs`（ZOS 层拥有自己的运行时对象形式）。
- 新增 `zos/apply.rs` 作为**唯一的** downcast 点：
  - `try_apply(obj, args, engine) -> Option<Result<TailResult, EvalError>>`
    —— eval 循环对不可调用对象返回 `None`，由 eval 自行报错；
  - `gf_shared(obj)` —— 反射 builtin 获取 GF cell 的受支持路径。
- method combination 构建逻辑整体迁入 `zos/apply.rs`；eval.rs 不再引用
  任何 ZOS 具体类型。

### 理由

- 分派语义回到 ZOS 层，MOP 演化不再触碰 eval 循环。
- 依赖方向单一化：`special/zos_forms → zos`（构造 GF）、`eval → zos::apply`
  （协议调用），无反向穿透。

### 代价

- GF 调用多一层间接（可忽略）。
- 未来若 MOP 需要更多 eval 钩子，应扩展 `zos::apply` 协议而非在 eval.rs
  加 downcast——这条边界由评审纪律与新 ADR 维护。

### 关联重构

- `builtins.rs` 单体（84 个绑定、1526 行）拆分为 `builtins/` 按域模块
  （numeric / predicates / collections / strings / zos_access / io /
  buffer / concurrency / json / macroexpand），`setup_env` 聚合注册；
  对外仅承诺 `setup_env`。拆分保持行为不变：`spit` 在拆分前即未注册，
  维持未注册并注释说明。

---

## ADR-014: 并发模型 —— 线程化与 VM 绑定，AST 解释器保持单线程

**状态**: ✅ 已采纳（技术验证完成，实现绑定 VM 阶段）

### 背景（已验证的事实）

ADR-012 将并发原语诚实化为同步占位，并把"真并发"推迟为独立决策。本 ADR
通过编译实验验证了当前类型系统的实际约束：

1. `Value` 同时 **!Send 与 !Sync**。第一个阻塞点是 `NativeFn.func:
   Arc<dyn Fn(...)>` 缺少 `Send + Sync` 约束。
2. 即使补上该约束，`Value::Function → Arc<Env>` 仍因 `Env` 的
   `RefCell<HashMap>` 保持 !Send（ADR-002 记录的约束）。
3. `Value::Object` 的 `Box<dyn ZosObject>` 同样阻塞——`ZosObject` trait
   未声明 `Send + Sync` 超trait（ADR-006 草案原本包含，落地时缺失）。

即：跨线程传递 `Value` 需要三处类型系统改动，缺一不可。

### 决策

1. **AST 解释器阶段不做线程化**。并发只在 VM（Phase 6+）落地，理由：
   VM 的调用帧是值栈 + 槽位，不共享 `Env` 链，天然规避 `Env` 线程
   安全问题；现在为 AST 解释器把 `Env` 改成 `Mutex` 是对注定被替换的
   结构做投资。
2. 并发形态采用**每线程独立 VM 实例 + Channel 消息传递**（CSP）：
   线程间只传 `Value`，不共享可变求值状态。`Value::Channel` 已是正确
   的桥接形状。
3. 在 VM 动工前，**允许**提前完成纯编译期的前置改动（无运行时成本）：
   - `NativeFn.func` 加 `Send + Sync` 约束（需确认现有闭包无违例）；
   - `ZosObject` 补 `Send + Sync` 超trait（与 ADR-006 草案对齐）；
   - `Env` 的内部可变性**留到 VM 一并解决**（唯一有运行时成本的项）。

### 否决的备选

- **Mutex 化 Env**：单线程解释器也要付锁成本，且 VM 阶段作废。
- **绿色线程 / async 运行时**：需要重写 eval 循环为状态机，成本最高；
  待真实 agent 负载证明需要后再评估。

### 代价

- agent 库的真实编排（LLM 调用、工具执行）继续等待——阻塞项被显式记录。
- "eval loop = agent loop" 的叙事在 VM 交付前对并发场景不成立，文档
  已如实标注。

---

## ADR-015: 内存模型 —— Arc 环泄漏已知，方案绑定 VM 阶段

**状态**: ✅ 已采纳（问题已复现并有测试锚定）

### 背景

闭包捕获 `Arc<Env>`，环境又持有 `Value`。自引用定义（如
`(def f (fn [] f))`）构成 `env → f → Function → env` 的引用环，Arc
引用计数无法回收。测试
`eval::tests::arc_cycle_in_self_referential_closure_is_leaked` 用弱引用
句柄复现并锚定了该行为：context 释放后环境仍然存活。

### 决策

1. 泄漏为**已知且被测试锚定**的现状；仓库内所有长期运行场景（REPL
   会话、嵌入宿主）应知晓：自引用闭包的内存不回收。
2. 修复方案与 VM 一并决策（同 ADR-014 的绑定）：候选为 arena/池 GC
   （VM 帧天然分区）或弱 env 引用（`Function` 持 `Weak<Env>` + 提升
   失败即闭包失效——需要定义失效语义）。在 VM 之前不投入。
3. 若未来修复落地，翻转锚定测试的断言作为验收。

### 代价

- 长会话中的环泄漏会累积；对嵌入式长时运行是真实风险，文档需如实
  提示（已在本 ADR 与特性矩阵记录）。

---

## ADR-016: 程序合成模块 —— 学习型提议、语言化记忆、eval 裁判

**状态**: ✅ 已采纳(规划见 [synthesis-plan](synthesis-plan.md);实现均为 Planned)

### 背景

同象性学习器 MVP(`lib/zio/learn.zio`,枚举 + eval 评分)验证了"数据 →
搜索 → 程序"的闭环,但枚举提议器受组合爆炸约束(深度 2 即需秒级,
深度 3 不可行)。程序合成的瓶颈在提议而非评分;学习机器(LLM、遗传
算子、RL 策略、神经网络)是提议能力的不同来源;zio 的 eval 是共享的
确定性裁判。

### 决策

1. **一个循环,N 种提议器(SICP 4.3 amb 的确定性工程化)**:提议器
   协议 `(proposer task history) → 候选文本列表` 可插拔——枚举(默认)、
   LLM、遗传算子、混合同协议;解析、闭世界白名单、规范化去重、评分、
   选择、预算(`:max-generations` / `:max-evals`)全部在循环内,
   对所有提议器一致生效。提议器只产文本,不解析、不评分、不执行。
2. **唯一裁判教义**:eval 是唯一 ground truth;任何学习型组件(损失
   预测器、先验模型)只能重排/剪枝候选以节省 eval 预算,不得替代
   eval 判定成功。embedding 只做可选的语义桥(自然语言任务),不做
   候选去重(候选去重用 AST 规范化 + 结构哈希)。
3. **宿主协议外部 attach**:trait `LlmHost` / `EmbedHost`(远期
   `ModelHost`,仅命名预留)进新 crate `zio-ai`(ADR-009 预留名,
   承载 "LLM API / 自学习" 的宿主能力面;crate 内不含学习算法)。
   `zio_ai::install(ctx, …)` 以 NativeFn 闭包注册 `llm-complete` /
   `embed`,core 零改动;无宿主返回 `capability-denied:` 前缀的稳定
   错误(合同锚定)。**不采用** IoHost 式 `EvalContext` 字段方案——
   trait 进 core 违反 ADR-009;协议独立成 crate 的前提是注入点外置。
4. **RL 环境视角**:循环天然是 RL 环境——state = (task, history)、
   action = 候选批、reward = −损失;经验库记录的 (任务, 程序, 得分)
   即轨迹数据。RL 学习器与 bandit 元调度只是该数据流的消费者,
   无需改循环。
5. **确定性回放合同**:核心循环对同样提议永远产出同样结果;随机性
   (GP 的 `:seed`、LLM temperature 的 Mock 冻结)隔离在提议边界;
   Mock record/replay 使学习过程精确复现,replay miss 即 fail-fast。
6. **记忆的语言化(经验即数据)**:`lib/zio/memory.zio` 三索引经验库
   ——结构索引(规范化 AST)、行为指纹(规范输入电池上的输出向量,
   解释器即 embedder)、可选向量索引(`lib/zio/vector.zio`,自然语言
   任务的语义桥);宏蒸馏升级为**反统一蒸馏**(Plotkin LGG →
   defmacro)+ **环境吸收**(宏定义进经验模块,后续学习 require 之,
   `:ops` 词汇表与白名单扩充;经验文件本身是可 load 的 zio 源码);
   学习指标含 DSL 收缩(求解程序描述长度随经验递减)。蒸馏候选须过
   既有合同测试。
7. **与 ADR-014 的关系**:阻塞式 LLM 调用如实标注为同步;真实 agent
   并发绑定 VM 阶段;WASM 侧(同步栈无法等待 fetch,Asyncify/worker
   未决)不在 L1-L4 承诺内,演示仅 Mock 回放。

### 理由

- 学习器的瓶颈在提议而非评分;提议抽象化后,枚举只是其中最笨的实现,
  新学习机器零改动接入,循环只有一份;
- 同像性使 LLM 等输出天然是可直接 eval 的数据——解析即校验是其他
  宿主语言需要额外基础设施才能获得的结构优势;
- 外部 attach 是"协议独立 crate + core 最小"唯一自洽的注入方式;
- RL 环境视角使泛化(ML/RL/NN)不产生新架构——只是同一数据流的
  新消费者与同一协议的新实现;
- 行为指纹使"解释器即 embedder"——数值/符号任务的记忆完全不依赖
  外部模型,向量只在对外的自然语言边界出现。

### 代价

- LLM 调用成本与延迟;缓解:全量记账、Mock 主导开发、候选缓存;
- 随机性引入可复现性风险;缓解:回放合同为强制合同测试;
- 新增一个 workspace crate;status 脚本 crate 计数需同步;
- `capability-denied` 以错误前缀约定而非类型化变体(避免 core 改动);
  如需真类型,另立 core 提案。

### 交付与衡量

分 L1-L4 四阶段,含论文(工作坊 → 完整)与汇报(内部里程碑 → 外部
分享)节点,详见 [synthesis-plan](synthesis-plan.md) 第 3/5/6/7 节。
