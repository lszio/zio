# Zio 术语表

> 语言、ZOS、工具链、公共基础设施与 Grove 的术语。规范名不等于已有 API；当前成熟度见[特性矩阵](feature-matrix.md)，架构与权限以[ADR-019](adrs.md#adr-019-grove-独立应用与同像性逻辑演化)为准。

Zio 库目标采用 Zio 实现并归入 `libs/`：Numa 持计算，Rill 持 CLI 组合，Loom 持 harness 组合。现有 Rust 通用传输/宿主合同位于 `contribs/native/loom/`，不是 Zio 库本体；普通语言 CLI 位于 `langs/cli/`，不装配 LLM replay。Grove 是 `apps/grove/` 独立应用，其 Rust 业务仍待后续迁为 Zio；语言介绍、文档与 playground 位于 `apps/site/`。本轮只完成结构切换，不证明完整库交付、Tree-sitter、LSP 或编译器自举。当前合同见[架构](zio-architecture.md)与[批准目录计划](superpowers/plans/2026-10-07-language-first-layout.md)。短名不等于已注册包 ID。

---

## A

### apply

函数调用操作。`apply` 接受一个可调用值和一个参数列表，绑定参数到函数的环境，然后求值函数体。是 eval 循环中与 `eval_inner` 并列的核心函数。

### AMOP（A Metaobject Protocol）

ZOS 的核心元编程协议。定义了类、通用函数、方法等元对象如何创建、组合、反射。名称来源于 Gregor Kiczales 等人的同名著作。

### ADR（Architecture Decision Record）

架构决策记录。轻量级文档记录每个重要架构决策的背景、决策、理由、代价。

### ACP（Agent Client Protocol）

外部 agent 与客户端之间的会话协议。目标双向支持：先 Loom 客户端与 Grove 教师适配，再服务端；不进入 Zio 核心语义。两方向分别验收，不将自有教师 HTTP 协议称为 ACP。

### Agent logic（可编码 agent 逻辑）

实际驱动任务分解、上下文选择、模型/工具路由、校验、修订和停止的版本化 Zio 程序。展示 DSL、提示词摘要或 LLM 自述不等于执行该逻辑；程序、实际路径与评价证据需关联。

---

## B

### Builtin

Rust 实现、通过 `NativeFn` 注册的原语。语言内建、性能计算与外部能力分别由各层安装；授权、存储与进程管理也可由 Rust 可信宿主实现，不把“其他都是宏”当作部署约束。

---

## C

### Class

ZOS 中的类型描述符。定义对象的结构（槽位）、继承关系（superclass）、元类（metaclass）。Class 本身也是 Object。

### CPL（Class Precedence List）

类优先级列表。多继承时，决定方法分派的顺序。ZOS 使用 C3 线性化算法。

### Condition System

带恢复选项的错误处理系统。不简单地抛出异常，而是 signal condition 并允许调用者从多个 restart 中选择修复策略。

### Call-next-method

在 `:around`、`:before`、`:primary` 方法体内调用下一个方法（按方法组合顺序）。是 Method Combination 的关键原语。

---

## D

### Dispatch Cache

GF（Generic Function）的参数类型 → 方法列表的缓存。ZOS 使用 4-参数哈希键（前 4 个参数的类型 ID 元组），命中时 O(1) 返回。

---

## E

### Eval

求值操作。`eval(expression, env) → value`。将 Sexp 代码树转换为 Value。是 Lisp 的核心操作，也是 Zio eval 循环的入口。

### EvalEngine

当前 AST evaluator 的组合 trait：`EvalEngine: EvalRuntime + ModuleRegistry`。
特殊形式和 builtin 函数通过它访问求值与模块能力。

### EvalContext

默认的 `EvalEngine` 实现。持有根环境、模块注册表、模块加载器。每个 `EvalContext` 实例是完全隔离的 eval 域。

### EvalRuntime

当前核心求值 trait，提供 `eval_expr`、`env()`、`source_map()` 和宿主 `io()`；
`io()` 默认使用 `StdIoHost`。模块加载、缓存与导出累积由独立的
`ModuleRegistry` trait 提供。完整签名见 [`langs/core/src/context.rs`](../langs/core/src/context.rs)。

---

## F

### Function

Zio 的第一类函数。包括用户定义函数（带词法闭包）、NativeFn（Rust 实现）、Macro。所有 Function 都是可调用值。

---

## G

### Generic Function（GF）

ZOS 的多分派函数。不是单分派对象的方法，而是完全独立、基于全部参数类型分派的行为入口。

### Grove

五字母正式短名，独立的模块化自学习与逻辑演化应用，消费 Zio 与 Numa、Rill、Loom，拥有学习目标、实验、反馈、检查点、独立评价、人工审查、逻辑演化和发布及 Web。内部宿主 `grove` crate 位于 `apps/grove/native/learning/`，应用 `grove-app` 位于 `apps/grove/native/app/`；内部采用库结构不意味着库优先或可选产品壳，语言核心无反向依赖。

---

## H

### Homoiconicity（同像性）

代码的内部表示与核心数据结构相同。在 Zio 中，一切代码都是 Sexp，而 Sexp 也是数据。这意味着程序可以读取、变换、生成自己的代码。这是宏系统的基础。

同像性支持读取、改写和审查显式逻辑，不自动提供完整宏展开、权限安全、行为正确或神经网络内部透明。

### Harness

Agent 执行基础设施的职责名称，官方 Zio 库正式短名为 [Loom](#loom)。Loom 持有模型/工具会话、预算、取消、provider 与 ACP；不持 Grove 的学习目标、独立评价或正式发布批准。

### Heap Object

ZOS 中需要在堆上分配的 Object。包括 Instance、Class、GenericFunction、Method、Package、Condition 等。通过 `Value::Object(Box<dyn ZosObject>)` 接入。

---

## I

### Immediate Object

ZOS 中可以内联在 `Value` 枚举中的类型。包括 Integer、Float、Boolean、Nil、Keyword 等。不分配堆内存。

---

## L

### Loom

四字母 harness 库方向。Zio 组合位于 `libs/loom/`；已有 Rust crate `loom`
位于 `contribs/native/loom/`，提供通用模型/工具合同、预算与传输适配，
不能把它视为 Zio 库已整体完成。学习目标、评价与批准留在 Grove。
ACP 与各模型能力的状态以特性矩阵为准；语言 CLI 不装配 LLM replay。
由 Grove 注入执行回调并通过未来 `grove acp serve` 装配，不是语言 CLI 子命令。

---

## M

### Macro

Zio 的宏系统。通过 `defmacro` 定义。宏接受 Sexp 参数（未求值），返回 Sexp（新代码），然后 eval 求值。是同像性的直接体现。

### MOP（Meta Object Protocol）

ZOS 的元对象协议。提供自定义 Class、对象创建、Dispatch、Slot 分配的运行时扩展能力。MOP 不依赖任何高级框架。

### Method

GF（Generic Function）的一个实现。由 `defmethod` 定义，包含参数类型约束（specializers）、组合限定符（qualifier）、函数体。

### Method Combination

ZOS 中多个 Method 组合执行的方式。支持 `:before`、`:after`、`:around`、`primary` 四种限定符，通过 `call-next-method` 串联。

### Module（Grove 模块）

版本化的源码、向量、神经网络或 LLM 能力单元，分别声明执行、更新、可修改范围、评价及状态兼容能力。共享生命周期，不假定所有模块都支持权重训练；LLM 推理、教师、适配器训练与完整权重训练是不同能力。

---

## N

### NativeFn

Rust 实现的 Zio 函数。签名：`fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError>`。所有性能关键路径通过 NativeFn 实现。

### Numa
四字母计算库方向，Zio 向量库已位于 `libs/numa/vector.zio`。
连续数组、矩阵及更广后端能力仍按特性矩阵验收；CPU worker 是
`apps/grove/workers/torch/` 应用后端，不证明整个 Numa 库已完成。
计算与 Grove 训练、评价、发布治理分开，不以新建独立 Rust 业务库为目标。

---

## O

### Object

ZOS 中一切运行时元素的基本单位。每个 Object 都有 `ObjectHeader`（class 引用、标志位、身份）。Object 不一定是 Class 的实例。

### Open Type System

ZOS 的开放类型系统。核心只提供 Class，未来可以通过 MOP 扩展 Protocol、Trait、Interface 等类型系统概念。

---

## P

### Package

ZOS 的符号命名空间管理器。负责符号的 export、import、alias、解析。是大型 Zio 项目的组织基础。

### Persistent Data Structure

持久化数据结构（不可变 + 结构共享）。Zio 默认使用 `im::Vector` 和 `im::HashMap`。修改返回新版本，旧版本仍然有效。是函数式编程和并发安全的基础。

### Proposer（提议器）

程序合成模块（ADR-016）中可插拔的候选来源。协议形状为 `(proposer task history) → 候选字符串列表`：提议器只产文本，解析、闭世界白名单、去重、评分、选择与预算全部在学习循环内。枚举器、LLM、遗传算子都是同一抽象的实现——学习循环是 `amb` 求值器（SICP 4.3）的确定性工程化，提议器对应其候选来源。

### Publication approval（正式发布批准）

发布者对指定候选及评价版本的人工批准。控制器仍检查硬门槛、冻结区和 expected-version 后原子切换；批准不是免检令牌，独立评价合格也不自动发布。在途调用固定旧程序及依赖。

---

## R

### Reader

Zio 的解析器。将源代码字符串解析为 Sexp 树。包括 tokenizer（lexer）和 parser（reader）。支持 reader macro 扩展。

### RefCell

Rust 的内部可变性模式。ZOS 在单线程阶段使用 `RefCell` 管理可变状态（slot 值、模块注册表）。Phase 4 多线程时可能改为 `Mutex`。

### Restart

Condition System 的恢复选项。当 condition 被 signal 时，调用者可以从多个 restart 中选择恢复策略。

### Rill
四字母 CLI 组合库方向（Planned），目标用 Zio 提供参数、子命令、帮助、
终端 I/O 与退出状态，不承担语言求值；尚未创建库目录，不设空占位。
`langs/cli/` 的 `zio-cli` 是普通语言二进制，不是 Rill 库。

---

## S

### Sexp

Symbolic Expression。Zio 的代码表示（AST）。一切代码和数据都编码为 Sexp。是「代码即数据」的具体实现。

### Self-hosting（工具链自举）

Zio 编写完整展开器、分析器与编译器，由初始执行环境引导后编译自身，再比较重复构建的规范化产物与行为。Rust 保留最小运行时、宿主及性能原语；Zio 编辑器或 agent 的应用自托管不等于编译器自举。

### Slot

ZOS 中类的属性定义。每个 Slot 有名称、类型约束（可选）、默认值、分配策略（实例/类）。

### Span

源码位置与来源标识。当前 Sexp 已携带 Span；Sexp ↔ Value 转换与宏生成路径不能默认保留来源，工具链和审计轨迹必须显式维护来源关系。

### Special Form

不按普通“先求值参数，再调用”规则处理的语言形式，如 `if`、`def`、
`fn`、`quote`。数量由[项目状态](status.md)生成，不在术语表手工复制；
新增领域能力优先在宿主/库/应用层实现，而不是追加特殊形式。

---

## T

### TCO（Tail Call Optimization）

尾调用优化。当前 AST evaluator 把尾位置的普通函数调用编码为
`TailResult::TailCall(Value, Vector<Value>)`，由 trampoline 反复执行，支持
普通自递归和互递归而不按调用次数增长 Rust 栈。`loop/recur` 是另一条专用路径。

### TailResult

eval 的返回值类型。有三个变体：`Value(Value)` 是普通返回值；
`TailCall(Value, Vector<Value>)` 把函数与已求值参数交给通用 trampoline；
`Recur(Vector<Value>)` 只把新参数交回 `loop` 重新绑定，不表示普通函数尾调用。

---

## V

### Value

Zio 的运行时值类型。是 Sexp 求值后的结果。包含 nil、布尔值、数字、字符串、符号、关键字、集合（List/Vector/Map）、函数、NativeFn、宏、以及 ZOS Object。

---

## Z

### ZOS（Zio Object System）

Zio 的统一运行时对象模型。AMOP 的参考实现。定义了 Object、Class、Generic Function、Method、Package、Condition 的运行时表示和组合规则。

### ZosRuntime

ZOS 规范中规划的能力 trait，用于隔离 `class_of`、GF 分派和 condition
signal 等操作。当前 `langs/core/src/context.rs` 尚无独立 `ZosRuntime` trait；现有
experimental ZOS 分派仍经 `EvalEngine` 路径执行。

### ZIR（Zio Intermediate Representation）

未来 Zio 编译器的中间表示。当前未实现。Phase 5+ 的编译优化和 JIT 的基础。
