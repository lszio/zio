# 06 - Eval 循环与 Apply：解释器的灵魂

## 核心问题

问：`(+ 1 2)` 是怎么算出 3 的？

答：有一个函数叫 `eval_inner`。它看到一个列表 `(+ 1 2)`，做了三件事：

1. 求值第一个元素 `+` → 找到加法函数
2. 求值其余元素 `1`、`2` → 得到值 1、2
3. 调用（apply）加法函数，传入 1 和 2 → 得到 3

这就是 **eval/apply 循环**。整个 Lisp 解释器就是不断重复这个三步曲。

## eval_inner：核心求值器

打开 `core/src/eval.rs`，找到 `eval_inner` 函数。它的结构非常清晰：

```rust
fn eval_inner(expr: &Sexp, env: &Arc<Env>, tail: bool, engine: &dyn EvalEngine)
    -> Result<TailResult, EvalError>
{
    match expr {
        // 1. 自求值类型：本身就是值，不需要进一步求值
        Sexp::Nil         => Value::Nil,
        Sexp::Boolean(b)  => Value::Boolean(*b),
        Sexp::Integer(n)  => Value::Integer(*n),
        Sexp::Float(f)    => Value::Float(*f),
        Sexp::String(s)   => Value::String(s.clone()),
        Sexp::Keyword(k)  => Value::Keyword(k.clone()),

        // 2. 符号：从环境中查找值
        Sexp::Symbol(name) => env.get(name).ok_or(...),

        // 3. 空列表 → nil
        Sexp::List(vec) if vec.is_empty() => Value::Nil,

        // 4. 非空列表 → 函数/宏/特殊形式调用
        Sexp::List(vec) => {
            let first = &vec[0];
            let args  = &vec[1..];

            // 4a. 检查是否为特殊形式
            if let Sexp::Symbol(name) = first {
                if let Some(result) = eval_special_form(name, args, env, tail, engine)? {
                    return Ok(result);
                }
            }

            // 4b. 求值第一个位置 → 得到函数值
            let func_val = eval_inner(first, env, false, engine)?;

            // 4c. 如果是宏 → 展开后重新求值
            if let Value::Macro(m) = &func_val {
                let expanded = apply_macro(m, args, env, engine)?;
                return eval_inner(&expanded, env, tail, engine);
            }

            // 4d. 宏展开时的特殊处理
            if let Value::Symbol(ref s) = &func_val {
                if let Some(expanded) = try_expand_by_name(s, args, env, engine)? {
                    return eval_inner(&expanded, env, tail, engine);
                }
            }

            // 4e. 求值所有参数
            let evaled_args: Vector<Value> = args.iter()
                .map(|a| eval_inner(a, env, false, engine)?.into_value())
                .collect::<Result<_, _>>()?;

            // 4f. 调用函数
            apply(func_val, evaled_args, engine)
        }

        // 5. 向量/Map → 递归求值每个元素
        Sexp::Vector(vec) => ...
        Sexp::Map(map)    => ...
    }
}
```

这就是 Zio 解释器的全部逻辑。**六个 match 分支**涵盖了所有情况。

## Apply：函数调用

当 eval 完成了参数求值，`apply` 负责实际调用：

```rust
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn EvalEngine)
    -> Result<TailResult, EvalError>
{
    match func {
        // 用户定义的函数：绑定参数到环境，求值函数体
        Value::Function(f) => {
            let new_env = Env::bind(&f.env, &f.params, &args)?;
            eval_inner(&f.body, &new_env, true, engine)
        }

        // Rust 实现的函数：直接调用
        Value::NativeFunction(nf) => {
            let result = (nf.func)(args, engine)?;
            Ok(TailResult::Value(result))
        }

        // 宏（作为值使用时的求值路径）
        Value::Macro(m) => { ... }

        // ZOS Generic Function → 多分派调用
        Value::Object(o) if is_gf(o) => {
            gf_dispatch(o, args, engine)
        }

        // 类型错误
        _ => Err(EvalError::not_callable(func)),
    }
}
```

### 用户函数调用详细流程

当函数 `(defn add [a b] (+ a b))` 被调用时：

```
1. apply 看到 Value::Function(f)
2. 创建新环境: Env::bind(函数定义时的环境, [a b], [实际参数值])
   → 新环境: {a: 1, b: 2}，外层指向函数定义时的环境
3. 在新环境中求值函数体 (+ a b)
   → eval_inner 看到列表 (+ a b)
   → + 是 NativeFunction，直接调用
   → a → 从新环境找到 1
   → b → 从新环境找到 2
   → 返回 3
```

## 公共入口

```rust
/// 最常用的公开接口
pub fn eval(expr: &Sexp, ctx: &dyn EvalEngine) -> Result<Value, EvalError> {
    eval_inner(expr, ctx.env(), false, ctx)?.into_result()
}
```

## 可视化

```
eval_inner 的决策树：

           ┌─ Nil/Boolean/Integer → 自求值
           │
    expr ──┼─ Symbol → env.get(name)
           │
           ├─ List ──┬─ 空列表 → nil
           │         │
           │         └─ 非空 ──┬─ 特殊形式 → 直接处理
           │                   │
           │                   ├─ 宏 → 展开 → 重新求值
           │                   │
           │                   └─ 函数 ── eval first → eval args → apply
           │
           ├─ Vector → eval 每个元素
           │
           └─ Map → eval 每个键和值
```

## TailResult 与尾调用优化

注意 `eval_inner` 返回的是 `TailResult` 而不是 `Value`。这是为了实现**尾调用优化**。

```rust
pub enum TailResult {
    Value(Value),         // 正常求值结果
    Recur(Env),           // recur 指令：回到 loop 重新执行
    TailCall(Arc<Env>, Sexp),  // 尾调用：不建帧直接跳转
}
```

当 `tail = true` 时，处于"尾位置"——调用者不需要做额外操作。此时可以重用当前栈帧：

```
;; 没有 TCO：每个递归调用创建一个新栈帧
(defn fact [n]
  (if (<= n 1) 1
    (* n (fact (- n 1)))))    ;; ← 乘完之后还要返回，不是尾位置

;; 有 TCO：递归调用重用栈帧
(defn fact-tail [n acc]
  (if (<= n 1) acc
    (fact-tail (- n 1) (* n acc))))   ;; ← 直接返回调用结果，尾位置
```

当前 Zio 实现了 `loop/recur` 的 TCO。其他尾位置（if 分支、let body 等）正在实现中。

## 测试中看 eval

打开 `core/src/eval.rs` 底部有很多测试。找一个看看：

```rust
#[test]
fn test_eval_add() {
    let ctx = make_ctx();
    let result = eval_str(&ctx, "(+ 1 2 3)");
    assert_eq!(result, Value::Integer(6));
}
```

测试模式：
1. `make_ctx()` —— 创建求值上下文
2. `eval_str()` —— 解析字符串 → 求值
3. `assert_eq!()` —— 验证结果

## 动手实验

在 REPL 中：

```lisp
;; eval 函数可以手动求值 S 表达式
(eval '(+ 1 2))          ;; → 3    quote 阻止自动求值，eval 手动触发

;; 函数定义的尾位置
(defn always-true [x]
  (if x true false))     ;; true 和 false 在尾位置
```

## 对应源码

| 文件 | 内容 |
|------|------|
| `eval.rs` | eval_inner + apply 完整实现 |
| `special/mod.rs` | TailResult 类型、eval_special_form 分发 |
| `eval.rs:372-1121`（底部）| 大量测试案例 |


## 核心记忆

> **eval 看到了什么就干什么**：符号去查找，列表去调用，其他直接返回。apply 是函数调用的核心：匹配函数类型，分派执行。
