# Eval 求值与编译管线

> Version 0.4 — eval/apply 循环、TCO、ZOS 集成、编译器未来规划

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
│                     │    or ZOS GF dispatch → method combination
└─────────────────────┘
    │
    ▼
Value / TailResult
```

### 1.2 ZOS 集成后的求值流程

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

### 1.3 未来管线（Phase 5+, 加入 JIT）

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

ZIR 的引入是 Phase 5+ 的事。在此之前，AST 解释器 + ZOS 运行时足够验证语言语义。

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
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn ZosRuntime) -> TailResult {
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

所有尾位置（Phase 1 目标）：

| 位置 | 当前 | Phase 1 |
|------|------|---------|
| `loop` body | ✅ `recur` | ✅ 同上 |
| `if` 的 then/else 分支 | ❌ 新建帧 | ✅ 不建帧 |
| `cond` 的最后一个表达式 | ❌ 新建帧 | ✅ 不建帧 |
| `and` / `or` 的最后一个参数 | ❌ 新建帧 | ✅ 不建帧 |
| `let` / `let*` body | ❌ 新建帧 | ✅ 不建帧 |
| `do` 的最后一个表达式 | ❌ 新建帧 | ✅ 不建帧 |
| 函数 `apply` 的尾调用 | ❌ 新建帧 | ✅ 不建帧 |

通过 `TailResult::TailCall(env, body)` 变体实现（当前仅 `Recur`，扩展到通用尾调用）。

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
├── builtins.rs       — 30 个内置函数
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

ZOS Phase 1 新增：

```
zio-core/src/zos/
├── mod.rs            — ZOS 模块入口
├── object.rs         — ObjectHeader + ZosObject trait
├── class.rs          — Class 定义 + 注册表 + defclass
├── slot.rs           — SlotDefinition + slot-value + (setf slot-value)
├── gf.rs             — GenericFunction + dispatch cache
├── method.rs         — Method 定义 + method combination
├── package.rs        — Package 符号管理
├── condition.rs      — Condition System（简化版）
└── reflection.rs     — class-of + type-of + 反射 API
```

### 3.2 EvalEngine 拆分

Phase 1 架构重构：将当前单一的 `EvalEngine` trait 拆分为 3 个子 trait：

```rust
/// 最小编译/求值能力。不含模块和 ZOS。
pub trait EvalRuntime {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
}

/// ZOS 运行时能力。需要 EvalRuntime 作为基础。
pub trait ZosRuntime: EvalRuntime {
    fn class_of(&self, val: &Value) -> ClassRef;
    fn find_and_apply_gf(&self, gf: &GenericFunction, args: &[Value])
        -> Result<TailResult, EvalError>;
    fn signal_condition(&self, c: Condition)
        -> Result<Option<TailResult>, EvalError>;
    fn invoke_restart(&self, name: &Symbol, args: &[Value])
        -> Result<TailResult, EvalError>;
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
```

**嵌入场景**：只需实现 `EvalRuntime`，不需要 ZOS 和模块。CLI 场景使用完整的 `EvalContext` 实现所有三个 trait。

---

## 4 编译管线（未来）

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
| [zos-spec.md](zos-spec.md) | ZOS 完整规范 |
| [zio-philosophy.md](zio-philosophy.md) | 语言哲学和设计定理 |
| [adrs.md](adrs.md) | 架构决策记录（特别是 ADR-005, 006, 008） |
