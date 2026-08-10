# Eval 求值与编译管线

> Version 0.4 — eval/apply 循环、TCO、ZOS 集成、编译器未来规划
>
> 当前可执行路径是 AST evaluator；ZOS class/generic dispatch 只有
> experimental subset。ZIR、bytecode VM 和 JIT 均为
> [Planned](feature-matrix.md)。仓库数量见[项目状态](status.md)。

---

## 1 求值管线

### 1.1 当前管线

```
Source text
    │
    ▼
┌─────────────────────┐
│  Reader: tokenize   │  → Vec<Token>
│  Reader: parse      │  → Sexp（带 span，Phase 1 后）
└─────────────────────┘
    │
    ▼
┌─────────────────────┐
│  eval::eval_inner() │  → 进入 eval 循环
│    ├─ Self-eval     │  Nil/Bool/Int/Float/String/Keyword → Value
│    ├─ Symbol lookup │  env.get(name) → Value
│    ├─ Special form  │  eval_special_form → dispatch
│    ├─ Macro expand  │  try_expand_by_name → re-eval
│    ├─ Eval args     │  eval_inner on each arg
│    └─ apply()       │  match func type → bind env → eval body
│                     │    or experimental ZOS subset dispatch
└─────────────────────┘
    │
    ▼
Value / TailResult
```

### 1.2 Experimental ZOS 子集求值流程

```
Sexp::List([Symbol("draw"), Symbol("rect")])
    │
    ▼
eval_inner: Symbol("draw") → 环境查找
    │
    ├─ Value::Function(f) → apply(f) → 绑定环境 → eval 函数体
    ├─ Value::Macro(m)    → 宏展开 → re-eval
    ├─ Value::Object(gf)  → ZOS GF dispatch
    │     ├─ class_of(rect) → <Class RECTANGLE>
    │     ├─ find_methods([RECTANGLE]) → [Method draw-(rectangle)]
    │     ├─ method_combination(:before, :primary, :after)
    │     └─ apply_method → eval primary method body
    └─ Value::NativeFunction(nf) → 直接 Rust 调用
```

### 1.3 Planned VM/JIT 管线

```
Source text → Reader → Sexp
                           │
                    ┌──────┴──────┐
                    ▼              ▼
            ┌────────────┐  ┌──────────────┐
            │ eval (AST)  │  │ ZIR Compiler  │
            │ 当前管线     │  │ Sexp → ZIR IR │
            │             │  │ → opt → codegen│
            └──────┬──────┘  └──────┬──────────┘
                   │                │
                   ▼                ▼
             Value / Object      machine code
```

ZIR、bytecode VM 与 JIT 尚未实现；其批准设计和状态见
[特性矩阵](feature-matrix.md)。当前 AST 解释器及 experimental ZOS 子集
用于验证语言语义。

---

## 2 Eval 引擎详解

### 2.1 eval_inner 核心逻辑

`eval_inner: (expr, env, tail, engine) → TailResult`

```rust
fn eval_inner(expr, env, tail, engine) {
    match expr {
        // 自求值类型
        Nil | Boolean | Integer | Float | String | Keyword → Value
        
        // 符号查找
        Symbol(s) → env.get(s) 或 error
        
        // 空列表 → nil
        List([]) → nil
        
        // 复合表达式
        List([first, args...]) → 
            if first 是特殊形式 → 特殊形式处理
            else eval(first) → func_val
            if func_val 是 Macro → 展开并 re-eval
            else eval 每个参数 → evaled_args
            apply(func_val, evaled_args, engine)
        
        // 向量/Map → 递归 eval 每个元素
        Vector / Map → eval 子元素
    }
}
```

### 2.2 apply 核心逻辑

```rust
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn EvalEngine) -> TailResult {
    match func {
        Function(f)        → bind env → eval body (tail = true)
        NativeFunction(nf) → nf.call(args, engine)
        Macro(m)           → expand → eval_inner(expanded, env, tail, engine)
        Object(gf) if GF   → gf.dispatch(args, engine)  // ZOS 多分派
        _                  → error: not a function
    }
}
```

### 2.3 TCO 策略

当前 evaluator 有两条不同的无栈增长路径：

- 普通函数调用出现在尾位置时，`eval_inner` 返回
  `TailResult::TailCall(Value, Vector<Value>)`。`eval()` trampoline（以及非尾调用
  内部的 trampoline）反复调用 `apply`，因此普通自递归和 `even?`/`odd?`
  这类互递归都不会为每次尾调用增加 Rust 栈帧。函数体以及 `if`、`cond`、
  `do`、`let`、`let*` 等控制形式会传播尾位标记。
- `(loop ...)`/`recur` 使用独立的 `TailResult::Recur(Vector<Value>)`。
  `loop` 消费参数向量、重新绑定 loop 局部变量并继续；`Recur` 不是通用函数
  尾调用，也不携带环境。

因此 `TailResult` 当前包含 `Value(Value)`、`Recur(Vector<Value>)` 和
`TailCall(Value, Vector<Value>)` 三个变体；通用函数尾调用不是未来规划。

---

## 3 ZOS 架构集成

### 3.1 ZOS 在核心中的位置

ZOS 是 `zio-core` crate 的一部分，不是独立 crate：

```
zio-core/src/
├── lib.rs            — 模块入口
├── value.rs          — Value 枚举 + Value::Object
├── sexp.rs           — Sexp 语法树
├── env.rs            — 词法环境
├── eval.rs           — eval/apply 循环
├── context.rs        — EvalRuntime trait + EvalContext
├── builtins.rs       — native bindings（数量见 status.md）
├── macros.rs         — 宏展开引擎
├── module.rs         — 模块系统
├── error.rs          — 错误类型
├── span.rs           — 源码位置
├── symbol.rs         — Symbol 结构
└── special/          — 特殊形式分发
    ├── mod.rs
    ├── bindings.rs   — def, defun, defmacro, fn
    ├── control.rs    — if, do, and, or, cond
    ├── letloop.rs    — let, let*, loop, recur
    ├── data.rs       — quote
    └── module_forms.rs — module, require
```

当前 experimental ZOS subset：

```
zio-core/src/zos/
├── mod.rs            — ZOS 模块入口
├── object.rs         — ObjectHeader + ZosObject trait
├── class.rs          — Class 定义 + 注册表 + defclass
├── package.rs        — Package 符号管理
└── gf.rs             — GenericFunction 与 Method 子集
```

完整 MOP、Condition System 等更广的 ZOS 规范不是当前稳定实现；状态以
[特性矩阵](feature-matrix.md)为准。

### 3.2 EvalEngine 拆分

这是 `core/src/context.rs` 中已经存在的当前 API。求值与模块注册分别由两个
能力 trait 表达，`EvalEngine` 是组合二者的 marker supertrait：

```rust
/// 最小编译/求值能力。不含模块和 ZOS。
pub trait EvalRuntime {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
}

/// 模块注册表。独立于求值。
pub trait ModuleRegistry {
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(&self, name: &[String], source: &str, parent_env: &Arc<Env>)
        -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
}

pub trait EvalEngine: EvalRuntime + ModuleRegistry {}
```

`EvalContext` 持有根环境、`ModuleTable` 和可选 loader，并分别实现
`EvalRuntime` 与 `ModuleRegistry`，最后实现 `EvalEngine`。只需要最小求值能力
的嵌入边界可以依赖 `EvalRuntime`；当前完整 AST evaluator 和 CLI 路径使用
`EvalEngine`。单独的 `ZosRuntime`/VM 能力边界不在当前 `context.rs` API 中；
ZIR、bytecode VM 与 JIT 仍是下节所述的规划范围。

---

## 4 编译管线（Planned）

### 4.1 ZIR 设计要点

ZIR 分以下阶段引入：

| Phase | 状态 | 管线 |
|-------|------|------|
| 1-4 | ✅ 纯 AST 解释器 | Reader → Sexp → Eval → Value |
| 5 | 📋 评估阶段 | 分析解释器热点，验证 ZIR 可行性 |
| 6 | 📋 字节码编译器（如验证通过） | 单通道、显式尾调用指令、Span 嵌入 |
| 7 | 📋 Cranelift JIT | 编译热函数到机器码 |

### 4.2 编译器层可选性

`compiler` 和 `vm` 层始终是**可选的**。所有语言特性（包括 ZOS）都在 AST 解释器层定义。编译器层只是渐进加速器，不是硬依赖。

```
基础模式（Phase 1-4）：Reader → AST eval
加速模式（Phase 6+）： Reader → ZIR → bytecode → eval 或 JIT
```

---

## 5 相关文档

| 文档 | 内容 |
|------|------|
| [zio-architecture.md](zio-architecture.md) | 系统架构总览、分层、当前状态 |
| [feature-matrix.md](feature-matrix.md) | 权威 Stable / Experimental / Planned 状态 |
| [status.md](status.md) | 自动生成的仓库事实 |
| [zos-spec.md](zos-spec.md) | ZOS 完整规范 |
| [zio-philosophy.md](zio-philosophy.md) | 语言哲学和设计定理 |
| [adrs.md](adrs.md) | 架构决策记录（特别是 ADR-005, 006, 008） |
