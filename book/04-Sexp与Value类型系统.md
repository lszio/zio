# 04 - Sexp 与 Value：代码域和运行时域

## 两个世界

Lisp 有一个其他语言少见的特性：**代码即数据**。但这实际上意味着——代码在你写的时候是一种结构，而运行的时候是另一种结构。

Zio 在实现中把这两个世界分得很清楚：

```
你写的代码 →  Sexp（语法树）
                ↓
            eval(求值)
                ↓
运行时结果 →  Value（值）
```

## 第一个世界：Sexp（语法树）

打开 `core/src/sexp.rs` 看看：

```rust
pub enum Sexp {
    Nil,                                // nil
    Boolean(bool),                      // true / false
    Integer(i64, Option<Span>),         // 42
    Float(f64, Option<Span>),           // 3.14
    String(String, Option<Span>),       // "hello"
    Symbol(String, Option<Span>),       // foo, +, defn
    Keyword(String, Option<Span>),      // :key, :name
    List(Vector<Sexp>, Option<Span>),   // (1 2 3) 或 (+ 1 2)
    Vector(Vector<Sexp>, Option<Span>), // [1 2 3]
    Map(HashMap<Sexp, Sexp>, Option<Span>), // {:a 1 :b 2}
    Char(char, Option<Span>),           // \a
}
```

**关键点**：Sexp 里面没有"函数"、"宏"这样的类型。只有纯数据。

当你输入 `(+ 1 2)` 时，Reader（解析器）把它变成了：

```
Sexp::List([
    Sexp::Symbol("+"),      ← 这只是一个名字字符串！
    Sexp::Integer(1),       ← 一个数字
    Sexp::Integer(2),       ← 另一个数字
])
```

注意：`+` 在这个阶段**只是一个名字**。它还没被"翻译"成加法函数。

每个变体（除 Nil 和 Boolean）都带有 `Option<Span>`——这是源码位置信息。出了错，Zio 可以告诉你"第 3 行第 5 列出错了"。

## 第二个世界：Value（运行时值）

打开 `core/src/value.rs`：

```rust
pub enum Value {
    // 与 Sexp 相同的纯数据部分
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
    Keyword(String),
    List(Vector<Value>),
    Vector(Vector<Value>),
    Map(HashMap<Value, Value>),

    // Sexp 没有的——可调用类型（运行时才有）
    Function(Arc<Function>),         // 用户定义的函数
    NativeFunction(NativeFn),        // Rust 实现的函数
    Macro(Macro),                    // 宏

    // ZOS 运行时对象（面向对象系统）
    Object(Box<dyn ZosObject>),      // 类的实例、类本身……
}
```

Value 和 Sexp 的**关键区别**：

| | Sexp（语法树） | Value（运行时值） |
|---|---|---|
| 包含什么 | 纯数据 | 数据 + 函数 + 宏 + 对象 |
| 谁来创建 | Reader（解析器） | Eval（求值器） |
| 谁在使用 | 宏（变换代码） | 程序运行时 |
| 例子中的 `+` | 只是一个符号 `"+"` | 实际的加法函数 |

## 从代码到值的旅程

让我们追踪 `(+ 1 2)` 的完整生命周期：

```
阶段1: Reader 解析
  "(+ 1 2)"  →  Sexp::List([Symbol("+"), Integer(1), Integer(2)])

阶段2: Eval 开始 — 看到 List，先 eval 第一个元素
  Symbol("+") → 在环境中查找 → Value::NativeFunction(add)

阶段3: Eval 每个参数
  Integer(1) → 自求值 → Value::Integer(1)
  Integer(2) → 自求值 → Value::Integer(2)

阶段4: Apply（调用函数）
  add(1, 2) → Value::Integer(3)
```

## 为什么要有两个分离的类型？

**因为宏需要操作代码（未求值的）**。

看这个宏：

```lisp
(defmacro unless [test body]
  (list 'if test nil body))
```

当宏被调用时，`test` 和 `body` 是 **Sexp**——代码的结构，还没求值。宏可以检查、变换、重组这些代码数据结构，然后返回一个新的 Sexp，再交给 eval 求值。

如果只有 Value 类型，你就没法区分"还没执行的代码"和"已经执行完的值"。

## 两个方向的转换

Zio 提供了在两个世界间转换的函数：

### Sexp → Value

```rust
// 简单可靠——因为 Sexp 没有 Value 特有的类型
impl From<Sexp> for Value { ... }
```

### Value → Sexp

```rust
// 可能失败——如果 Value 包含 Function/Macro 就没法转回 Sexp
pub fn value_to_sexp(value: &Value) -> Result<Sexp, EvalError> { ... }
```

## 可视化：宏的数据往返

```
宏定义:
  (defmacro unless [test body]
    (list 'if test nil body))

宏调用:
  (unless false 42)
      │
      ▼
  (list 'if false nil 42)    ← 宏在 Sexp 域操作
      │                         list 创建新的 Sexp 列表
      ▼
  (if false nil 42)          ← 展开结果被重新求值
      │
      ▼
  nil                           ← 最终值
```

## 动手实验

在 REPL 中：

```lisp
;; 用 quote 阻止求值，看到代码的结构
'(+ 1 2)        ;; → (+ 1 2)             不会计算，保持为列表
(list '+ 1 2)   ;; → (+ 1 2)             手动构建同样的结构
(eval '(+ 1 2)) ;; → 3                   手动求值

;; 宏展开
(defmacro unless [test body]
  (list 'if test nil body))

(macroexpand '(unless false 42))  ;; → (if false nil 42)
```

## 对应源码

- `core/src/sexp.rs` —— Sexp 枚举定义（~150 行）
- `core/src/value.rs` —— Value 枚举定义（~300 行）
- `core/src/macros.rs` —— value_to_sexp 转换函数

## 核心记忆

> **Sexp 是代码域，Value 是运行时域**。宏在 Sexp 域工作，eval 是两者的桥梁。
