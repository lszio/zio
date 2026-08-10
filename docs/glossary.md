# Zio 术语表

> 运行时、类型系统、宏系统、ZOS 的关键概念定义。

---

## A

### apply

函数调用操作。`apply` 接受一个可调用值和一个参数列表，绑定参数到函数的环境，然后求值函数体。是 eval 循环中与 `eval_inner` 并列的核心函数。

### AMOP（A Metaobject Protocol）

ZOS 的核心元编程协议。定义了类、通用函数、方法等元对象如何创建、组合、反射。名称来源于 Gregor Kiczales 等人的同名著作。

### ADR（Architecture Decision Record）

架构决策记录。轻量级文档记录每个重要架构决策的背景、决策、理由、代价。

---

## B

### Builtin

Rust 实现的内置函数。通过 `NativeFn` 注册到环境。在 Zio 中，builtin 是唯一用 Rust 直接实现的业务逻辑（其他都是宏或库）。

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

当前核心求值 trait。只包含 `eval_expr` 和 `env()`；模块加载与缓存由独立的
`ModuleRegistry` trait 提供。

---

## F

### Function

Zio 的第一类函数。包括用户定义函数（带词法闭包）、NativeFn（Rust 实现）、Macro。所有 Function 都是可调用值。

---

## G

### Generic Function（GF）

ZOS 的多分派函数。不是单分派对象的方法，而是完全独立、基于全部参数类型分派的行为入口。

---

## H

### Homoiconicity（同像性）

代码的内部表示与核心数据结构相同。在 Zio 中，一切代码都是 Sexp，而 Sexp 也是数据。这意味着程序可以读取、变换、生成自己的代码。这是宏系统的基础。

### Heap Object

ZOS 中需要在堆上分配的 Object。包括 Instance、Class、GenericFunction、Method、Package、Condition 等。通过 `Value::Object(Box<dyn ZosObject>)` 接入。

---

## I

### Immediate Object

ZOS 中可以内联在 `Value` 枚举中的类型。包括 Integer、Float、Boolean、Nil、Keyword 等。不分配堆内存。

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

---

## N

### NativeFn

Rust 实现的 Zio 函数。签名：`fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError>`。所有性能关键路径通过 NativeFn 实现。

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

---

## R

### Reader

Zio 的解析器。将源代码字符串解析为 Sexp 树。包括 tokenizer（lexer）和 parser（reader）。支持 reader macro 扩展。

### RefCell

Rust 的内部可变性模式。ZOS 在单线程阶段使用 `RefCell` 管理可变状态（slot 值、模块注册表）。Phase 4 多线程时可能改为 `Mutex`。

### Restart

Condition System 的恢复选项。当 condition 被 signal 时，调用者可以从多个 restart 中选择恢复策略。

---

## S

### Sexp

Symbolic Expression。Zio 的代码表示（AST）。一切代码和数据都编码为 Sexp。是「代码即数据」的具体实现。

### Slot

ZOS 中类的属性定义。每个 Slot 有名称、类型约束（可选）、默认值、分配策略（实例/类）。

### Span

源码位置信息。`{ start: BytePos, end: BytePos, line: usize, col: usize, source: SourceId }`。Zio 的 Phase 1 目标是将 Span 嵌入每个 Sexp。

### Special Form

不被标准 eval 规则（先求值参数，再应用函数）处理的语法形式。如 `if`、
`def`、`fn`、`quote`。当前生成计数为 23；权威数量见
[项目状态](status.md)，不要在术语表中手工维护。

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
signal 等操作。当前 `core/src/context.rs` 尚无独立 `ZosRuntime` trait；现有
experimental ZOS 分派仍经 `EvalEngine` 路径执行。

### ZIR（Zio Intermediate Representation）

未来 Zio 编译器的中间表示。当前未实现。Phase 5+ 的编译优化和 JIT 的基础。
