# 01: 你好，eval

> 评估一段 Lisp 代码，就是"求值"（eval）。从 S 表达式（S-expression）到运行时值（Value），这就是 eval 循环的核心。

---

## 问题

写一个 Lisp 方言，第一件事不是设计语法，不是写 REPL，而是实现 **eval**。

Lisp 的 eval 和其他语言的解释器有一个根本不同：**代码本身就是数据结构**。你写的 `(+ 1 2)` 不只是一串字符，它就是一个由三个元素组成的链表——符号 `+`、整数 `1`、整数 `2`。

这个性质叫**同像性**（homoiconicity），它是宏系统的基石。但同像性也带来了第一个架构决策：我们需要两套类型系统——一套表示"代码"，一套表示"运行时的值"。

## eval 循环

打开 [`core/src/eval.rs`](../core/src/eval.rs)，看 `eval_inner` 函数：

```rust
fn eval_inner(expr: &Sexp, env: &Arc<Env>, tail: bool, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    match expr {
        // Self-evaluating types
        Sexp::Nil       => Ok(TailResult::Value(Value::Nil)),
        Sexp::Boolean(b) => Ok(TailResult::Value(Value::Boolean(*b))),
        Sexp::Integer(i) => Ok(TailResult::Value(Value::Integer(*i))),
        Sexp::Float(f)   => Ok(TailResult::Value(Value::Float(*f))),
        Sexp::String(s)  => Ok(TailResult::Value(Value::String(s.clone()))),
        Sexp::Keyword(k) => Ok(TailResult::Value(Value::Keyword(k.clone()))),

        // Symbol lookup
        Sexp::Symbol(s) => env.get(s)
            .map(TailResult::Value)
            .ok_or_else(|| EvalError::symbol_not_found(s.clone())),

        // List: special form, macro, or function call
        Sexp::List(list) => { /* dispatch */ }
    }
}
```

逻辑非常简单：

1. **自求值类型**：数字、字符串、布尔值——它们本身就是值，直接返回。
2. **符号查找**：`x` → 在环境中查找 `x` 绑定的值。
3. **列表**：这是唯一的"复合表达式"。空列表是 `nil`；非空列表的第一个元素决定怎么求值——特殊形式、宏、还是函数调用。

核心洞察：**所有 Lisp 表达式要么是原子（自求值或符号），要么是列表。只有这两种情况。** 这就是 Lisp 语法极简的根源。

## 从列表到函数调用

`(+ 1 2)` 是三个元素的列表：

```rust
Sexp::List([
    Sexp::Symbol("+"),
    Sexp::Integer(1),
    Sexp::Integer(2)
])
```

eval 的处理流程：

1. 检查第一个元素（`+`）是不是一个特殊形式的名字 → 不是，继续
2. 评估第一个元素得到函数值 → `Value::NativeFunction(+)` 
3. 检查是不是宏 → 不是，继续
4. 评估参数（`1` → `Value::Integer(1)`, `2` → `Value::Integer(2)`）
5. 调用 `apply`：传入函数值和参数列表 → `Value::Integer(3)`

这就是 `eval` 的本质：**递归地评估每个子表达式，直到碰到自求值类型，然后把结果一层层组合回去。**

## 尾位置

`eval_inner` 的签名里有一个 `tail: bool` 参数。这个参数标记当前表达式是否处于**尾位置**（tail position）——即它的值就是整个函数的返回值。

为什么需要这个？因为通用 TCO（尾调用优化）是未来路线图中的任务（Phase 1: 核心稳定化）。`TailResult` 枚举有两个变体：

- `TailResult::Value(v)` — 普通值
- `TailResult::Recur(env)` — 需要跳回 `loop` 帧继续

当前只有 `loop`/`recur` 利用了尾位置（文件 `special/letloop.rs`），但架构已经为通用 TCO 铺好了路。

## 同像性在行动

同像性不是说"代码是字符串"，而是"代码的内部表示就是语言的核心数据结构"。

当我们写宏时：

```lisp
(defmacro unless [test body]
  (list 'if test nil body))
```

宏 `unless` 接受两个参数（`test` 和 `body`），返回一个由 `list` 构造的 S 表达式。这个 S 表达式再被 eval 求值。`unless` 本身**不是一个特殊形式**——它只是一个返回数据结构的普通函数。

这个数据->代码->数据的往返就是同像性：

```
代码（Sexp）→ eval → 值（Value）
  ↑                      │
  └── macro：构造 Sexp ←─┘
```

## 在代码中

- 打开 [`core/src/eval.rs`](../core/src/eval.rs)，看 `eval_inner`（第 81-161 行）
- 打开 [`core/src/sexp.rs`](../core/src/sexp.rs)，看 `Sexp` 枚举的定义
- 打开 [`core/src/value.rs`](../core/src/value.rs)，对比 `Value` 枚举

```bash
# 启动 REPL，试试 eval
cargo run

# 在 REPL 中
zio> (+ 1 2 3)
6
zio> (defn identity [x] x)
zio> (identity 42)
42
```

---

**下一篇：[02: 两个世界：Sexp 与 Value](02-two-worlds.md)** — 为什么我们需要两套类型系统，以及它们之间的转换代价。