# 13 - REPL 与 CLI 实现

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

打开 `cli/src/main.rs`。整个 REPL 实现不到 200 行。

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

```rust
fn make_ctx(sm: &Arc<SourceMap>) -> EvalContext {
    let env = make_root_env();
    let loader = make_require_loader(sm);
    EvalContext::with_loader(env, loader)
}

fn make_root_env() -> Arc<Env> {
    let env = Arc::new(Env::new(None));  // 根环境
    builtins::setup_env(&env);            // 安装 30 个内置函数
    setup_special_forms(&env);            // 也可以设置特殊形式引用
    env
}
```

根环境创建时为空（`outer: None`），然后通过 `setup_env` 注入所有内置函数。

## 脚本运行

```rust
fn run_script(path: &str, sm: &Arc<SourceMap>) -> Result<Value, EvalError> {
    let ctx = make_ctx(sm);
    load_stdlib(&ctx, sm);        // 加载标准库

    let source = read_source_file(&PathBuf::from(path))?;
    let mut last_val = Value::Nil;

    // 支持文件中有多个表达式，逐个求值
    for expr in parse_all(&source) {
        last_val = eval::eval(&expr, &ctx)?;
    }

    Ok(last_val)
}
```

## 标准库加载

```rust
fn load_stdlib(ctx: &EvalContext, sm: &Arc<SourceMap>) {
    // 从嵌入式标准库加载 core.zio
    let stdlib = zio_core::stdlib_source();  // include_str!("../stdlib/zio/core.zio")
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
1. cargo run
   ↓
2. main() 检测参数：< 2 个 → REPL 模式
   ↓
3. make_root_env()
   ├── 创建空环境 (outer: None)
   ├── builtins::setup_env(&env)   → 安装 + - * / 等 30 个函数
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
cargo run

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
cargo run -- hello.zio

# 4. 错误恢复
zio> (/ 1 0)
Error: division by zero
zio> (println "Still alive!")
Still alive!
nil
```

## 对应源码

- `cli/src/main.rs` —— REPL + 脚本运行器（~170 行）
- `core/src/context.rs` —— EvalContext 定义
- `core/src/builtins.rs:setup_env` —— 内置函数注册
- `core/src/lib.rs` —— stdlib_source() 嵌入标准库

## 核心记忆

> **REPL = Read → Eval → Print → Loop。** 错误不杀死 REPL。一个 eval 引擎可以在不同模式间复用（REPL、脚本、嵌入）。
