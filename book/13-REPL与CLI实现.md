# 13 - REPL 与 CLI 实现

> 2026-10-07 目录更新：本文教学代码与旧阶段/验收描述保留历史语境；源码链接和运行路径已映射到当前位置。现行目标是 `libs/` 的 Zio 库与 `apps/grove/` 的 Zio 业务迁移，Rust Grove 业务尚未整体重写；通用 Rust transport/host adapter 在 `contribs/native/loom/`。当前归属见[架构](../docs/zio-architecture.md)与[批准目录计划](../docs/superpowers/plans/2026-10-07-language-first-layout.md)。

## REPL 是什么？

REPL = **R**ead **E**val **P**rint **L**oop。

这是 Lisp 的核心交互界面——一种"对话式编程"：

```
你输入: (+ 1 2)
Zio 回应: 3

你输入: (defn square [x] (* x x))
Zio 回应: #<function (x)>

你输入: (square 5)
Zio 回应: 25
```

## CLI 入口

打开 `langs/cli/src/main.rs`。本章保留简化的 REPL 教学实现，不是当前入口的完整清单。
`zio-cli` 是普通语言二进制，不提供 `--llm-replay`；模型与 replay 合同在
`contribs/native/loom/`，由授权宿主或应用装配。Rill 是未来 Zio CLI 组合库方向，
不是独立 Rust 业务库目标。目录归属见[架构](../docs/zio-architecture.md)。

### Main 函数

```rust
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sm = Arc::new(SourceMap::new());

    if args.len() > 1 {
        // 运行脚本：zio program.zio
        run_script(&args[1], &sm).unwrap();
    } else {
        // 交互模式：zio
        load_stdlib(&ctx, &sm);     // 加载标准库
        run_repl(&sm);              // 启动 REPL
    }
}
```

### REPL 循环

```rust
fn run_repl(sm: &Arc<SourceMap>) {
    let ctx = make_ctx(sm);         // 创建求值上下文
    let stdin = io::stdin();

    println!("Zio REPL");
    println!("Press Ctrl+D or type (exit) to quit");

    loop {
        print!("zio> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if stdin.read_line(&mut input).is_err() || input.trim() == "(exit)" {
            break;  // Ctrl+D 或 (exit) 退出
        }

        let input = input.trim();
        if input.is_empty() { continue; }

        // 主要的 Read-Eval-Print 循环
        match reader::read(input) {              // Read
            Ok(sexp) => {
                match eval::eval(&sexp, &ctx) {  // Eval
                    Ok(val) => println!("{val}"),  // Print
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
            Err(e) => eprintln!("Parse error: {e}"),
        }
    }  // Loop
}
```

看清楚了吗？就三步：`read → eval → print`，然后循环。

整个过程可视化：

```
┌──────┐    ┌──────┐    ┌───────┐    ┌───────┐
│ 你   │───→│ READ │───→│ EVAL  │───→│ PRINT │───→ 你看到
│ 输入  │    │ parse│    │ eval  │    │ 显示  │    │ 结果
│"+ 1 2"│    │→Sexp│    │→Value│    │"3"    │
└──────┘    └──────┘    └───────┘    └───────┘
                ↑            ↑
           reader::read  eval::eval
```

### 上下文创建

装配不在 CLI 里，也不在 core 的 eval 里：`langs/core/src/bootstrap.rs` 是唯一的装配入口，
CLI、编译器前端和 Grove 的候选执行都调用同一个 `language_context`。

```rust
let ctx = zio_core::bootstrap::language_context(
    zio_core::bootstrap::ModuleRoots::new(roots)?,  // 显式授予的模块根
)?;                                                 // 标准库失败即返回 Err
```

它做三件事：安装内置函数（`builtins::setup_env`）、按授予根安装 `require` 加载器、
加载 `core.zio` 标准库。标准库是失败即停的 —— 半加载的 stdlib 会让之后的每个符号解析
都变成猜测，所以解析或求值出错直接返回错误，不降级为警告。

根环境创建时为空（`outer: None`），然后通过 `setup_env` 注入所有内置函数。

## 脚本运行

```rust
fn run_script(path: &str) -> Result<Value, EvalError> {
    let ctx = language_context(script_roots(path))?;
    let source = std::fs::read_to_string(path)?;
    // 源文件按路径名注册进 ctx 的 SourceMap，错误定位直接指向脚本行号
    eval_source(&ctx, path, &source)
}
```

`eval_source` 是"运行一段 Zio 文本"的唯一入口：它注册来源、逐个求值顶层 form、
返回最后一个值。

## 标准库加载

```rust
fn load_stdlib(ctx: &EvalContext, sm: &Arc<SourceMap>) {
    // 从嵌入式标准库加载 core.zio
    let stdlib = zio_core::stdlib_source();  // 标准库源位于 libs/std/core.zio
    let exprs = parse_all(stdlib);
    for expr in exprs {
        eval::eval(&expr, ctx).unwrap();
    }
}
```

标准库是 `.zio` 文件，在编译时通过 `include_str!` 嵌入到二进制中。这样 REPL 启动时不需要找外部文件。

## 多行输入支持

REPL 的一个实用功能：当括号没闭合时，等待更多输入。

```lisp
zio> (+ 1

  | 2
  | 3)
6
```

这可以通过检查括号平衡来实现——如果解析失败（未闭合括号），继续读取输入。

## SourceMap：追踪源码位置

```rust
fn run_repl(sm: &Arc<SourceMap>) {
    let input = ...;
    let source_id = sm.add_source("REPL", &input); // 注册输入片段
    match reader::read_with_source(input, source_id) {
        // 现在错误可以显示 REPL 中的位置
    }
}
```

SourceMap 给每个输入片段分配一个 ID，这样错误消息可以指向"第 3 次输入的第五行"。

## 错误处理

REPL 的一个关键设计：**错误不退出 REPL**。

```lisp
zio> (/ 1 0)
Error: division by zero
zio> (+ 1 2)     ← REPL 仍然活着
3
```

这是通过 `match` 实现的——错误被捕获并显示，然后继续循环。只有 `(exit)` 或 Ctrl+D 才会真正退出。

## 完整的执行流程

从启动到退出：

```
1. cargo run -p zio-cli
   ↓
2. main() 检测参数：< 2 个 → REPL 模式
   ↓
3. make_root_env()
   ├── 创建空环境 (outer: None)
   ├── builtins::setup_env(&env)   → 安装 + - * / 等全部内置函数
   └── 创建 EvalContext
   ↓
4. load_stdlib(&ctx)
   └── 解析并求值嵌入式 core.zio
   ↓
5. run_repl(&sm)
   └── 无限循环:
       ├── 打印 "zio> "
       ├── 读取一行输入
       ├── reader::read(input)   → Sexp
       ├── eval::eval(sexp, ctx)  → Value
       ├── println!("{value}")
       └── 回到循环，除非 Ctrl+D
```

## 动手实验

```bash
# 1. 启动 REPL
cargo run -p zio-cli

# 2. 在 REPL 中
zio> (+ 1 2 3)
6
zio> (defn fib [n]
  |   (if (< n 2) n
  |     (+ (fib (- n 1)) (fib (- n 2)))))
#<function (n)>
zio> (fib 10)
55

# 3. 运行脚本
echo '(println "Hello from script!")' > hello.zio
cargo run -p zio-cli -- hello.zio

# 4. 错误恢复
zio> (/ 1 0)
Error: division by zero
zio> (println "Still alive!")
Still alive!
nil
```

## 对应源码
- `langs/cli/src/main.rs` —— REPL + 脚本运行器（~170 行）
- `langs/core/src/context.rs` —— EvalContext 定义
- `langs/core/src/builtins/mod.rs:setup_env` —— 内置函数聚合注册
- `langs/core/src/lib.rs` —— stdlib_source() 嵌入标准库
- `langs/core/src/lib.rs` —— stdlib_source() 嵌入标准库

## 核心记忆

> **REPL = Read → Eval → Print → Loop。** 错误不杀死 REPL。一个 eval 引擎可以在不同模式间复用（REPL、脚本、嵌入）。
