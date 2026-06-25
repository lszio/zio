# 03: 显式状态：EvalEngine Trait

> 从 4 个 `thread_local!` 全局变量到 trait object 注入——一次有洁癖的架构重构。

---

## 问题

`eval` 函数需要状态。很多状态：

- **环境**（`Env`）：变量绑定在哪里？
- **模块注册表**（`ModuleRegistry`）：哪些模块被加载了？
- **加载器**（`ModuleLoader`）：怎么从源代码加载模块？
- **loading 栈**：循环依赖检测

在 2024 年秋天，Zio 的代码里是这样管理这些状态的：

```rust
thread_local! {
    static MODULES: RefCell<ModuleRegistry> = ...;
    static LOADING_STACK: RefCell<Vec<PathBuf>> = ...;
    static REQUIRE_LOADER: RefCell<Option<RequireLoader>> = ...;
}
```

没问题？有问题。

## thread_local 的四宗罪

### 1. 不可测试

```rust
#[test]
fn test_module() {
    // 副作用！这个测试可能影响其他测试
    eval("(module :m (def x 1))");
    // 要隔离就必须 reset，但 thread_local 是进程级状态
}
```

每个测试共享全局状态。并行测试永远不可预测。你永远不知道哪个测试先改变了 `MODULES`。

### 2. 不可嵌入

如果有人想在自己的 Rust 应用里**嵌入** Zio：

```rust
// 我需要两个独立的解释器实例
thread_local! { /* 只能有一组全局变量 */ }
```

两个解释器共享同一个全局模块注册表？灾难。

### 3. 不可扩展

特殊形式（`if`、`do`、`let`）需要访问 `eval` 能力。但在 thread_local 时代，特殊形式要调用 eval 只能通过：

```rust
// 之前：动态分发函数指针，带生命周期问题
type EvalFn = fn(&Sexp, &Env, bool, &dyn Fn(...)) -> ...;
```

生命周期标注满天飞，实现处处是 unsafe 的味道。

### 4. 隐藏依赖

一个函数签名看不出它是否需要全局状态。`eval("(module :m ...)")` 看起来纯函数，实际偷偷修改了全局 `MODULES`。

## 解决方案：EvalContext + EvalEngine Trait

打开 [`core/src/context.rs`](../core/src/context.rs)：

```rust
pub trait EvalEngine {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool)
        -> Result<TailResult, EvalError>;
    fn env(&self) -> &Arc<Env>;
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(&self, ...) -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
}
```

以及实现它的具体结构：

```rust
pub struct EvalContext {
    pub env: Arc<Env>,
    pub modules: std::cell::RefCell<ModuleRegistry>,
    pub loader: std::cell::RefCell<Option<Box<ModuleLoader>>>,
}
```

所有可变状态集中在 `EvalContext` 中。整个 eval 链通过 `&dyn EvalEngine` 传递——一个 trait object 引用。

## 设计细节

### 为什么用 trait 而不是直接传 struct？

**因为特殊形式和一些 builtin 函数需要环回（re-entrant）调用 eval**。`map` 需要对每个元素调用 eval；`module` 需要加载新模块。这些调用还需要访问同一组状态。

如果用具体 struct：

```rust
// 循环依赖：eval 需要 EvalContext，map 需要 eval
fn builtin_map(args, ctx: &EvalContext) -> ... {
    ctx.eval(...)  // EvalContext 要有 eval 方法
}
// 然后 EvalContext 依赖 special，special 依赖 EvalContext
// Rust 编译器：RIP
```

Trait 打破了这个循环：

```
eval.rs          → 调用 special::eval_special_form
special/*.rs     → 调用 engine.eval_expr (trait method)
                                           ↑
core/src/lib.rs  ← 汇编：impl EvalEngine for EvalContext
```

Trait 放在一个独立的 `context.rs` 中，不依赖 `special`、`macros` 等模块。所有需要环回调用 eval 的代码只依赖 `EvalEngine` trait，不依赖 `EvalContext` struct。

### RefCell 的选择

`EvalContext` 使用 `RefCell` 而不是 `Mutex`：

```rust
pub modules: std::cell::RefCell<ModuleRegistry>,
pub loader: std::cell::RefCell<Option<Box<ModuleLoader>>>,
```

理由：Zio 当前是单线程的（Rust 的 `!Send` 闭包）。`RefCell` 比 `Mutex` 快得多（无系统调用），且能在 `&self` 方法上提供内部可变性——这对 trait object 设计至关重要。

代价：未来如果需要多线程并发 eval，需要把 `RefCell` 替换为 `Mutex` 。

### NativeFn 的 engine 参数

所有 Rust 实现的 builtin 函数（`map`、`filter`、`reduce`）也接收 `engine` 参数：

```rust
pub struct NativeFn {
    func: Arc<dyn Fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError> + Send + Sync>,
}
```

这使高阶函数可以回调 eval：

```rust
fn builtin_map(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    // args[0] 是函数，args[1] 是列表
    for element in list.iter() {
        // 调用 engine.eval_expr 来 eval 函数体
        result.push(engine.eval_expr(...)?);
    }
}
```

## 重构的效果

重构前：
- 4 个 `thread_local!` 全局变量
- 测试互相影响
- 无法嵌入
- 生命周期标注散布在特殊形式中

重构后：
- 0 个 `thread_local!` 全局变量
- 独立 `EvalContext` 实例可隔离测试
- `EvalEngine` trait 可 mock
- 嵌入者创建自己的 `EvalContext`

```rust
// 嵌入 Zio：
let env = Arc::new(Env::new());
let ctx = EvalContext::new(env.clone());
let result = eval::eval(&sexp, &ctx)?;
// 第二个实例完全独立：
let ctx2 = EvalContext::new(Arc::new(Env::new()));
```

## ADR-002 回顾

```
ADR-002: EvalContext + EvalEngine trait 收容所有 mutable 状态
决定:    所有可变状态属于 EvalContext，通过 &dyn EvalEngine trait 注入
理由:    可测试性、可嵌入性、安全性
代价:    NativeFn 签名多一个 engine: &dyn EvalEngine 参数
```

## 在代码中

- 打开 [`core/src/context.rs`](../core/src/context.rs) 看 `EvalEngine` trait 和 `EvalContext`
- 打开 [`core/src/eval.rs`](../core/src/eval.rs) 看 `impl EvalEngine for EvalContext`
- 打开 [`core/src/builtins.rs`](../core/src/builtins.rs) 搜索 `engine` 看高阶函数如何使用它

```bash
# 确认 0 个 thread_local
grep -r "thread_local" core/src/ reader/src/ cli/src/
# → 无输出
```

---

**下一篇：[04: 持久化数据结构为什么是默认选择](04-persistent-data.md)** — im::Vector 和 im::HashMap 如何让结构共享成为默认行为。