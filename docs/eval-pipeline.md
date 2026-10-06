# Eval 求值与编译管线

> Version 0.5 — 当前 AST 路径、保留 ZOS 与 Zio 工具链自举（2026-10-05）
>
> AST evaluator 为当前执行路径；ZOS class/generic dispatch 为
> Experimental 子集。完整 Zio 展开/分析/编译器、最小后端和 JIT 为
> Planned，分别验收。数量见[项目状态](status.md)，状态见[特性矩阵](feature-matrix.md)。

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

### 1.4 生成代码与真实 agent 逻辑

`eval` 只执行程序，不等于模型/工具会话循环。Grove 的计划主线复用本执行器，
由 Zio 程序实际控制任务步骤，再由 Loom 装配模型、工具、会话、预算、
取消、provider 与 ACP；生成代码的解析、宏/依赖传递权限检查与隔离执行
是另一明确边界。Loom 不持 Grove 的学习目标、独立评价或发布治理。
当前 `macroexpand` 只展开外层形式，不是完整编译前端；Sexp 转 Value 再返回
会丢失原来源，不能用打印后的 AST 声称已经保留可审计的执行位置。
计划增加可关闭的执行观测：开启时关联程序版本、源码/表达式、分支与能力
调用；关闭时不构造轨迹对象，不把学习策略或模型协议放入核心。

Zio 库目标采用 Zio 实现并归入 `libs/`：Numa 持计算，Rill 持 CLI 组合，Loom 持 harness 组合。现有 Rust 通用传输/宿主合同位于 `contribs/native/loom/`，不是 Zio 库本体；普通语言 CLI 位于 `langs/cli/`，不装配 LLM replay。Grove 是 `apps/grove/` 独立应用，其 Rust 业务仍待后续迁为 Zio；语言介绍、文档与 playground 位于 `apps/site/`。本轮只完成结构切换，不证明完整库交付、Tree-sitter、LSP 或编译器自举。当前合同见[架构](zio-architecture.md)与[批准目录计划](superpowers/plans/2026-10-07-language-first-layout.md)。短名不等于已注册包 ID。

---

## 2 Eval 循环

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

langs/core/src/
langs/core/src/
├── lib.rs            — 模块入口
├── value.rs          — Value 枚举 + Value::Object
├── sexp.rs           — Sexp 语法树
├── env.rs            — 词法环境
├── eval.rs           — eval/apply 循环
├── context.rs        — EvalRuntime trait + EvalContext
├── builtins/         — native bindings，按域分模块（数量见 status.md）
│   ├── mod.rs        — setup_env 聚合注册
│   ├── numeric.rs    · predicates.rs · collections.rs · strings.rs
│   ├── zos_access.rs · io.rs · buffer.rs · concurrency.rs
│   └── json.rs       · macroexpand.rs
├── macros.rs         — 宏展开引擎
├── module.rs         — 模块系统
├── error.rs          — 错误类型
├── span.rs           — 源码位置
├── io.rs             — IoHost 抽象（ADR-011）
└── special/          — 特殊形式分发
    ├── mod.rs
    ├── bindings.rs   — def, defn, defmacro, fn
    ├── control.rs    — if, do, and, or, cond
    ├── letloop.rs    — let, let*, loop, recur
    ├── data.rs       — quote
    ├── module_forms.rs — module, require, export
    └── zos_forms.rs  — defclass, defgeneric, defmethod, defpackage, try
```

当前 experimental ZOS subset：

langs/core/src/zos/
langs/core/src/zos/
├── mod.rs            — ZOS 模块入口
├── object.rs         — ObjectHeader + ZosObject trait
├── class.rs          — Class 定义 + 注册表 + defclass
├── package.rs        — Package 符号管理
├── gf.rs             — GenericFunction、Method 与 GFObject
├── apply.rs          — ZOS 可调用协议（唯一的 GF downcast 点，ADR-013）
├── mop.rs            — metaclass registry
└── reflection.rs     — 反射引擎
```

完整 MOP、Condition System 等更广的 ZOS 规范不是当前稳定实现；状态以
[特性矩阵](feature-matrix.md)为准。

### 3.2 EvalEngine 拆分
这是 `langs/core/src/context.rs` 中已经存在的当前 API。求值与模块注册分别由两个
这是 `langs/core/src/context.rs` 中已经存在的当前 API。求值与模块注册分别由两个
能力 trait 表达，`EvalEngine` 是组合二者的 marker supertrait：

```rust
/// 最小求值、源码定位与宿主 I/O 能力。不含模块注册和 ZOS。
pub trait EvalRuntime {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
    fn source_map(&self) -> &Arc<crate::span::SourceMap>;
    fn io(&self) -> &dyn crate::io::IoHost {
        &DEFAULT_STD_IO
    }
}

static DEFAULT_STD_IO: crate::io::StdIoHost = crate::io::StdIoHost;

/// 模块注册表。独立于求值。
pub trait ModuleRegistry {
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(&self, name: &[String], source: &str, parent_env: &Arc<Env>)
        -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
    fn push_module_exports(&self);
    fn add_module_export(&self, name: String);
    fn take_module_exports(&self) -> Vec<String>;
}

pub trait EvalEngine: EvalRuntime + ModuleRegistry {}
```

`EvalContext` 持有根环境、`ModuleTable`、可选 loader、源码注册表、宿主 I/O
与模块导出栈，并分别实现
`EvalRuntime` 与 `ModuleRegistry`，最后实现 `EvalEngine`。只需要最小求值能力
的嵌入边界可以依赖 `EvalRuntime`；当前完整 AST evaluator 和 CLI 路径使用
`EvalEngine`。单独的 `ZosRuntime`/VM 能力边界不在当前 `context.rs` API 中；
ZIR、bytecode VM 与 JIT 仍是下节所述的规划范围。

---

## 4 编译管线（Planned）

### 4.1 自举交付顺序

| 步骤 | 状态 | 管线与验收 |
|------|------|------------|
| 当前语义参考 | Stable / Experimental ZOS | Reader → Sexp → AST eval；固定首轮语言与 ZOS 合同 |
| Zio 前端 | Planned | Zio 完整展开 → 分析；递归展开、词法绑定与来源保留分别验证 |
| 最小编译器与后端 | Planned | Zio 编译器生成版本化字节码；Rust 最小后端复用核心值、宿主和 ZOS 分派 |
| 工具链自举 | Planned | 引导编译器 → 自编译 → 重复构建；规范化产物与 AST/编译执行行为对照 |
| 后续性能路线 | Planned | 热点测量后再增加优化 ZIR/JIT，不作为 Grove G1 或第一轮自举的前置任务 |

### 4.2 引导执行与编译器的区别

AST evaluator 保留为引导环境和语义对照，普通解释执行不依赖新编译器。
编译器不是语言语义的替代品，但**完成工具链自举必须实际有 Zio 编译器
并编译自身**；不能以编辑器、`eval` 或宏 demo 代替。ZOS 语义仍属于核心，
编译后路径通过同一对象分派入口，不能把 ZOS 拆成可选领域插件。

最小后端的数据格式与完整指令合同在实施计划 T02 固定，不能在多个计划
分别建立 ZIR/VM。现有完整 VM 性能设计是后续参考，开工顺序以
[统一实现计划](superpowers/plans/2026-10-05-zio-grove-convergence.md)为准。

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
| [superpowers/plans/2026-10-05-zio-grove-convergence.md](superpowers/plans/2026-10-05-zio-grove-convergence.md) | 当前实现依赖与真实验收；工具链独立于 Grove 主线 |
