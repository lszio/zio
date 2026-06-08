# Zio

> A Modern Lisp for the Agent Era.

Zio 是一门面向未来的通用 Lisp 语言，用 Rust 实现。当前处于 **v0.2 架构重组阶段**——从一个 REPL toy 演进为生产级语言。

## 当前状态

| 组件 | 进度 | 说明 |
|------|------|------|
| Reader | ✅ | 基于栈的 S-expression 解析 |
| Eval | ✅ | AST 解释器，词法作用域，闭包 |
| Special Forms | ✅ | quote/def/if/do/fn/let/loop/recur/defmacro/and/or/cond |
| Macros | ✅ | defmacro 可用，无 hygiene |
| Builtins | ⚠️ | 仅整数算术，基础 list/seq 操作 |
| 模块系统 | 🚧 | Phase 2 目标 |
| 标准库 | ❌ | Phase 3 目标 |
| Compiler/VM | ❌ | Phase 5 目标 |

**62 测试通过 · ~2,700 LOC Rust**

## 快速开始

```bash
cd zio
cargo run
```

```
Zio REPL
Press Ctrl+D or type (exit) to quit
zio> (+ 1 2 3)
6
zio> (defn fib [n] (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2)))))
#<function (n)>
zio> (fib 10)
55
zio> (defmacro unless [test body] (list 'if test nil body))
#<macro unless (test body)>
zio> (unless false 42)
42
```

## 架构

详细架构设计见 [docs/architecture-v0.2.md](docs/architecture-v0.2.md)。

```
                    ┌─────────────┐
                    │  CLI (zio)   │
                    └──────┬──────┘
          ┌────────────────┼────────────────┐
          ▼                ▼                ▼
    ┌──────────┐    ┌──────────┐    ┌──────────────┐
    │  reader   │    │ compiler  │    │  std (stdlib) │
    │  (parse)  │    │  (ZIR)   │    │  (Rust+Zio)  │
    └────┬─────┘    └────┬─────┘    └──────┬───────┘
         └───────────────┼─────────────────┘
                         ▼
                  ┌──────────┐
                  │ core (vm) │
                  │ Value·Env·│
                  │ GC·Module │
                  └──────────┘
```

## 路线图

| 阶段 | 时间 | 交付物 |
|------|------|--------|
| **Phase 1** — 架构重组 | 当前 | workspace 拆分、Span、special.rs 重构、浮点数 |
| **Phase 2** — 模块系统 | 2 周后 | `zio run`、`load`/`module`/`require`、多文件 |
| **Phase 3** — 标准库 | 4 周后 | math/string/seq/file/io/test 标准库 |
| **Phase 4** — 语言特性 | 6 周后 | 模式匹配、协议、多分派、条件系统 |
| **Phase 5** — VM | 10 周后 | ZIR 中间表示、字节码解释器 |
| **Phase 6** — 并发 | 16 周后 | Actor、通道、FFI |
| **Phase 7** — 生产化 | 24 周后 | LSP、调试器、Native 编译、WASM |

## 设计原则

- **渐进式**：简单的脚本用 `(load)` 即可，复杂项目用 `(module)` + 类型注解
- **嵌入友好**：core crate 最小依赖，可从 Rust 端直接调用
- **Agent First**：程序结构可被运行时感知和查询
- **同像性 (Homoiconic)**：代码即数据

## License

MIT