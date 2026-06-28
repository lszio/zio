# 02: 两个世界：Sexp 与 Value

> 一个表示"代码"，一个表示"运行时的值"。它们是两个枚举，但长得非常像。为什么分开？

---

## 直觉

看一个简单的 Zio 表达式：

```lisp
(+ 1 (* 2 3))
```

从**文本**这个角度看，它就是一个嵌套的列表：`+, 1, (*, 2, 3)`。

从**运行时**的角度看，它是另一个东西：整数 `1` 和整数 `6` 相加得到 `7`。

这是两个完全不同的域：

| 域 | 元素 | 用途 | 可变吗？ |
|----|------|------|---------|
| 代码（语法） | 符号、列表、关键字 | 表示程序结构 | 宏会构造和变换 |
| 运行时（值） | 函数、闭包、环境引用 | 表示程序状态 | 计算产生新值 |

问题来了：**如果只有一个类型系统，宏怎么得到"符号"而不是"符号当前绑定的值"？**

这就是 ADR-001 的原因。

## 两个枚举

代码域：[`core/src/sexp.rs`](../core/src/sexp.rs) 中的 `Sexp`

```rust
pub enum Sexp {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),     // ← 代码中的名字
    Keyword(String),
    List(Vector<Sexp>),
    Vector(Vector<Sexp>),
    Map(HashMap<Sexp, Sexp>),
}
```

运行时域：[`core/src/value.rs`](../core/src/value.rs) 中的 `Value`

```rust
pub enum Value {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),         // ← 运行时查到的名字
    Keyword(String),
    List(Vector<Value>),
    Vector(Vector<Value>),
    Map(HashMap<Value, Value>),

    // Value 独有——运行时产生的东西：
    Function(Arc<Function>),         // 用户定义的闭包
    NativeFunction(NativeFn),        // Rust 实现的函数
    Macro(Macro),                    // defmacro 的结果
    Atom(RefCell<Value>),            // 可变引用（未来）
    Promise(...),                    // 未来（Phase 4）
    Buffer(Vec<u8>),                 // 字节向量（Future）
}
```

对比一下就能直观看出区别：

- **`Sexp` 只有"数据"**：它表达的是程序文本。没有函数，没有闭包，没有原子。
- **`Value` 有"运算结果"**：它包含函数（用户定义的和内置的）、宏、未来的原子和 promise。

但也有跨越两边的例子：

- `Sexp::Symbol("x")` — 在代码中，"这个名字"。
- `Value::Symbol("x")` — 在运行时，"这个名字被当作值（而不是查找它）"。

这就是关键区别：**eval `Sexp::Symbol("x")` 时会去环境查找, 得到 `Value::Integer(42)`. 但如果 `quote` 阻止了求值, `Sexp::Symbol("x")` 变成 `Value::Symbol("x")`, 符号本身作为值保留。**

## quote：两个世界的桥梁

```lisp
> x                    ;; 假设 x = 42
42
> (quote x)
x                      ;; 符号 x 本身，不是 42
> 'x
x                      ;; ' 是 quote 的语法糖
```

`quote` 的特殊之处在于：它接受一个 `Sexp`，**直接返回一个对应的 `Value`**，跳过求值。

在代码中（`special/data.rs`）：

```rust
fn eval_quote(args: &[Sexp], ...) -> Result<TailResult, EvalError> {
    if args.len() == 1 {
        Ok(TailResult::Value(sexp_to_value(&args[0])))
    }
}
```

`sexp_to_value` 递归地将 `Sexp` 树转换为 `Value` 树——符号仍然是符号，列表仍然是列表，只是换了类型系统的身份。

## 宏：在两个世界往返

宏工作方式的三步：

1. **接收 Sexp**：宏的参数是未经求值的 S 表达式
2. **返回 Sexp**：宏体通过 `list`、`quote` 等构造一个新的 S 表达式
3. **eval 新 Sexp**：`reader::read` → `eval`，把宏返回的代码变成值

看 `unless` 宏的执行流程：

```
输入:   (unless false 42)
         ↓
Sexp:   List([Symbol("unless"), Symbol("false"), Integer(42)])
         ↓
宏展开: (if false nil 42)
         ↓  (重新 eval)
Value:  Nil
```

注意每一步的类型：

```
文本          → 代码 (Sexp)
宏定义体       → 代码 → 值 (Sexp → Value::Macro)
宏调用         → 代码 (Sexp)
宏展开         → 代码 (Sexp)  ← 宏体返回的就是 Sexp
重新 eval      → 值 (Value)
```

**宏工作在两套类型系统的交界处。** 如果只有一套类型系统，宏就不可能在不触发求值的情况下操作"代码"。

## 转换代价

两个枚举意味着转换函数：

```rust
impl From<Sexp> for Value { /* 递归转换 */ }
pub fn sexp_to_value(s: &Sexp) -> Value { /* 同上 */ }
pub fn value_to_sexp(v: &Value) -> Sexp { /* 反向 */ }
```

代价是什么？

- **性能**：每次宏展开后需要从 `Value` 转回 `Sexp`（`value_to_sexp`），这是一个 `O(n)` 克隆。
- **代码量**：两个枚举 + 两套显示/比较逻辑。
- **心智负担**：开发者需要意识到"我现在在哪个域"。

但收益更大：

- **宏不需要在运行时域中做"暂停求值"的 hack**
- **quote 的自然实现**：`'x` → 从 Sexp 域直通 Value 域
- **类型安全**：Rust 编译器保证你不会在 Sexp 中见到一个 `Macro`，也不会在 Value 中漏掉它

## ADR-001 回顾

ADR（Architecture Decision Record）是项目中的轻量级架构决策日志。

```
ADR-001: Sexp 与 Value 分离
决定:    Sexp 是语法树，Value 是运行时值
理由:    同像性需要在两个域之间往返
代价:    两个类似的枚举，偶尔需要转换函数
```

打开 [`docs/adrs.md`](../docs/adrs.md#adr-001-sexp-与-value-分离) 查看 ADR-001 完整记录。

## 在代码中

- 对比 [`core/src/sexp.rs`](../core/src/sexp.rs) 和 [`core/src/value.rs`](../core/src/value.rs) 的枚举定义
- 看 [`core/src/special/data.rs`](../core/src/special/data.rs) 中 `quote` 的实现
- 看 [`core/src/macros.rs`](../core/src/macros.rs) 中宏展开后的 `value_to_sexp` 调用

```rust
// macros.rs 中宏展开的关键调用：
let result = eval_inner(&body, &macro_env, false, engine)?; // 在代码域构建新 Sexp
let result_sexp = value_to_sexp(&result);                  // 转回代码域
eval_inner(&result_sexp, env, tail, engine)                // 在新环境中求值
```

---

**下一篇：[03: 显式状态：EvalEngine Trait](03-explicit-state.md)** — 从一个 joke（4 个 thread_local! 全局变量）到可测试的架构。